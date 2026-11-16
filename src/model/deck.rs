//! Deck control: which keys are sent to the focused presentation.

use serde::{Deserialize, Serialize};

/// Keys accepted for deck next/previous.
pub const DECK_KEYS: [&str; 8] = [
    "right", "left", "pagedown", "pageup", "down", "up", "space", "f5",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Deck {
    pub next_key: String,
    pub prev_key: String,
}

impl Default for Deck {
    fn default() -> Self {
        Self {
            next_key: "right".to_string(),
            prev_key: "left".to_string(),
        }
    }
}

impl Deck {
    pub fn normalize(&mut self) {
        if keysym(&self.next_key).is_none() {
            self.next_key = "right".to_string();
        }
        if keysym(&self.prev_key).is_none() {
            self.prev_key = "left".to_string();
        }
    }

    pub fn next_keysym(&self) -> u32 {
        keysym(&self.next_key).unwrap_or(0xFF53)
    }

    pub fn prev_keysym(&self) -> u32 {
        keysym(&self.prev_key).unwrap_or(0xFF51)
    }
}

/// X11 keysym for a stored key name.
pub fn keysym(key: &str) -> Option<u32> {
    match key {
        "right" => Some(0xFF53),
        "left" => Some(0xFF51),
        "pagedown" => Some(0xFF56),
        "pageup" => Some(0xFF55),
        "down" => Some(0xFF54),
        "up" => Some(0xFF52),
        "space" => Some(0x20),
        "f5" => Some(0xFFC2),
        _ => None,
    }
}

/// Localisation key for a stored key name.
pub fn key_label_key(key: &str) -> &'static str {
    match key {
        "right" => "deck.key.right",
        "left" => "deck.key.left",
        "pagedown" => "deck.key.pagedown",
        "pageup" => "deck.key.pageup",
        "down" => "deck.key.down",
        "up" => "deck.key.up",
        "space" => "deck.key.space",
        "f5" => "deck.key.f5",
        _ => "deck.key.right",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_right_and_left() {
        let deck = Deck::default();
        assert_eq!(deck.next_keysym(), 0xFF53);
        assert_eq!(deck.prev_keysym(), 0xFF51);
    }

    #[test]
    fn invalid_keys_normalize_to_defaults() {
        let mut deck = Deck {
            next_key: "nope".into(),
            prev_key: "space".into(),
        };
        deck.normalize();
        assert_eq!(deck.next_key, "right");
        assert_eq!(deck.prev_key, "space");
        assert_eq!(deck.prev_keysym(), 0x20);
    }

    #[test]
    fn every_documented_key_maps_to_a_keysym() {
        for key in DECK_KEYS {
            assert!(keysym(key).is_some(), "{key}");
            assert_ne!(key_label_key(key), "deck.key.right.missing");
        }
    }
}
