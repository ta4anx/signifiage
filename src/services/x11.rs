//! X11 window listing and key injection.
//!
//! This module is deliberately plain Rust: no GTK/glib types and no threads are
//! created here. Callers own any worker thread; every function opens its own
//! short-lived X11 connection and returns `Result`/`Option` instead of panicking.
//!
//! `WM_CLASS` is reported as `instance.class` (NUL-separated parts joined with a
//! dot) so a presentation can be matched by its class prefix without ambiguity.

use std::thread::sleep;
use std::time::Duration;

use x11rb::CURRENT_TIME;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageData, ClientMessageEvent, ConnectionExt as XprotoExt, EventMask,
    InputFocus, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, MapState, Window,
};
use x11rb::protocol::xtest::ConnectionExt as XtestExt;

/// How long to wait after activating a window before typing into it.
const ACTIVATE_SETTLE: Duration = Duration::from_millis(80);
/// Gap between synthetic key press/release events.
const KEY_GAP: Duration = Duration::from_millis(30);

/// Keysym for Shift_L; used when a keysym lives in the second keyboard-mapping column.
const SHIFT_L: u32 = 0xFFE1;
/// Fallback keycode for Shift_L when the keyboard mapping does not advertise it.
const FALLBACK_SHIFT_KEYCODE: u8 = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub xid: u32,
    pub title: String,
    pub wm_class: String,
    pub width: u32,
    pub height: u32,
    pub pid: Option<u32>,
}

#[derive(Debug, thiserror::Error)]
pub enum X11Error {
    #[error("no DISPLAY")]
    NoDisplay,
    #[error("x11: {0}")]
    X11(String),
    #[error("key injection failed: {0}")]
    Inject(String),
}

type RustConn = x11rb::rust_connection::RustConnection;

fn display_env() -> Option<String> {
    std::env::var("DISPLAY")
        .ok()
        .filter(|value| !value.is_empty())
}

fn connect() -> Result<(RustConn, usize), X11Error> {
    if display_env().is_none() {
        return Err(X11Error::NoDisplay);
    }
    x11rb::connect(None).map_err(|err| X11Error::X11(err.to_string()))
}

fn x11_err(err: impl std::fmt::Display) -> X11Error {
    X11Error::X11(err.to_string())
}

fn inject_err(err: impl std::fmt::Display) -> X11Error {
    X11Error::Inject(err.to_string())
}

/// `$DISPLAY` is set and a connection to it succeeds.
pub fn display_available() -> bool {
    connect().is_ok()
}

/// Intern an atom, mapping connection errors to `X11Error`.
fn atom(conn: &RustConn, name: &[u8]) -> Result<Atom, X11Error> {
    conn.intern_atom(false, name)
        .map_err(x11_err)?
        .reply()
        .map(|reply| reply.atom)
        .map_err(x11_err)
}

/// Read a property without trusting its type. Returns `(format, bytes)`; a
/// missing property, an empty value, or an unsupported format yields `None`.
fn property(conn: &RustConn, window: Window, property: Atom) -> Option<(u8, Vec<u8>)> {
    let reply = conn
        .get_property(false, window, property, AtomEnum::ANY, 0, u32::MAX / 4)
        .ok()?
        .reply()
        .ok()?;
    if reply.format == 0 || reply.value.is_empty() {
        return None;
    }
    Some((reply.format, reply.value))
}

/// Decode an 8-bit text property; falls back to Latin-1 for invalid UTF-8.
fn decode_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => bytes.iter().map(|&byte| byte as char).collect(),
    }
}

fn property_text(conn: &RustConn, window: Window, atom: Atom) -> Option<String> {
    match property(conn, window, atom)? {
        (8, bytes) => {
            let text = decode_text(&bytes);
            if text.is_empty() { None } else { Some(text) }
        }
        // Unexpected type/format: skip this property rather than guessing.
        _ => None,
    }
}

fn property_u32(conn: &RustConn, window: Window, atom: Atom) -> Option<u32> {
    match property(conn, window, atom)? {
        (32, bytes) if bytes.len() >= 4 => {
            Some(u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        }
        _ => None,
    }
}

fn property_windows(conn: &RustConn, window: Window, atom: Atom) -> Option<Vec<Window>> {
    let (format, bytes) = property(conn, window, atom)?;
    if format != 32 || bytes.len() % 4 != 0 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|chunk| u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect(),
    )
}

/// Title from `_NET_WM_NAME`, falling back to `WM_NAME`.
fn window_title(conn: &RustConn, window: Window, net_wm_name: Atom) -> Option<String> {
    property_text(conn, window, net_wm_name)
        .or_else(|| property_text(conn, window, AtomEnum::WM_NAME.into()))
}

fn window_wm_class(conn: &RustConn, window: Window) -> Option<String> {
    let (format, bytes) = property(conn, window, AtomEnum::WM_CLASS.into())?;
    if format != 8 {
        return None;
    }
    let parts: Vec<String> = bytes
        .split(|&byte| byte == 0)
        .filter(|part| !part.is_empty())
        .map(decode_text)
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("."))
    }
}

/// List toplevel windows that are viewable, sized, and not owned by `exclude_pid`.
///
/// Windows without a title are only kept when they expose a `WM_CLASS`. The
/// result is sorted case-insensitively by title.
pub fn list_windows(exclude_pid: u32) -> Result<Vec<WindowInfo>, X11Error> {
    let (conn, screen_num) = connect()?;
    let root = conn.setup().roots[screen_num].root;
    let net_client_list = atom(&conn, b"_NET_CLIENT_LIST")?;
    let net_wm_name = atom(&conn, b"_NET_WM_NAME")?;
    let net_wm_pid = atom(&conn, b"_NET_WM_PID")?;

    let candidates: Vec<Window> = match property_windows(&conn, root, net_client_list) {
        Some(windows) if !windows.is_empty() => windows,
        _ => {
            conn.query_tree(root)
                .map_err(x11_err)?
                .reply()
                .map_err(x11_err)?
                .children
        }
    };

    let mut windows = Vec::new();
    for window in candidates {
        let Ok(attrs) = conn.get_window_attributes(window).map_err(x11_err)?.reply() else {
            // Window vanished between listing and inspection.
            continue;
        };
        if attrs.map_state != MapState::VIEWABLE {
            continue;
        }
        let Ok(geometry) = conn.get_geometry(window).map_err(x11_err)?.reply() else {
            continue;
        };
        if geometry.width == 0 || geometry.height == 0 {
            continue;
        }
        let title = window_title(&conn, window, net_wm_name);
        let wm_class = window_wm_class(&conn, window);
        if title.is_none() && wm_class.is_none() {
            continue;
        }
        let pid = property_u32(&conn, window, net_wm_pid);
        if pid == Some(exclude_pid) {
            continue;
        }
        windows.push(WindowInfo {
            xid: window,
            title: title.unwrap_or_default(),
            wm_class: wm_class.unwrap_or_default(),
            width: u32::from(geometry.width),
            height: u32::from(geometry.height),
            pid,
        });
    }

    windows.sort_by(|left, right| {
        left.title
            .to_lowercase()
            .cmp(&right.title.to_lowercase())
            .then_with(|| left.xid.cmp(&right.xid))
    });
    Ok(windows)
}

/// Re-find a previously captured window: exact XID first, then exact title, then
/// `WM_CLASS` together with a title prefix. Returns `None` when nothing matches.
pub fn resolve_window(xid: u32, title: &str, wm_class: &str) -> Option<u32> {
    if xid == 0 && title.is_empty() && wm_class.is_empty() {
        return None;
    }
    let windows = list_windows(0).ok()?;

    if xid != 0 {
        if let Some(window) = windows.iter().find(|window| window.xid == xid) {
            return Some(window.xid);
        }
    }
    if !title.is_empty() {
        if let Some(window) = windows.iter().find(|window| window.title == title) {
            return Some(window.xid);
        }
    }
    if !wm_class.is_empty() {
        let prefix_match = windows.iter().find(|window| {
            window.wm_class == wm_class && (title.is_empty() || window.title.starts_with(title))
        });
        if let Some(window) = prefix_match {
            return Some(window.xid);
        }
        // A title prefix without any recorded class still narrows the search.
        if title.is_empty() {
            return None;
        }
        if let Some(window) = windows
            .iter()
            .find(|window| window.title.starts_with(title))
        {
            return Some(window.xid);
        }
    }
    None
}

/// Ask the window manager to activate `xid` and move input focus to it.
pub fn activate_window(xid: u32) -> Result<(), X11Error> {
    let (conn, screen_num) = connect()?;
    let root = conn.setup().roots[screen_num].root;
    let net_active_window = atom(&conn, b"_NET_ACTIVE_WINDOW")?;

    let event = ClientMessageEvent::new(
        32,
        xid,
        net_active_window,
        ClientMessageData::from([2u32, CURRENT_TIME, 0, 0, 0]),
    );
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event,
    )
    .map_err(x11_err)?;
    conn.set_input_focus(InputFocus::PARENT, xid, CURRENT_TIME)
        .map_err(x11_err)?;
    conn.flush().map_err(x11_err)?;
    Ok(())
}

struct KeyboardMap {
    keysyms: Vec<u32>,
    per_keycode: usize,
    min_keycode: u8,
}

impl KeyboardMap {
    fn keycode_for(&self, keysym: u32) -> Option<(u8, bool)> {
        for (index, columns) in self.keysyms.chunks_exact(self.per_keycode).enumerate() {
            let keycode = self.min_keycode.checked_add(index as u8)?;
            if columns.first() == Some(&keysym) {
                return Some((keycode, false));
            }
            if columns.get(1) == Some(&keysym) {
                return Some((keycode, true));
            }
        }
        None
    }

    fn shift_keycode(&self) -> u8 {
        self.keycode_for(SHIFT_L)
            .map(|(keycode, _)| keycode)
            .unwrap_or(FALLBACK_SHIFT_KEYCODE)
    }
}

fn keyboard_map(conn: &RustConn) -> Result<KeyboardMap, X11Error> {
    let setup = conn.setup();
    let min_keycode = setup.min_keycode;
    let max_keycode = setup.max_keycode;
    let count = max_keycode
        .checked_sub(min_keycode)
        .map(|span| span.saturating_add(1))
        .unwrap_or(1);
    let reply = conn
        .get_keyboard_mapping(min_keycode, count)
        .map_err(x11_err)?
        .reply()
        .map_err(x11_err)?;
    let per_keycode = usize::from(reply.keysyms_per_keycode);
    if per_keycode == 0 {
        return Err(X11Error::Inject("empty keyboard mapping".to_string()));
    }
    Ok(KeyboardMap {
        keysyms: reply.keysyms,
        per_keycode,
        min_keycode,
    })
}

fn fake_key(conn: &RustConn, root: Window, keycode: u8, press: bool) -> Result<(), X11Error> {
    let type_ = if press {
        KEY_PRESS_EVENT
    } else {
        KEY_RELEASE_EVENT
    };
    conn.xtest_fake_input(type_, keycode, CURRENT_TIME, root, 0, 0, 0)
        .map_err(inject_err)?;
    conn.flush().map_err(inject_err)?;
    Ok(())
}

/// Inject a keysym with XTest. When `xid` is given, that window is activated
/// first and the injection waits 80 ms so the presentation can take focus.
///
/// Keysyms reachable only through the shifted column are sent with a synthetic
/// Shift press/release around the real key; presses and releases are 30 ms apart.
pub fn send_key(xid: Option<u32>, keysym: u32) -> Result<(), X11Error> {
    if let Some(xid) = xid {
        activate_window(xid)?;
        sleep(ACTIVATE_SETTLE);
    }

    let (conn, screen_num) = connect()?;
    let root = conn.setup().roots[screen_num].root;
    let map = keyboard_map(&conn)?;
    let (keycode, needs_shift) = map
        .keycode_for(keysym)
        .ok_or_else(|| X11Error::Inject(format!("keysym {keysym:#x} not in keyboard mapping")))?;
    let shift = needs_shift.then(|| map.shift_keycode());

    if let Some(shift) = shift {
        fake_key(&conn, root, shift, true)?;
        sleep(KEY_GAP);
    }
    fake_key(&conn, root, keycode, true)?;
    sleep(KEY_GAP);
    fake_key(&conn, root, keycode, false)?;
    if let Some(shift) = shift {
        sleep(KEY_GAP);
        fake_key(&conn, root, shift, false)?;
    }
    Ok(())
}

/// X11 keysym for one of the deck key names (`right`, `pagedown`, `f5`, …).
pub fn keysym_from_name(name: &str) -> Option<u32> {
    crate::model::deck::keysym(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysym_names_reuse_the_deck_mapping() {
        assert_eq!(keysym_from_name("right"), Some(0xFF53));
        assert_eq!(keysym_from_name("space"), Some(0x20));
        assert_eq!(keysym_from_name("bogus"), None);
    }

    #[test]
    fn empty_resolve_criteria_short_circuit() {
        assert_eq!(resolve_window(0, "", ""), None);
    }
}
