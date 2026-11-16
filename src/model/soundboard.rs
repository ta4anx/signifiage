//! Soundboard pages and pads.

use serde::{Deserialize, Serialize};

use crate::model::layer::Rgba;
use crate::model::new_id;

/// Pad palette, cycled when new pads are created.
pub const PALETTE: [&str; 6] = [
    "#334155", "#0F766E", "#7C2D12", "#4C1D95", "#1E3A8A", "#3F6212",
];

/// Hotkeys on the first page, in pad order.
pub const HOTKEYS: [&str; 12] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
];

/// Pads per page.
pub const PADS_PER_PAGE: usize = 12;
/// Pages created with a fresh project.
pub const DEFAULT_PAGES: usize = 3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pad {
    pub id: String,
    pub label: String,
    pub color: String,
    pub asset: Option<String>,
    pub volume: f64,
    pub r#loop: bool,
    pub hotkey: Option<String>,
}

impl Default for Pad {
    fn default() -> Self {
        Self {
            id: new_id(),
            label: String::new(),
            color: PALETTE[0].to_string(),
            asset: None,
            volume: 0.8,
            r#loop: false,
            hotkey: None,
        }
    }
}

impl Pad {
    pub fn new(index: usize, hotkey: Option<String>) -> Self {
        let color = PALETTE[index % PALETTE.len()].to_string();
        Self {
            label: format!("{} {}", rust_i18n::t!("soundboard.pad_prefix"), index + 1),
            color,
            hotkey,
            ..Self::default()
        }
    }

    pub fn rgba(&self) -> Rgba {
        Rgba::parse(&self.color)
    }

    /// A pad can play only once an audio asset is bound.
    pub fn is_armed(&self) -> bool {
        self.asset.as_deref().is_some_and(|a| !a.trim().is_empty())
    }

    pub fn normalize(&mut self) {
        self.color = Rgba::normalize_hex(&self.color);
        if !self.volume.is_finite() {
            self.volume = 0.8;
        }
        self.volume = self.volume.clamp(0.0, 1.0);
        if let Some(hotkey) = &self.hotkey
            && hotkey.trim().is_empty()
        {
            self.hotkey = None;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundboardPage {
    pub id: String,
    pub name: String,
    pub pads: Vec<Pad>,
}

impl Default for SoundboardPage {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: String::new(),
            pads: Vec::new(),
        }
    }
}

impl SoundboardPage {
    /// Page `index` (0-based) with localised name and, for page 0, F1–F12.
    pub fn new(index: usize) -> Self {
        let pads = (0..PADS_PER_PAGE)
            .map(|pad| {
                let hotkey = if index == 0 {
                    Some(HOTKEYS[pad].to_string())
                } else {
                    None
                };
                Pad::new(pad, hotkey)
            })
            .collect();
        Self {
            name: format!("{} {}", rust_i18n::t!("soundboard.page_prefix"), index + 1),
            pads,
            ..Self::default()
        }
    }

    /// Pad index for a hotkey such as `F3`.
    pub fn pad_for_hotkey(&self, hotkey: &str) -> Option<usize> {
        self.pads
            .iter()
            .position(|pad| pad.hotkey.as_deref() == Some(hotkey))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Soundboard {
    pub active_page: usize,
    pub pages: Vec<SoundboardPage>,
}

impl Default for Soundboard {
    fn default() -> Self {
        Self {
            active_page: 0,
            pages: (0..DEFAULT_PAGES).map(SoundboardPage::new).collect(),
        }
    }
}

impl Soundboard {
    pub fn active(&self) -> Option<&SoundboardPage> {
        self.pages.get(self.active_page)
    }

    pub fn active_mut(&mut self) -> Option<&mut SoundboardPage> {
        self.pages.get_mut(self.active_page)
    }

    pub fn pad(&self, pad_id: &str) -> Option<&Pad> {
        self.pages
            .iter()
            .flat_map(|page| page.pads.iter())
            .find(|pad| pad.id == pad_id)
    }

    pub fn pad_mut(&mut self, pad_id: &str) -> Option<&mut Pad> {
        self.pages
            .iter_mut()
            .flat_map(|page| page.pads.iter_mut())
            .find(|pad| pad.id == pad_id)
    }

    pub fn add_page(&mut self) {
        let page = SoundboardPage::new(self.pages.len());
        self.pages.push(page);
        self.active_page = self.pages.len() - 1;
    }

    /// Remove a page; the last page cannot be removed.
    pub fn remove_page(&mut self, index: usize) -> bool {
        if self.pages.len() <= 1 || index >= self.pages.len() {
            return false;
        }
        self.pages.remove(index);
        if self.active_page >= self.pages.len() {
            self.active_page = self.pages.len() - 1;
        }
        true
    }

    /// True when another pad already owns `hotkey`.
    pub fn hotkey_taken(&self, hotkey: &str, except_pad: Option<&str>) -> bool {
        self.pages
            .iter()
            .flat_map(|p| p.pads.iter())
            .any(|pad| pad.hotkey.as_deref() == Some(hotkey) && Some(pad.id.as_str()) != except_pad)
    }

    pub fn normalize(&mut self) {
        if self.pages.is_empty() {
            self.pages = (0..DEFAULT_PAGES).map(SoundboardPage::new).collect();
        }
        if self.active_page >= self.pages.len() {
            self.active_page = 0;
        }
        for page in &mut self.pages {
            for pad in &mut page.pads {
                pad.normalize();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_have_three_pages_of_twelve_pads() {
        let board = Soundboard::default();
        assert_eq!(board.pages.len(), 3);
        for page in &board.pages {
            assert_eq!(page.pads.len(), 12);
        }
        assert_eq!(board.pages[0].pads[0].hotkey.as_deref(), Some("F1"));
        assert_eq!(board.pages[0].pads[11].hotkey.as_deref(), Some("F12"));
        assert!(board.pages[1].pads.iter().all(|p| p.hotkey.is_none()));
        assert_eq!(board.pages[0].pads[0].label, "Pad 1");
        assert!(!board.pages[0].pads[0].is_armed());
    }

    #[test]
    fn pages_add_remove_and_clamp_active() {
        let mut board = Soundboard::default();
        board.add_page();
        assert_eq!(board.pages.len(), 4);
        assert_eq!(board.active_page, 3);
        assert!(board.remove_page(3));
        assert_eq!(board.active_page, 2);
        while board.pages.len() > 1 {
            board.remove_page(0);
        }
        assert!(!board.remove_page(0));
    }

    #[test]
    fn hotkey_conflicts_are_detected() {
        let board = Soundboard::default();
        assert!(board.hotkey_taken("F1", None));
        let first_pad = board.pages[0].pads[0].id.clone();
        assert!(!board.hotkey_taken("F1", Some(&first_pad)));
        assert!(board.hotkey_taken("F1", Some("other")));
    }

    #[test]
    fn pad_volume_and_colour_normalize() {
        let mut pad = Pad::new(0, None);
        pad.volume = 5.0;
        pad.color = "nope".into();
        pad.hotkey = Some("  ".into());
        pad.normalize();
        assert_eq!(pad.volume, 1.0);
        assert_eq!(pad.color, "#FFFFFF");
        assert!(pad.hotkey.is_none());
    }
}
