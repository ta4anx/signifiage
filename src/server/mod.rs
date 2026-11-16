//! LAN web remote: embedded UI, state publication, actions and uploads.
//!
//! One [`RemoteServer`] runs per process. It owns a small Tokio runtime on a
//! dedicated thread; the GTK main thread only talks to it through
//! [`RemoteServer::publish`] (a watch channel write) and an
//! `mpsc::Sender<RemoteAction>` for commands. No GTK object is touched off the
//! main thread.

pub mod actions;
mod routes;
mod uploads;

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::runtime::Builder;
use tokio::sync::{mpsc, watch};

pub use actions::{
    ActionError, DeckCommand, PatternCommand, PlaybackCommand, PrompterCommand, QueueCommand,
    RemoteAction, SoundboardCommand, TimerCommand,
};

/// Which application owns the server, mirrored to the web UI as `mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Gui,
    Player,
}

impl Mode {
    pub fn as_id(self) -> &'static str {
        match self {
            Mode::Gui => "gui",
            Mode::Player => "player",
        }
    }
}

impl From<crate::services::controller::Mode> for Mode {
    fn from(mode: crate::services::controller::Mode) -> Self {
        match mode {
            crate::services::controller::Mode::Gui => Mode::Gui,
            crate::services::controller::Mode::Player => Mode::Player,
        }
    }
}

/// Minimum gap between SSE state sends; the controller publishes 10×/s and the
/// UI must not be flooded.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(500);
/// How often the publisher thread flushes a coalesced pending state.
const FLUSH_INTERVAL: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------- state model

/// One program/preview playlist reference for the web selector.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaylistRef {
    pub id: String,
    pub name: String,
}

/// What the program output is showing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgramInfo {
    pub name: String,
    pub scene_id: Option<String>,
    /// `black`, `playlist`, `manual` or `pattern`.
    pub source: String,
    pub black: bool,
    pub freeze: bool,
    pub mute: bool,
    pub on_air: bool,
    pub remaining_secs: Option<f64>,
}

impl Default for ProgramInfo {
    fn default() -> Self {
        Self {
            name: String::new(),
            scene_id: None,
            source: "black".to_string(),
            black: false,
            freeze: false,
            mute: false,
            on_air: false,
            remaining_secs: None,
        }
    }
}

/// Active playlist and preview/program positions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaylistInfo {
    pub id: String,
    pub name: String,
    pub preview_index: usize,
    pub program_index: Option<usize>,
    pub total: usize,
    #[serde(rename = "loop")]
    pub looping: bool,
    pub auto: bool,
}

/// A queue row as shown on the remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueEntryInfo {
    pub ticket: String,
    pub name: String,
    pub note: String,
}

impl Default for QueueEntryInfo {
    fn default() -> Self {
        Self {
            ticket: String::new(),
            name: String::new(),
            note: String::new(),
        }
    }
}

/// Current row, the next five waiting rows and status counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueInfo {
    pub current: Option<QueueEntryInfo>,
    pub next: Vec<QueueEntryInfo>,
    pub waiting: usize,
    pub called: usize,
    pub done: usize,
    pub skipped: usize,
}

/// One timer, formatted by the UI for the remote.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimerInfo {
    pub id: String,
    pub name: String,
    pub running: bool,
    pub elapsed_secs: i64,
    pub remaining_secs: Option<i64>,
    /// Human-readable countdown target (`HH:MM`), when the timer has one.
    pub end_at: Option<String>,
}

/// Teleprompter runtime state (text itself is not sent to phones).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrompterInfo {
    pub running: bool,
    pub speed: f64,
}

impl Default for PrompterInfo {
    fn default() -> Self {
        Self {
            running: false,
            speed: 45.0,
        }
    }
}

/// One soundboard pad button.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PadInfo {
    pub id: String,
    pub label: String,
    pub color: String,
    pub hotkey: String,
    pub playing: bool,
}

/// One soundboard page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundboardPageInfo {
    pub id: String,
    pub name: String,
    pub pads: Vec<PadInfo>,
}

/// Soundboard pages and the active index.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundboardInfo {
    pub active_page: usize,
    pub pages: Vec<SoundboardPageInfo>,
}

/// Deck-control availability and the last error (cleared by the UI after 5 s).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeckInfo {
    pub available: bool,
    pub label: String,
    pub backend: String,
    pub hint: String,
    pub last_error: String,
}

/// A built-in test pattern offered by the remote.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PatternInfo {
    pub id: String,
    pub name: String,
}

/// Version handshake for the embedded page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteInfo {
    pub version: String,
}

impl Default for RemoteInfo {
    fn default() -> Self {
        Self {
            version: crate::VERSION.to_string(),
        }
    }
}

/// Complete snapshot rendered by `web/app.js`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteState {
    pub mode: Mode,
    pub project: Option<String>,
    pub locale: String,
    pub program: ProgramInfo,
    pub playlist: PlaylistInfo,
    /// Playlist choices for the remote selector (extra key used by the page).
    pub playlists: Vec<PlaylistRef>,
    pub queue: QueueInfo,
    pub timers: Vec<TimerInfo>,
    pub prompter: PrompterInfo,
    pub soundboard: SoundboardInfo,
    pub deck: DeckInfo,
    pub patterns: Vec<PatternInfo>,
    pub pattern_override: bool,
    pub remote: RemoteInfo,
}

impl Default for RemoteState {
    fn default() -> Self {
        Self {
            mode: Mode::Gui,
            project: None,
            locale: "en".to_string(),
            program: ProgramInfo::default(),
            playlist: PlaylistInfo::default(),
            playlists: Vec::new(),
            queue: QueueInfo::default(),
            timers: Vec::new(),
            prompter: PrompterInfo::default(),
            soundboard: SoundboardInfo::default(),
            deck: DeckInfo::default(),
            patterns: Vec::new(),
            pattern_override: false,
            remote: RemoteInfo::default(),
        }
    }
}

impl RemoteState {
    /// A snapshot with the right mode/locale and version; fill the rest.
    pub fn new(mode: Mode, locale: impl Into<String>) -> Self {
        Self {
            mode,
            locale: locale.into(),
            ..Self::default()
        }
    }
}

// ------------------------------------------------------------------ publisher

#[derive(Debug, Default)]
struct PublisherState {
    last_json: Option<String>,
    last_at: Option<Instant>,
    pending: Option<String>,
}

/// Data shared with the HTTP handlers and the publisher thread.
pub(crate) struct SharedState {
    tx: watch::Sender<Arc<String>>,
    publish: Mutex<PublisherState>,
    has_project: AtomicBool,
    project_dir: Mutex<Option<PathBuf>>,
    stopping: AtomicBool,
}

impl SharedState {
    fn new(tx: watch::Sender<Arc<String>>) -> Self {
        Self {
            tx,
            publish: Mutex::new(PublisherState::default()),
            has_project: AtomicBool::new(false),
            project_dir: Mutex::new(None),
            stopping: AtomicBool::new(false),
        }
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<Arc<String>> {
        self.tx.subscribe()
    }

    pub(crate) fn latest(&self) -> Arc<String> {
        self.tx.subscribe().borrow().clone()
    }

    pub(crate) fn has_project(&self) -> bool {
        self.has_project.load(Ordering::SeqCst)
    }

    pub(crate) fn project_dir(&self) -> Option<PathBuf> {
        self.project_dir
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_project_dir(&self, dir: Option<PathBuf>) {
        self.has_project.store(dir.is_some(), Ordering::SeqCst);
        *self
            .project_dir
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = dir;
    }

    fn publisher(&self) -> std::sync::MutexGuard<'_, PublisherState> {
        self.publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record the newest desired state and send it when the rate limit allows.
    fn offer(&self, json: String, force: bool) {
        let to_send = {
            let mut publisher = self.publisher();
            if publisher.last_json.as_deref() == Some(json.as_str()) {
                // The desired state is already on the wire; anything queued is
                // older than it and must not be sent later.
                publisher.pending = None;
                None
            } else if force
                || publisher
                    .last_at
                    .is_none_or(|at| at.elapsed() >= PUBLISH_INTERVAL)
            {
                publisher.last_json = Some(json.clone());
                publisher.last_at = Some(Instant::now());
                publisher.pending = None;
                Some(json)
            } else {
                publisher.pending = Some(json);
                None
            }
        };
        if let Some(json) = to_send {
            self.tx.send_replace(Arc::new(json));
        }
    }

    /// Send the coalesced state once the 500 ms window has elapsed.
    fn flush_pending(&self) {
        let to_send = {
            let mut publisher = self.publisher();
            let Some(pending) = publisher.pending.clone() else {
                return;
            };
            if publisher.last_json.as_deref() == Some(pending.as_str()) {
                publisher.pending = None;
                return;
            }
            if publisher
                .last_at
                .is_some_and(|at| at.elapsed() < PUBLISH_INTERVAL)
            {
                return;
            }
            publisher.last_json = Some(pending.clone());
            publisher.last_at = Some(Instant::now());
            publisher.pending = None;
            Some(pending)
        };
        if let Some(json) = to_send {
            self.tx.send_replace(Arc::new(json));
        }
    }

    fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }
}

// --------------------------------------------------------------- server handle

/// A running web remote. Dropping it shuts the HTTP server down.
pub struct RemoteServer {
    inner: Option<Inner>,
}

struct Inner {
    shared: Arc<SharedState>,
    shutdown: watch::Sender<bool>,
    http_thread: Option<std::thread::JoinHandle<()>>,
    flush_thread: Option<std::thread::JoinHandle<()>>,
    port: u16,
}

impl RemoteServer {
    /// Bind `0.0.0.0:<port>` and serve until [`RemoteServer::stop`].
    ///
    /// Bind and thread-spawn errors are returned so Settings can show the OS
    /// error and keep the rest of the app running. An empty token is refused:
    /// the caller must generate one first ([`crate::config::Config::ensure_remote_token`]).
    pub fn start(
        port: u16,
        token: String,
        mode: Mode,
        sender: mpsc::Sender<RemoteAction>,
    ) -> io::Result<Self> {
        let token = token.trim().to_string();
        if token.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "remote token must not be empty",
            ));
        }

        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port))?;
        listener.set_nonblocking(true)?;
        let bound_port = listener.local_addr()?.port();

        let (state_tx, _state_rx) = watch::channel(Arc::new(String::new()));
        let shared = Arc::new(SharedState::new(state_tx));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let app_state = Arc::new(routes::AppState {
            shared: shared.clone(),
            token: Arc::from(token.as_str()),
            mode,
            sender,
            shutdown: shutdown_rx.clone(),
            upload_lock: tokio::sync::Mutex::new(()),
        });
        let router = routes::router(app_state);

        let runtime = Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("signifiage-remote-io")
            .enable_all()
            .build()?;

        let http_thread = match std::thread::Builder::new()
            .name("signifiage-remote".to_string())
            .spawn(move || {
                let result = runtime.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener)?;
                    axum::serve(listener, router)
                        .with_graceful_shutdown(routes::wait_shutdown(shutdown_rx))
                        .await
                });
                if let Err(error) = result {
                    tracing::warn!("remote server stopped with error: {error}");
                }
            }) {
            Ok(handle) => handle,
            Err(error) => return Err(error),
        };

        let flush_shared = shared.clone();
        let flush_thread = match std::thread::Builder::new()
            .name("signifiage-remote-publish".to_string())
            .spawn(move || {
                while !flush_shared.is_stopping() {
                    std::thread::sleep(FLUSH_INTERVAL);
                    flush_shared.flush_pending();
                }
            }) {
            Ok(handle) => handle,
            Err(error) => {
                shared.stopping.store(true, Ordering::SeqCst);
                let _ = shutdown_tx.send_replace(true);
                let _ = http_thread.join();
                return Err(error);
            }
        };

        tracing::info!(
            port = bound_port,
            mode = mode.as_id(),
            "remote server started"
        );
        Ok(Self {
            inner: Some(Inner {
                shared,
                shutdown: shutdown_tx,
                http_thread: Some(http_thread),
                flush_thread: Some(flush_thread),
                port: bound_port,
            }),
        })
    }

    /// The TCP port actually bound (useful when `port == 0`).
    pub fn port(&self) -> u16 {
        self.inner.as_ref().map(|inner| inner.port).unwrap_or(0)
    }

    /// Local URL for Settings; the LAN address is assembled by the UI.
    pub fn local_url(&self) -> String {
        format!("http://localhost:{}", self.port())
    }

    /// Push a state snapshot. Identical JSON is dropped and bursts are
    /// coalesced to at most two SSE events per second.
    pub fn publish(&self, state: &RemoteState) {
        self.publish_with(state, false);
    }

    /// Push a state snapshot immediately (after a mutation/action), bypassing
    /// the 500 ms coalescing window. Identical JSON is still dropped.
    pub fn publish_now(&self, state: &RemoteState) {
        self.publish_with(state, true);
    }

    fn publish_with(&self, state: &RemoteState, force: bool) {
        let Some(inner) = &self.inner else {
            return;
        };
        inner
            .shared
            .has_project
            .store(state.project.is_some(), Ordering::SeqCst);
        match serde_json::to_string(state) {
            Ok(json) => inner.shared.offer(json, force),
            Err(error) => tracing::warn!("cannot encode remote state: {error}"),
        }
    }

    /// Keep the upload target in sync with the open project.
    pub fn set_project_dir(&self, dir: Option<PathBuf>) {
        if let Some(inner) = &self.inner {
            inner.shared.set_project_dir(dir);
        }
    }

    /// Stop accepting requests and end active SSE streams.
    pub fn stop(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        let Some(mut inner) = self.inner.take() else {
            return;
        };
        inner.shared.stopping.store(true, Ordering::SeqCst);
        let _ = inner.shutdown.send_replace(true);
        if let Some(handle) = inner.http_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = inner.flush_thread.take() {
            let _ = handle.join();
        }
        tracing::info!("remote server stopped");
    }
}

impl Drop for RemoteServer {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    fn request_bytes(
        port: u16,
        method: &str,
        path: &str,
        token: Option<&str>,
        content_type: Option<&str>,
        body: Option<&[u8]>,
    ) -> (u16, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("write timeout");
        let mut request =
            format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
        if let Some(token) = token {
            request.push_str(&format!("X-Signifiage-Token: {token}\r\n"));
        }
        if let Some(content_type) = content_type {
            request.push_str(&format!("Content-Type: {content_type}\r\n"));
        }
        if let Some(body) = body {
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).expect("write request");
        if let Some(body) = body {
            stream.write_all(body).expect("write body");
        }
        stream.flush().expect("flush");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read response");
        let text = String::from_utf8_lossy(&response).to_string();
        let status = text
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|status| status.parse::<u16>().ok())
            .expect("status line");
        let body = text
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default();
        (status, body)
    }

    fn http(
        port: u16,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<&str>,
    ) -> (u16, String) {
        request_bytes(
            port,
            method,
            path,
            token,
            body.map(|_| "application/json"),
            body.map(str::as_bytes),
        )
    }

    fn upload(port: u16, path: &str, token: &str, filename: &str, bytes: &[u8]) -> (u16, String) {
        const BOUNDARY: &str = "signifiagetestboundary";
        let mut body = Vec::new();
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        request_bytes(
            port,
            "POST",
            path,
            Some(token),
            Some(&format!("multipart/form-data; boundary={BOUNDARY}")),
            Some(&body),
        )
    }

    #[test]
    fn state_json_has_the_shape_the_page_expects() {
        let mut state = RemoteState::new(Mode::Player, "tr");
        state.project = Some("Demo".to_string());
        state.program.name = "Welcome".to_string();
        state.program.on_air = true;
        state.playlist.looping = true;
        state.queue.current = Some(QueueEntryInfo {
            ticket: "001".to_string(),
            name: "Ayşe".to_string(),
            note: String::new(),
        });
        let value = serde_json::to_value(&state).expect("serialize");
        assert_eq!(value["mode"], "player");
        assert_eq!(value["project"], "Demo");
        assert_eq!(value["locale"], "tr");
        assert_eq!(value["program"]["on_air"], true);
        assert_eq!(value["program"]["source"], "black");
        assert_eq!(value["playlist"]["loop"], true);
        assert_eq!(value["queue"]["current"]["ticket"], "001");
        assert_eq!(value["queue"]["waiting"], 0);
        assert!(value["timers"].is_array());
        assert_eq!(value["prompter"]["speed"], 45.0);
        assert!(value["soundboard"]["pages"].is_array());
        assert_eq!(value["deck"]["available"], false);
        assert!(value["patterns"].is_array());
        assert_eq!(value["pattern_override"], false);
        assert_eq!(value["remote"]["version"], crate::VERSION);
        assert!(value["playlists"].is_array());

        let round_trip: RemoteState = serde_json::from_value(value).expect("deserialize");
        assert_eq!(round_trip, state);
    }

    #[test]
    fn serves_ui_state_actions_and_rejects_bad_tokens() {
        let (sender, mut receiver) = mpsc::channel(8);
        let server = RemoteServer::start(0, "test-token-123".to_string(), Mode::Gui, sender)
            .expect("server should start");
        let port = server.port();
        assert!(port > 0);

        // Static UI is public.
        let (status, body) = http(port, "GET", "/", None, None);
        assert_eq!(status, 200);
        assert!(
            body.contains("<title>Signifiage</title>"),
            "index body: {body}"
        );
        let (status, _) = http(port, "GET", "/app.js", None, None);
        assert_eq!(status, 200);
        let (status, _) = http(port, "GET", "/i18n.json", None, None);
        assert_eq!(status, 200);

        // API requires the token; no project yet -> 409.
        assert_eq!(http(port, "GET", "/api/state", None, None).0, 401);
        assert_eq!(http(port, "GET", "/api/state", Some("wrong"), None).0, 401);
        assert_eq!(
            http(port, "GET", "/api/state", Some("test-token-123"), None).0,
            409
        );
        assert_eq!(
            http(
                port,
                "POST",
                "/api/actions",
                Some("test-token-123"),
                Some("{\"action\":\"queue.next\",\"args\":{}}")
            )
            .0,
            409
        );

        let mut state = RemoteState::new(Mode::Gui, "en");
        state.project = Some("Demo".to_string());
        state.program.name = "Welcome".to_string();
        server.publish(&state);

        let (status, body) = http(port, "GET", "/api/state", Some("test-token-123"), None);
        assert_eq!(status, 200);
        let parsed: RemoteState = serde_json::from_str(&body).expect("state json");
        assert_eq!(parsed.project.as_deref(), Some("Demo"));
        assert_eq!(parsed.program.name, "Welcome");

        let (status, body) = http(
            port,
            "POST",
            "/api/actions",
            Some("test-token-123"),
            Some("{\"action\":\"queue.next\",\"args\":{}}"),
        );
        assert_eq!(status, 200);
        assert!(body.contains("\"ok\":true"), "action body: {body}");
        assert_eq!(
            receiver.try_recv().expect("action enqueued"),
            RemoteAction::Queue(QueueCommand::Next)
        );

        // Unknown action and missing args are 400 and enqueue nothing.
        assert_eq!(
            http(
                port,
                "POST",
                "/api/actions",
                Some("test-token-123"),
                Some("{\"action\":\"queue.explode\",\"args\":{}}")
            )
            .0,
            400
        );
        assert_eq!(
            http(
                port,
                "POST",
                "/api/actions",
                Some("test-token-123"),
                Some("{\"action\":\"playback.black\",\"args\":{}}")
            )
            .0,
            400
        );
        assert!(receiver.try_recv().is_err());

        // Token in the query string works for EventSource-style clients.
        assert_eq!(
            http(port, "GET", "/api/state?token=test-token-123", None, None).0,
            200
        );
        assert_eq!(
            http(port, "GET", "/api/state?token=nope", None, None).0,
            401
        );

        server.stop();
    }

    #[test]
    fn sse_stream_sends_the_current_state_immediately() {
        let (sender, _rx) = mpsc::channel(4);
        let server = RemoteServer::start(0, "sse-token-123".to_string(), Mode::Gui, sender)
            .expect("server should start");
        let mut state = RemoteState::new(Mode::Gui, "en");
        state.project = Some("Demo".to_string());
        state.program.name = "Welcome".to_string();
        server.publish(&state);

        let mut stream = TcpStream::connect(("127.0.0.1", server.port())).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        stream
            .write_all(
                b"GET /api/events?token=sse-token-123 HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: text/event-stream\r\n\r\n",
            )
            .expect("write request");

        let mut collected = Vec::new();
        let mut buffer = [0u8; 4096];
        while collected.len() < 64 * 1024 {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    collected.extend_from_slice(&buffer[..read]);
                    let text = String::from_utf8_lossy(&collected);
                    if text.contains("event: state") && text.contains("\"project\":\"Demo\"") {
                        break;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(_) => break,
            }
        }
        drop(stream);
        server.stop();

        let response = String::from_utf8_lossy(&collected);
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "unexpected response: {response}"
        );
        assert!(
            response.contains("text/event-stream"),
            "missing SSE content type: {response}"
        );
        assert!(
            response.contains("event: state"),
            "missing state event: {response}"
        );
        assert!(
            response.contains("\"project\":\"Demo\""),
            "missing state payload: {response}"
        );
    }

    #[test]
    fn uploads_are_sanitized_stored_and_announced() {
        let (sender, mut receiver) = mpsc::channel(8);
        let server = RemoteServer::start(0, "upload-token-123".to_string(), Mode::Gui, sender)
            .expect("server should start");
        let dir = std::env::temp_dir().join(format!("signifiage-upload-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp project");
        server.set_project_dir(Some(dir.clone()));
        let port = server.port();

        let (status, body) = upload(
            port,
            "/api/upload",
            "upload-token-123",
            "../../My Clip.PNG",
            b"first",
        );
        assert_eq!(status, 200, "body: {body}");
        let payload: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["asset"], "assets/My_Clip.png");
        assert_eq!(
            std::fs::read(dir.join("assets/My_Clip.png")).expect("stored file"),
            b"first".to_vec()
        );
        match receiver.try_recv().expect("upload announced") {
            RemoteAction::AssetUploaded { asset } => {
                assert_eq!(asset, "assets/My_Clip.png");
            }
            other => panic!("unexpected action {other:?}"),
        }

        let (status, body) = upload(
            port,
            "/api/upload",
            "upload-token-123",
            "My Clip.PNG",
            b"second",
        );
        assert_eq!(status, 200, "body: {body}");
        let payload: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(payload["asset"], "assets/My_Clip-2.png");
        assert_eq!(
            std::fs::read(dir.join("assets/My_Clip-2.png")).expect("collision file"),
            b"second".to_vec()
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(RemoteAction::AssetUploaded { .. })
        ));

        assert_eq!(
            upload(port, "/api/upload", "upload-token-123", "notes.txt", b"x").0,
            415
        );
        assert_eq!(
            upload(port, "/api/upload", "wrong-token", "pic.png", b"x").0,
            401
        );
        assert!(receiver.try_recv().is_err());
        server.stop();

        // Player mode refuses uploads: the package is never edited.
        let (sender, _rx) = mpsc::channel(2);
        let player = RemoteServer::start(0, "upload-token-456".to_string(), Mode::Player, sender)
            .expect("player server should start");
        player.set_project_dir(Some(dir.clone()));
        assert_eq!(
            upload(
                player.port(),
                "/api/upload",
                "upload-token-456",
                "screen.png",
                b"x"
            )
            .0,
            409
        );
        player.stop();

        let _ = std::fs::remove_dir_all(&dir);
    }
}
