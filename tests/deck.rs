//! Deck control: keysym mapping and error surfacing without a display.

use signifiage::model::deck::{self, DECK_KEYS};
use signifiage::services::deck::{DeckBackend, DeckController, DeckError, keysym_for};
use signifiage::services::x11;

#[test]
fn keysym_for_maps_every_documented_key() {
    assert_eq!(keysym_for("right"), Some(0xFF53));
    assert_eq!(keysym_for("left"), Some(0xFF51));
    assert_eq!(keysym_for("pagedown"), Some(0xFF56));
    assert_eq!(keysym_for("pageup"), Some(0xFF55));
    assert_eq!(keysym_for("down"), Some(0xFF54));
    assert_eq!(keysym_for("up"), Some(0xFF52));
    assert_eq!(keysym_for("space"), Some(0x20));
    assert_eq!(keysym_for("f5"), Some(0xFFC2));
    for key in DECK_KEYS {
        assert_eq!(keysym_for(key), deck::keysym(key), "{key}");
    }
    assert_eq!(keysym_for("escape"), None);
}

#[test]
fn backend_ids_round_trip() {
    for backend in [DeckBackend::X11, DeckBackend::Portal] {
        assert_eq!(DeckBackend::from_id(backend.id()), Some(backend));
    }
    assert_eq!(DeckBackend::from_id(""), None);
    assert_eq!(DeckBackend::from_id("wayland"), None);
}

#[test]
fn x11_availability_follows_the_display_connection() {
    let controller = DeckController::new(DeckBackend::X11);
    assert_eq!(controller.available(), x11::display_available());
    assert!(!controller.hint().is_empty());
}

#[test]
fn portal_start_is_optimistic_and_localised() {
    let controller = DeckController::new(DeckBackend::Portal);
    assert!(controller.available());
    assert!(!controller.hint().is_empty());
    assert!(controller.last_error().is_none());
}

#[test]
fn x11_without_a_display_reports_an_error_instead_of_panicking() {
    if x11::display_available() {
        // A real X session is present; injecting a key would hit the deskop.
        return;
    }
    let controller = DeckController::new(DeckBackend::X11);
    assert!(!controller.available());

    let err = controller
        .next(0xFF53, None)
        .expect_err("next must fail without a display");
    assert!(matches!(err, DeckError::NoDisplay));
    let surfaced = controller
        .last_error()
        .expect("last_error records the failure");
    assert!(!surfaced.is_empty());

    controller.clear_error();
    assert!(controller.last_error().is_none());

    let prev = controller.prev(0xFF51, Some(0x42));
    assert!(matches!(prev, Err(DeckError::NoDisplay)));
}

#[test]
fn deck_errors_have_actionable_text() {
    for err in [
        DeckError::NoDisplay,
        DeckError::NoTarget,
        DeckError::Portal("denied".to_string()),
        DeckError::X11("connection refused".to_string()),
    ] {
        assert!(!err.to_string().is_empty());
        assert!(!err.message().is_empty());
    }
}
