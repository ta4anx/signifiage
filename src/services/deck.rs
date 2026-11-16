//! Deck control: send next/previous keys to the focused presentation.
//!
//! X11 sessions use `services::x11` (activate the captured window, XTest key
//! press/release) on a worker thread. Wayland sessions go through the Remote
//! Desktop portal, which can only inject into whichever window has focus; the UI
//! is responsible for the 2 s "focus the presentation" delay. Errors are never
//! fatal: they surface through [`DeckController::last_error`].

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::model::deck;
use crate::services::portal::{PortalError, PortalRemoteDesktop};
use crate::services::x11;

/// How keys reach the presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckBackend {
    X11,
    Portal,
}

impl DeckBackend {
    pub fn id(self) -> &'static str {
        match self {
            DeckBackend::X11 => "x11",
            DeckBackend::Portal => "portal",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "x11" => Some(DeckBackend::X11),
            "portal" => Some(DeckBackend::Portal),
            _ => None,
        }
    }
}

/// Failures reported by deck actions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeckError {
    #[error("No X11 display is available; deck control needs an X session")]
    NoDisplay,
    #[error("The program has no captured window to focus")]
    NoTarget,
    #[error("The remote desktop portal is unavailable: {0}")]
    Portal(String),
    #[error("The presentation window could not be activated: {0}")]
    X11(String),
}

impl DeckError {
    /// Localised, actionable text for the deck card and the web remote.
    pub fn message(&self) -> String {
        match self {
            DeckError::NoDisplay | DeckError::NoTarget => {
                rust_i18n::t!("deck.unavailable").to_string()
            }
            DeckError::Portal(detail) | DeckError::X11(detail) => detail.clone(),
        }
    }
}

/// Errors stay visible for this long, then the deck card reports the normal hint.
const LAST_ERROR_TTL: Duration = Duration::from_secs(5);
/// Gap between the synthetic press and release in the portal session.
const PORTAL_KEY_GAP: Duration = Duration::from_millis(30);

type ErrorSlot = Arc<Mutex<Option<(String, Instant)>>>;

/// Sends deck keys through the configured backend.
pub struct DeckController {
    backend: DeckBackend,
    available: bool,
    hint: String,
    last_error: ErrorSlot,
    portal: Arc<Mutex<Option<PortalRemoteDesktop>>>,
}

impl DeckController {
    pub fn new(backend: DeckBackend) -> Self {
        let (available, hint) = match backend {
            DeckBackend::X11 => {
                let available = x11::display_available();
                let hint = if available {
                    rust_i18n::t!("deck.hint").to_string()
                } else {
                    rust_i18n::t!("deck.unavailable").to_string()
                };
                (available, hint)
            }
            // The portal is asked for permission on first use; only then can it
            // fail, so availability starts optimistic.
            DeckBackend::Portal => (true, rust_i18n::t!("deck.hint").to_string()),
        };
        Self {
            backend,
            available,
            hint,
            last_error: Arc::new(Mutex::new(None)),
            portal: Arc::new(Mutex::new(None)),
        }
    }

    pub fn backend(&self) -> DeckBackend {
        self.backend
    }

    /// False when the selected backend has no usable session.
    pub fn available(&self) -> bool {
        self.available
    }

    /// Human hint for the deck card and the web remote.
    pub fn hint(&self) -> &str {
        &self.hint
    }

    /// Most recent error, cleared automatically after five seconds.
    pub fn last_error(&self) -> Option<String> {
        let guard = self.last_error.lock().ok()?;
        guard
            .as_ref()
            .and_then(|(message, at)| (at.elapsed() < LAST_ERROR_TTL).then(|| message.clone()))
    }

    pub fn clear_error(&self) {
        if let Ok(mut guard) = self.last_error.lock() {
            *guard = None;
        }
    }

    /// Send the "next" key. `target` is the captured X11 window, when known.
    pub fn next(&self, keysym: u32, target: Option<u32>) -> Result<(), DeckError> {
        self.send(keysym, target)
    }

    /// Send the "previous" key. `target` is the captured X11 window, when known.
    pub fn prev(&self, keysym: u32, target: Option<u32>) -> Result<(), DeckError> {
        self.send(keysym, target)
    }

    fn send(&self, keysym: u32, target: Option<u32>) -> Result<(), DeckError> {
        match self.backend {
            DeckBackend::X11 => {
                if !self.available {
                    let err = DeckError::NoDisplay;
                    record(&self.last_error, &err);
                    return Err(err);
                }
                let slot = self.last_error.clone();
                std::thread::Builder::new()
                    .name("signifiage-deck-x11".to_string())
                    .spawn(move || {
                        // `send_key` activates the target, waits 80 ms, then
                        // presses/releases 30 ms apart.
                        if let Err(err) = x11::send_key(target, keysym) {
                            let err = DeckError::X11(err.to_string());
                            record(&slot, &err);
                        }
                    })
                    .map_err(|err| {
                        let err = DeckError::X11(format!("cannot start the key thread: {err}"));
                        record(&self.last_error, &err);
                        err
                    })?;
                Ok(())
            }
            DeckBackend::Portal => {
                let portal = self.portal.clone();
                let slot = self.last_error.clone();
                std::thread::Builder::new()
                    .name("signifiage-deck-portal".to_string())
                    .spawn(move || {
                        let outcome = send_portal(&portal, keysym);
                        if let Err(err) = outcome {
                            record(&slot, &err);
                        }
                    })
                    .map_err(|err| {
                        let err = DeckError::Portal(format!("cannot start the key thread: {err}"));
                        record(&self.last_error, &err);
                        err
                    })?;
                Ok(())
            }
        }
    }
}

fn send_portal(
    portal: &Arc<Mutex<Option<PortalRemoteDesktop>>>,
    keysym: u32,
) -> Result<(), DeckError> {
    let mut guard = portal
        .lock()
        .map_err(|_| DeckError::Portal("the portal session is poisoned".to_string()))?;
    if guard.is_none() {
        *guard = Some(PortalRemoteDesktop::connect().map_err(map_portal)?);
    }
    let desktop = guard
        .as_ref()
        .ok_or_else(|| DeckError::Portal("the portal session is missing".to_string()))?;
    desktop.send_keysym(keysym, true).map_err(map_portal)?;
    std::thread::sleep(PORTAL_KEY_GAP);
    desktop.send_keysym(keysym, false).map_err(map_portal)?;
    Ok(())
}

fn map_portal(err: PortalError) -> DeckError {
    match err {
        PortalError::Unsupported => {
            DeckError::Portal(rust_i18n::t!("capture.error.portal_unavailable").to_string())
        }
        other => DeckError::Portal(other.to_string()),
    }
}

fn record(slot: &ErrorSlot, err: &DeckError) {
    match slot.lock() {
        Ok(mut guard) => *guard = Some((err.message(), Instant::now())),
        Err(_) => tracing::warn!(%err, "deck error (slot poisoned)"),
    }
}

/// X11 keysym for one of the stored deck key names.
pub fn keysym_for(key: &str) -> Option<u32> {
    deck::keysym(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysym_for_delegates_to_the_model() {
        for key in deck::DECK_KEYS {
            assert_eq!(keysym_for(key), deck::keysym(key), "{key}");
        }
        assert_eq!(keysym_for("not-a-key"), None);
    }

    #[test]
    fn backend_ids_round_trip() {
        for backend in [DeckBackend::X11, DeckBackend::Portal] {
            assert_eq!(DeckBackend::from_id(backend.id()), Some(backend));
        }
        assert_eq!(DeckBackend::from_id("xtest"), None);
    }

    #[test]
    fn hints_and_errors_are_never_empty() {
        let portal = DeckController::new(DeckBackend::Portal);
        assert!(!portal.hint().is_empty());
        assert!(portal.available());
        let err = DeckError::NoDisplay;
        assert!(!err.to_string().is_empty());
        assert!(!err.message().is_empty());
    }
}
