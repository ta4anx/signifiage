//! xdg-desktop-portal screen capture and remote keyboard injection.
//!
//! Every portal call runs on one dedicated Tokio runtime owned by a background
//! thread; long, interactive requests (`Screencast::request`) are executed on a
//! short-lived worker thread so a GTK caller never blocks on a picker dialog.
//! A process-global [`PortalCache`] keeps the PipeWire file descriptor and the
//! portal session alive for as long as a capture source is in use.
//!
//! Nothing here is GTK-aware: the UI polls the [`std::sync::mpsc`] receiver from
//! its 100 ms tick and installs results through [`store`].

use std::collections::HashMap;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::mpsc::{self, Receiver};
use std::sync::{LazyLock, Mutex};

use ashpd::desktop::PersistMode;
use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions};
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};

use ashpd::desktop::{ResponseError, Session};

/// Everything that can go wrong while talking to the desktop portal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PortalError {
    /// No portal frontend / D-Bus session is reachable.
    #[error("The desktop portal is not available: {0}")]
    Unavailable(String),
    /// The user dismissed the portal picker.
    #[error("Screen capture was cancelled")]
    Cancelled,
    /// The portal refused the request.
    #[error("Screen capture was denied: {0}")]
    Denied(String),
    /// The installed portal does not implement screen capture.
    #[error("This desktop does not support screen capture through the portal")]
    Unsupported,
    /// The portal accepted the session but returned no stream.
    #[error("The desktop portal returned no screen-capture stream")]
    NoStream,
    /// No stream has been approved for this capture source yet.
    #[error("Screen capture has not been approved for this source")]
    NotRequested,
    /// The portal worker thread is gone or the runtime died.
    #[error("The desktop portal worker is not running")]
    WorkerStopped,
}

impl PortalError {
    /// Localised, actionable text for a toast or an inline error.
    pub fn message(&self) -> String {
        match self {
            PortalError::Cancelled => rust_i18n::t!("capture.error.cancelled").to_string(),
            PortalError::Denied(_) | PortalError::Unsupported => {
                rust_i18n::t!("capture.error.portal_unavailable").to_string()
            }
            PortalError::Unavailable(_)
            | PortalError::NoStream
            | PortalError::NotRequested
            | PortalError::WorkerStopped => {
                rust_i18n::t!("capture.error.portal_unavailable").to_string()
            }
        }
    }
}

/// A resolved PipeWire stream: the file descriptor stays owned by the cached
/// portal session, so the pipeline may use `fd` for as long as the session lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalStream {
    /// Raw PipeWire remote file descriptor.
    pub fd: i32,
    /// PipeWire node id to capture.
    pub node: u32,
    /// Monitor/window label reported by the portal (localised default).
    pub label: String,
    /// Token that lets a later request preselect the same source.
    pub restore_token: Option<String>,
}

/// Map an ashpd error onto the service error without losing the actionable part.
fn map_error(err: ashpd::Error) -> PortalError {
    use ashpd::Error;
    use ashpd::PortalError as AshpdPortalError;
    match err {
        Error::Response(ResponseError::Cancelled) => PortalError::Cancelled,
        Error::Response(_) => PortalError::Denied("the desktop portal rejected the request".into()),
        Error::Portal(AshpdPortalError::Cancelled(_)) => PortalError::Cancelled,
        Error::Portal(AshpdPortalError::NotAllowed(message)) => PortalError::Denied(message),
        Error::Portal(AshpdPortalError::Failed(message)) => PortalError::Denied(message),
        Error::Portal(AshpdPortalError::InvalidArgument(message)) => PortalError::Denied(message),
        Error::PortalNotFound(_) | Error::RequiresVersion(_, _) => PortalError::Unsupported,
        other => PortalError::Unavailable(other.to_string()),
    }
}

type PortalRuntime = Result<tokio::runtime::Handle, String>;

/// Start the dedicated portal runtime once; the thread keeps it alive forever.
static RUNTIME: LazyLock<PortalRuntime> = LazyLock::new(|| {
    let (tx, rx) = mpsc::sync_channel::<PortalRuntime>(1);
    let spawned = std::thread::Builder::new()
        .name("signifiage-portal-runtime".to_string())
        .spawn(move || {
            let built = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build();
            match built {
                Ok(rt) => {
                    let _ = tx.send(Ok(rt.handle().clone()));
                    // Park this thread so the runtime keeps its I/O reactor.
                    rt.block_on(std::future::pending::<()>());
                }
                Err(err) => {
                    let _ = tx.send(Err(err.to_string()));
                }
            }
        });
    if let Err(err) = spawned {
        return Err(format!("cannot start the portal thread: {err}"));
    }
    rx.recv()
        .unwrap_or_else(|_| Err("the portal runtime thread stopped".to_string()))
});

fn runtime() -> Result<tokio::runtime::Handle, PortalError> {
    match &*RUNTIME {
        Ok(handle) => Ok(handle.clone()),
        Err(message) => Err(PortalError::Unavailable(message.clone())),
    }
}

/// One screencast session: session handle, PipeWire fd and node id.
pub struct PortalScreencast {
    /// Kept only so the portal session (and its PipeWire fd) stays alive.
    _session: Session<Screencast>,
    fd: OwnedFd,
    node: u32,
    restore_token: Option<String>,
    label: String,
}

impl std::fmt::Debug for PortalScreencast {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortalScreencast")
            .field("node", &self.node)
            .field("fd", &self.fd.as_raw_fd())
            .field("label", &self.label)
            .finish()
    }
}

impl PortalScreencast {
    /// Create a session and run the interactive picker.
    ///
    /// Blocking: call this from a worker thread. The UI should use
    /// [`request_async`] instead.
    pub fn request(restore_token: Option<String>) -> Result<Self, PortalError> {
        let handle = runtime()?;
        handle.block_on(Self::request_inner(restore_token))
    }

    async fn request_inner(restore_token: Option<String>) -> Result<Self, PortalError> {
        let proxy = Screencast::new().await.map_err(map_error)?;
        let session = proxy
            .create_session(Default::default())
            .await
            .map_err(map_error)?;
        let options = SelectSourcesOptions::default()
            .set_multiple(false)
            .set_cursor_mode(CursorMode::HIDDEN)
            .set_sources(SourceType::MONITOR | SourceType::WINDOW)
            .set_persist_mode(PersistMode::DoNot)
            .set_restore_token(restore_token.as_deref());
        proxy
            .select_sources(&session, options)
            .await
            .map_err(map_error)?;

        let streams = proxy
            .start(&session, None, Default::default())
            .await
            .map_err(map_error)?
            .response()
            .map_err(map_error)?;
        let restore_token = streams.restore_token().map(str::to_string);
        let stream = streams
            .streams()
            .first()
            .cloned()
            .ok_or(PortalError::NoStream)?;
        let node = stream.pipe_wire_node_id();
        let fd = proxy
            .open_pipe_wire_remote(&session, Default::default())
            .await
            .map_err(map_error)?;
        tracing::info!(node, "portal screen cast started");
        Ok(Self {
            _session: session,
            fd,
            node,
            restore_token,
            label: rust_i18n::t!("capture.type.portal").to_string(),
        })
    }

    /// Raw PipeWire remote descriptor; owned by this session.
    pub fn fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }

    /// PipeWire node id reported by the portal.
    pub fn node_id(&self) -> u32 {
        self.node
    }

    /// Token for preselecting this source on the next request.
    pub fn restore_token(&self) -> Option<&str> {
        self.restore_token.as_deref()
    }

    /// Human label for the captured source.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Snapshot of the stream for planning and UI display.
    pub fn stream(&self) -> PortalStream {
        PortalStream {
            fd: self.fd(),
            node: self.node,
            label: self.label.clone(),
            restore_token: self.restore_token.clone(),
        }
    }
}

/// Remote desktop session used for keyboard injection on Wayland.
pub struct PortalRemoteDesktop {
    session: Session<RemoteDesktop>,
    proxy: RemoteDesktop,
}

impl std::fmt::Debug for PortalRemoteDesktop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortalRemoteDesktop").finish()
    }
}

impl PortalRemoteDesktop {
    /// Ask for a keyboard-only remote desktop session.
    ///
    /// Blocking; the first call shows the portal permission dialog.
    pub fn connect() -> Result<Self, PortalError> {
        let handle = runtime()?;
        handle.block_on(async {
            let proxy = RemoteDesktop::new().await.map_err(map_error)?;
            let session = proxy
                .create_session(Default::default())
                .await
                .map_err(map_error)?;
            proxy
                .select_devices(
                    &session,
                    SelectDevicesOptions::default().set_devices(DeviceType::KEYBOARD),
                )
                .await
                .map_err(map_error)?;
            proxy
                .start(&session, None, Default::default())
                .await
                .map_err(map_error)?
                .response()
                .map_err(map_error)?;
            tracing::info!("portal remote desktop session started");
            Ok(Self { session, proxy })
        })
    }

    /// Press or release a keysym inside the remote desktop session.
    pub fn send_keysym(&self, keysym: u32, pressed: bool) -> Result<(), PortalError> {
        let handle = runtime()?;
        let state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        handle.block_on(async {
            self.proxy
                .notify_keyboard_keysym(&self.session, keysym as i32, state, Default::default())
                .await
                .map_err(map_error)
        })
    }
}

/// Per-source cache of approved screencast sessions.
///
/// Keys are capture-source identity strings (`CaptureSource::pipeline_key`), so
/// returning to a scene reuses the same session instead of re-prompting.
#[derive(Default)]
pub struct PortalCache {
    sessions: HashMap<String, PortalScreencast>,
}

impl PortalCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contains(&self, layer_id: &str) -> bool {
        self.sessions.contains_key(layer_id)
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Approved stream for `layer_id`, if any.
    pub fn stream_for(&self, layer_id: &str) -> Result<PortalStream, PortalError> {
        self.sessions
            .get(layer_id)
            .map(PortalScreencast::stream)
            .ok_or(PortalError::NotRequested)
    }

    /// Insert or replace the session for `layer_id`.
    pub fn insert(
        &mut self,
        layer_id: impl Into<String>,
        cast: PortalScreencast,
    ) -> Option<PortalScreencast> {
        self.sessions.insert(layer_id.into(), cast)
    }

    /// Return the cached stream or run the portal picker right now.
    ///
    /// Blocking: call only from a worker thread.
    pub fn get_or_request(
        &mut self,
        layer_id: &str,
        restore_token: Option<String>,
    ) -> Result<PortalStream, PortalError> {
        if let Some(cast) = self.sessions.get(layer_id) {
            return Ok(cast.stream());
        }
        let cast = PortalScreencast::request(restore_token)?;
        let stream = cast.stream();
        self.sessions.insert(layer_id.to_string(), cast);
        Ok(stream)
    }

    /// Drop the session (and with it the PipeWire fd) for `layer_id`.
    pub fn remove(&mut self, layer_id: &str) -> bool {
        self.sessions.remove(layer_id).is_some()
    }

    pub fn clear(&mut self) {
        self.sessions.clear();
    }
}

static CACHE: LazyLock<Mutex<PortalCache>> = LazyLock::new(|| Mutex::new(PortalCache::new()));

/// Process-global cache shared by the controller, the inspector and the player.
pub fn cache() -> &'static Mutex<PortalCache> {
    &CACHE
}

fn lock_cache() -> Result<std::sync::MutexGuard<'static, PortalCache>, PortalError> {
    cache()
        .lock()
        .map_err(|_| PortalError::Unavailable("the portal cache is poisoned".into()))
}

/// Store an approved session under `layer_id`.
pub fn store(layer_id: &str, cast: PortalScreencast) {
    match cache().lock() {
        Ok(mut cache) => {
            cache.insert(layer_id.to_string(), cast);
        }
        Err(_) => tracing::error!("portal cache is poisoned; dropping the session"),
    }
}

/// Cached stream for `layer_id`; [`PortalError::NotRequested`] when unapproved.
pub fn stream_for_key(layer_id: &str) -> Result<PortalStream, PortalError> {
    lock_cache()?.stream_for(layer_id)
}

/// Forget the session for `layer_id`; the caller must stop dependent pipelines
/// first because the PipeWire descriptor is closed.
pub fn forget(layer_id: &str) -> bool {
    match cache().lock() {
        Ok(mut cache) => cache.remove(layer_id),
        Err(_) => false,
    }
}

/// Run the portal picker on a worker thread and deliver its result over a channel.
///
/// The approved session is installed in the cache under `layer_id` before the
/// result is sent, so the UI only has to retry planning when it receives `Ok`.
/// A dropped receiver means the worker could not be started.
pub fn request_async(
    layer_id: String,
    restore_token: Option<String>,
) -> Receiver<Result<PortalStream, PortalError>> {
    let (tx, rx) = mpsc::channel();
    let spawn = std::thread::Builder::new()
        .name("signifiage-portal-request".to_string())
        .spawn(move || {
            let outcome = match PortalScreencast::request(restore_token) {
                Ok(cast) => {
                    let stream = cast.stream();
                    store(&layer_id, cast);
                    Ok(stream)
                }
                Err(err) => {
                    tracing::warn!(%err, "screen capture request failed");
                    Err(err)
                }
            };
            let _ = tx.send(outcome);
        });
    if let Err(err) = spawn {
        tracing::error!(%err, "cannot start the portal request thread");
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_errors_have_text() {
        for err in [
            PortalError::Cancelled,
            PortalError::Unsupported,
            PortalError::NoStream,
            PortalError::NotRequested,
            PortalError::Denied("nope".into()),
            PortalError::Unavailable("no bus".into()),
        ] {
            assert!(!err.to_string().is_empty());
            assert!(!err.message().is_empty());
        }
    }

    #[test]
    fn cache_starts_empty_and_rejects_unknown_keys() {
        let mut cache = PortalCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.stream_for("nobody"), Err(PortalError::NotRequested));
        assert!(!cache.remove("nobody"));
    }
}
