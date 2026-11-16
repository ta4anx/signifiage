//! Teleprompter text and display settings (scroll state is runtime only).

use serde::{Deserialize, Serialize};

use crate::model::layer::Rgba;

/// Speed limits in design pixels per second.
pub const MIN_SPEED: f64 = 10.0;
pub const MAX_SPEED: f64 = 200.0;
/// Font size limits in design pixels.
pub const MIN_FONT_SIZE: f64 = 8.0;
pub const MAX_FONT_SIZE: f64 = 512.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prompter {
    pub text: String,
    pub font_family: String,
    pub font_size: f64,
    pub line_spacing: f64,
    /// Scroll speed, pixels per second.
    pub speed: f64,
    /// Text column width as a percentage of the output width.
    pub width_pct: f64,
    pub mirror_h: bool,
    pub mirror_v: bool,
    pub fg: String,
    pub bg: String,
}

impl Default for Prompter {
    fn default() -> Self {
        Self {
            text: String::new(),
            font_family: "Sans".to_string(),
            font_size: 64.0,
            line_spacing: 1.3,
            speed: 45.0,
            width_pct: 70.0,
            mirror_h: false,
            mirror_v: false,
            fg: "#F8FAFC".to_string(),
            bg: "#000000".to_string(),
        }
    }
}

impl Prompter {
    pub fn normalize(&mut self) {
        if self.font_family.trim().is_empty() {
            self.font_family = "Sans".to_string();
        }
        if !self.font_size.is_finite() {
            self.font_size = 64.0;
        }
        self.font_size = self.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if !self.line_spacing.is_finite() || self.line_spacing < 1.0 {
            self.line_spacing = 1.3;
        }
        self.line_spacing = self.line_spacing.clamp(1.0, 4.0);
        if !self.speed.is_finite() {
            self.speed = 45.0;
        }
        self.speed = self.speed.clamp(MIN_SPEED, MAX_SPEED);
        if !self.width_pct.is_finite() {
            self.width_pct = 70.0;
        }
        self.width_pct = self.width_pct.clamp(10.0, 100.0);
        self.fg = Rgba::normalize_hex(&self.fg);
        self.bg = Rgba::normalize_hex(&self.bg);
    }

    pub fn fg_rgba(&self) -> Rgba {
        Rgba::parse(&self.fg)
    }

    pub fn bg_rgba(&self) -> Rgba {
        Rgba::parse(&self.bg)
    }

    pub fn has_script(&self) -> bool {
        !self.text.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_schema() {
        let p = Prompter::default();
        assert_eq!(p.font_size, 64.0);
        assert_eq!(p.speed, 45.0);
        assert_eq!(p.width_pct, 70.0);
        assert!(!p.mirror_h && !p.mirror_v);
        assert!(!p.has_script());
    }

    #[test]
    fn normalize_clamps_ranges() {
        let mut p = Prompter {
            speed: 9999.0,
            font_size: 1.0,
            width_pct: 5.0,
            line_spacing: 0.2,
            fg: "bad".into(),
            ..Prompter::default()
        };
        p.normalize();
        assert_eq!(p.speed, MAX_SPEED);
        assert_eq!(p.font_size, MIN_FONT_SIZE);
        assert_eq!(p.width_pct, 10.0);
        assert_eq!(p.line_spacing, 1.3);
        assert_eq!(p.fg, "#FFFFFF");
    }
}
