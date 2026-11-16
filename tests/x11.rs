use std::process::Command;

use signifiage::services::x11::{
    X11Error, activate_window, display_available, keysym_from_name, list_windows, resolve_window,
    send_key,
};

const CHILD_MARKER: &str = "SIGNIFIAGE_X11_NO_DISPLAY_CHILD";

#[test]
fn keysym_from_name_matches_deck_keys() {
    assert_eq!(keysym_from_name("right"), Some(0xFF53));
    assert_eq!(keysym_from_name("left"), Some(0xFF51));
    assert_eq!(keysym_from_name("space"), Some(0x20));
    assert_eq!(keysym_from_name("not-a-key"), None);
}

#[test]
fn resolve_window_requires_some_criteria() {
    assert_eq!(resolve_window(0, "", ""), None);
}

#[test]
fn list_windows_is_well_formed() {
    if !display_available() {
        eprintln!("no DISPLAY; list_windows shape not exercised");
        return;
    }
    let windows = list_windows(std::process::id()).expect("list windows with a live display");
    eprintln!("listed {} X11 windows", windows.len());
    for window in &windows {
        assert_ne!(window.xid, 0);
        assert!(window.width > 0 && window.height > 0);
        assert!(
            !window.title.is_empty() || !window.wm_class.is_empty(),
            "window {:#x} has neither title nor WM_CLASS",
            window.xid
        );
        assert_ne!(window.pid, Some(std::process::id()));
    }
    let mut sorted = windows.clone();
    sorted.sort_by(|a, b| {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.xid.cmp(&b.xid))
    });
    assert_eq!(
        windows.iter().map(|w| w.xid).collect::<Vec<_>>(),
        sorted.iter().map(|w| w.xid).collect::<Vec<_>>(),
        "windows are sorted by title"
    );
}

#[test]
fn unknown_keysym_reports_inject_error() {
    if !display_available() {
        eprintln!("no DISPLAY; key injection not exercised");
        return;
    }
    match send_key(None, 0xFFFF_FFFF) {
        Err(X11Error::Inject(message)) => assert!(message.contains("keysym")),
        other => panic!("expected an inject error, got {other:?}"),
    }
}

#[test]
fn no_display_reports_no_display() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        assert!(!display_available());
        assert!(matches!(list_windows(0), Err(X11Error::NoDisplay)));
        assert!(matches!(activate_window(0), Err(X11Error::NoDisplay)));
        assert!(matches!(
            send_key(Some(0), 0xFF53),
            Err(X11Error::NoDisplay)
        ));
        return;
    }

    // Run this test again in a child process with DISPLAY removed, so the check
    // never mutates the parent test process' environment.
    let exe = std::env::current_exe().expect("test executable");
    let output = Command::new(exe)
        .args(["--exact", "no_display_reports_no_display", "--nocapture"])
        .env(CHILD_MARKER, "1")
        .env_remove("DISPLAY")
        .output()
        .expect("spawn child test process");
    assert!(
        output.status.success(),
        "child without DISPLAY failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
