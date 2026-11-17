//! Layers and their per-kind properties.

use serde::{Deserialize, Serialize};

use crate::model::new_id;

/// Straight RGBA colour, 0–255 per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const WHITE: Rgba = Rgba {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    pub const BLACK: Rgba = Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };

    /// Parse `#RRGGBB` or `#RRGGBBAA` (case-insensitive, `#` optional).
    /// Anything else falls back to opaque white.
    pub fn parse(value: &str) -> Rgba {
        let hex = value.trim().trim_start_matches('#');
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        if hex.len() == 6 || hex.len() == 8 {
            if let (Some(r), Some(g), Some(b)) = (byte(0), byte(2), byte(4)) {
                let a = if hex.len() == 8 { byte(6) } else { Some(255) };
                if let Some(a) = a {
                    return Rgba { r, g, b, a };
                }
            }
        }
        Rgba::WHITE
    }

    /// Normalise a colour string for storage; invalid input becomes `#FFFFFF`.
    pub fn normalize_hex(value: &str) -> String {
        let hex = value.trim().trim_start_matches('#');
        let valid =
            (hex.len() == 6 || hex.len() == 8) && hex.chars().all(|c| c.is_ascii_hexdigit());
        if valid {
            format!("#{}", hex.to_ascii_uppercase())
        } else {
            "#FFFFFF".to_string()
        }
    }

    pub fn to_hex(self) -> String {
        if self.a == 255 {
            format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
        } else {
            format!("#{:02X}{:02X}{:02X}{:02X}", self.r, self.g, self.b, self.a)
        }
    }
}

/// Layer rectangle in design pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Default for Rect {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        }
    }
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn is_finite(&self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.w.is_finite() && self.h.is_finite()
    }

    /// Clamp size/position into a design canvas, keeping a 20x20 minimum.
    pub fn clamp_to(&mut self, design_w: f64, design_h: f64) {
        const MIN: f64 = 20.0;
        if !self.is_finite() {
            *self = Rect::default();
        }
        self.w = self.w.clamp(MIN, design_w.max(MIN));
        self.h = self.h.clamp(MIN, design_h.max(MIN));
        self.x = self.x.clamp(0.0, (design_w - self.w).max(0.0));
        self.y = self.y.clamp(0.0, (design_h - self.h).max(0.0));
    }

    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

impl TextAlign {
    pub fn id(self) -> &'static str {
        match self {
            TextAlign::Left => "left",
            TextAlign::Center => "center",
            TextAlign::Right => "right",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "left" => Some(TextAlign::Left),
            "center" => Some(TextAlign::Center),
            "right" => Some(TextAlign::Right),
            _ => None,
        }
    }
}

/// Shared typography for every text-bearing layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextStyle {
    pub family: String,
    pub size: f64,
    pub bold: bool,
    pub italic: bool,
    pub color: String,
    pub align: TextAlign,
    pub line_spacing: f64,
    pub uppercase: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            family: "Sans".to_string(),
            size: 64.0,
            bold: false,
            italic: false,
            color: "#F8FAFC".to_string(),
            align: TextAlign::Left,
            line_spacing: 1.1,
            uppercase: false,
        }
    }
}

impl TextStyle {
    /// Clamp numeric ranges and normalise the colour string.
    pub fn normalize(&mut self) {
        if self.family.trim().is_empty() {
            self.family = "Sans".to_string();
        }
        if !self.size.is_finite() || self.size <= 0.0 {
            self.size = 64.0;
        }
        self.size = self.size.clamp(4.0, 4096.0);
        if !self.line_spacing.is_finite() || self.line_spacing < 1.0 {
            self.line_spacing = 1.1;
        }
        self.line_spacing = self.line_spacing.clamp(1.0, 4.0);
        self.color = Rgba::normalize_hex(&self.color);
    }

    pub fn rgba(&self) -> Rgba {
        Rgba::parse(&self.color)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fit {
    #[default]
    Contain,
    Cover,
    Stretch,
}

impl Fit {
    pub fn id(self) -> &'static str {
        match self {
            Fit::Contain => "contain",
            Fit::Cover => "cover",
            Fit::Stretch => "stretch",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "contain" => Some(Fit::Contain),
            "cover" => Some(Fit::Cover),
            "stretch" => Some(Fit::Stretch),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockPreset {
    #[default]
    H24,
    #[serde(rename = "24h_seconds")]
    H24Seconds,
    #[serde(rename = "12h")]
    H12,
    #[serde(rename = "12h_seconds")]
    H12Seconds,
}

impl ClockPreset {
    pub fn id(self) -> &'static str {
        match self {
            ClockPreset::H24 => "24h",
            ClockPreset::H24Seconds => "24h_seconds",
            ClockPreset::H12 => "12h",
            ClockPreset::H12Seconds => "12h_seconds",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "24h" => Some(ClockPreset::H24),
            "24h_seconds" => Some(ClockPreset::H24Seconds),
            "12h" => Some(ClockPreset::H12),
            "12h_seconds" => Some(ClockPreset::H12Seconds),
            _ => None,
        }
    }

    pub fn show_seconds(self) -> bool {
        matches!(self, ClockPreset::H24Seconds | ClockPreset::H12Seconds)
    }

    pub fn is_12h(self) -> bool {
        matches!(self, ClockPreset::H12 | ClockPreset::H12Seconds)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerMode {
    #[default]
    Elapsed,
    Remaining,
    EndAt,
}

impl TimerMode {
    pub fn id(self) -> &'static str {
        match self {
            TimerMode::Elapsed => "elapsed",
            TimerMode::Remaining => "remaining",
            TimerMode::EndAt => "end_at",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "elapsed" => Some(TimerMode::Elapsed),
            "remaining" => Some(TimerMode::Remaining),
            "end_at" => Some(TimerMode::EndAt),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PatternKind {
    #[default]
    Grid,
    Zebra,
    Colorbars,
    Focus,
}

impl PatternKind {
    pub const ALL: [PatternKind; 4] = [
        PatternKind::Grid,
        PatternKind::Zebra,
        PatternKind::Colorbars,
        PatternKind::Focus,
    ];

    pub fn id(self) -> &'static str {
        match self {
            PatternKind::Grid => "grid",
            PatternKind::Zebra => "zebra",
            PatternKind::Colorbars => "colorbars",
            PatternKind::Focus => "focus",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "grid" => Some(PatternKind::Grid),
            "zebra" => Some(PatternKind::Zebra),
            "colorbars" => Some(PatternKind::Colorbars),
            "focus" => Some(PatternKind::Focus),
            _ => None,
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            PatternKind::Grid => "pattern.grid",
            PatternKind::Zebra => "pattern.zebra",
            PatternKind::Colorbars => "pattern.colorbars",
            PatternKind::Focus => "pattern.focus",
        }
    }
}

/// Capture source description stored in a capture layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CaptureSource {
    Camera {
        #[serde(default)]
        device: String,
        #[serde(default)]
        label: String,
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
        #[serde(default)]
        fps: Option<u32>,
    },
    X11Window {
        #[serde(default)]
        title: String,
        #[serde(default)]
        wm_class: String,
        #[serde(default)]
        xid: u32,
    },
    Portal {
        #[serde(default)]
        label: String,
        #[serde(default)]
        restore_token: Option<String>,
    },
}

impl Default for CaptureSource {
    fn default() -> Self {
        CaptureSource::Camera {
            device: String::new(),
            label: String::new(),
            width: None,
            height: None,
            fps: None,
        }
    }
}

impl CaptureSource {
    pub fn kind_id(&self) -> &'static str {
        match self {
            CaptureSource::Camera { .. } => "camera",
            CaptureSource::X11Window { .. } => "x11_window",
            CaptureSource::Portal { .. } => "portal",
        }
    }

    /// Human label shown in the inspector, render placeholders and the web state.
    pub fn label(&self) -> String {
        match self {
            CaptureSource::Camera { device, label, .. } => {
                if !label.trim().is_empty() {
                    label.clone()
                } else if !device.trim().is_empty() {
                    device.clone()
                } else {
                    rust_i18n::t!("capture.unset").to_string()
                }
            }
            CaptureSource::X11Window {
                title,
                wm_class,
                xid,
            } => {
                if !title.trim().is_empty() {
                    title.clone()
                } else if !wm_class.trim().is_empty() {
                    wm_class.clone()
                } else if *xid != 0 {
                    format!("X11 0x{xid:x}")
                } else {
                    rust_i18n::t!("capture.unset").to_string()
                }
            }
            CaptureSource::Portal { label, .. } => {
                if !label.trim().is_empty() {
                    label.clone()
                } else {
                    rust_i18n::t!("capture.unset").to_string()
                }
            }
        }
    }

    /// Stable identity used to decide whether a pipeline can be kept.
    pub fn pipeline_key(&self) -> String {
        match self {
            CaptureSource::Camera {
                device,
                width,
                height,
                fps,
                ..
            } => {
                format!("camera|{device}|{width:?}|{height:?}|{fps:?}")
            }
            CaptureSource::X11Window { xid, .. } => format!("x11|{xid}"),
            CaptureSource::Portal { label, .. } => format!("portal|{label}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextProps {
    pub text: String,
    pub style: TextStyle,
    pub background: Option<String>,
    pub padding: f64,
}

impl Default for TextProps {
    fn default() -> Self {
        Self {
            text: String::new(),
            style: TextStyle::default(),
            background: None,
            padding: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClockProps {
    pub preset: ClockPreset,
    pub show_date: bool,
    pub style: TextStyle,
}

impl Default for ClockProps {
    fn default() -> Self {
        Self {
            preset: ClockPreset::H24,
            show_date: false,
            style: TextStyle {
                size: 96.0,
                ..TextStyle::default()
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimerProps {
    pub timer_id: String,
    pub mode: TimerMode,
    pub style: TextStyle,
    pub warning_color: String,
}

impl Default for TimerProps {
    fn default() -> Self {
        Self {
            timer_id: "show".to_string(),
            mode: TimerMode::Remaining,
            style: TextStyle {
                size: 96.0,
                ..TextStyle::default()
            },
            warning_color: "#F97316".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EtaProps {
    pub label: String,
    pub style: TextStyle,
}

impl Default for EtaProps {
    fn default() -> Self {
        Self {
            label: String::new(),
            style: TextStyle {
                size: 48.0,
                ..TextStyle::default()
            },
        }
    }
}

impl EtaProps {
    /// Localised default label when the project left it blank.
    pub fn display_label(&self) -> String {
        if self.label.trim().is_empty() {
            rust_i18n::t!("layer.eta.default_label").to_string()
        } else {
            self.label.clone()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageProps {
    pub asset: Option<String>,
    pub fit: Fit,
}

impl Default for ImageProps {
    fn default() -> Self {
        Self {
            asset: None,
            fit: Fit::Contain,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoProps {
    pub asset: Option<String>,
    pub fit: Fit,
    pub r#loop: bool,
    pub volume: f64,
}

impl Default for VideoProps {
    fn default() -> Self {
        Self {
            asset: None,
            fit: Fit::Contain,
            r#loop: false,
            volume: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureProps {
    pub source: CaptureSource,
    pub fit: Fit,
    pub mirror: bool,
}

impl Default for CaptureProps {
    fn default() -> Self {
        Self {
            source: CaptureSource::default(),
            fit: Fit::Contain,
            mirror: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueProps {
    pub title: String,
    pub empty_text: String,
    pub upcoming: u32,
    pub show_ticket: bool,
    pub show_note: bool,
    /// Show the previously finished call at the bottom (tracking).
    pub show_last_called: bool,
    /// Show who is next / who is waiting (privacy switch hides names).
    pub show_next: bool,
    pub accent: String,
    pub style: TextStyle,
}

impl Default for QueueProps {
    fn default() -> Self {
        Self {
            title: String::new(),
            empty_text: String::new(),
            upcoming: 4,
            show_ticket: true,
            show_note: true,
            show_last_called: true,
            show_next: true,
            accent: "#22C55E".to_string(),
            style: TextStyle {
                size: 72.0,
                align: TextAlign::Center,
                bold: true,
                ..TextStyle::default()
            },
        }
    }
}

impl QueueProps {
    pub fn display_title(&self) -> String {
        if self.title.trim().is_empty() {
            rust_i18n::t!("queue.default_title").to_string()
        } else {
            self.title.clone()
        }
    }

    pub fn display_empty(&self) -> String {
        if self.empty_text.trim().is_empty() {
            rust_i18n::t!("queue.display_empty").to_string()
        } else {
            self.empty_text.clone()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TickerProps {
    pub text: String,
    pub speed: f64,
    pub gap: f64,
    pub style: TextStyle,
    pub background: Option<String>,
}

impl Default for TickerProps {
    fn default() -> Self {
        Self {
            text: String::new(),
            speed: 120.0,
            gap: 120.0,
            style: TextStyle {
                size: 56.0,
                ..TextStyle::default()
            },
            background: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PatternProps {
    pub pattern: PatternKind,
    pub color: String,
}

impl Default for PatternProps {
    fn default() -> Self {
        Self {
            pattern: PatternKind::Grid,
            color: "#F8FAFC".to_string(),
        }
    }
}

/// The `type` + `props` pair of a layer, flattened into the layer object.
///
/// `props` is optional on load: a layer without it gets that kind's defaults.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", content = "props", rename_all = "snake_case")]
pub enum LayerKind {
    Text(TextProps),
    Clock(ClockProps),
    Timer(TimerProps),
    Eta(EtaProps),
    Image(ImageProps),
    Video(VideoProps),
    Capture(CaptureProps),
    Queue(QueueProps),
    Ticker(TickerProps),
    Pattern(PatternProps),
}

impl<'de> Deserialize<'de> for LayerKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct KindVisitor;

        impl<'de> serde::de::Visitor<'de> for KindVisitor {
            type Value = LayerKind;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a layer kind with an optional props object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<LayerKind, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut kind: Option<String> = None;
                let mut props: Option<serde_json::Value> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "type" => kind = Some(map.next_value()?),
                        "props" => props = Some(map.next_value()?),
                        _ => {
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                let kind = kind.ok_or_else(|| serde::de::Error::missing_field("type"))?;
                LayerKind::from_parts(&kind, props).map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_map(KindVisitor)
    }
}

impl LayerKind {
    /// Build a kind from its stored `type` id and optional `props` object.
    pub fn from_parts(kind: &str, props: Option<serde_json::Value>) -> Result<Self, String> {
        fn props_or_default<T: serde::de::DeserializeOwned + Default>(
            props: Option<serde_json::Value>,
        ) -> Result<T, String> {
            match props {
                Some(serde_json::Value::Null) | None => Ok(T::default()),
                Some(value) => serde_json::from_value(value).map_err(|err| err.to_string()),
            }
        }
        Ok(match kind {
            "text" => LayerKind::Text(props_or_default(props)?),
            "clock" => LayerKind::Clock(props_or_default(props)?),
            "timer" => LayerKind::Timer(props_or_default(props)?),
            "eta" => LayerKind::Eta(props_or_default(props)?),
            "image" => LayerKind::Image(props_or_default(props)?),
            "video" => LayerKind::Video(props_or_default(props)?),
            "capture" => LayerKind::Capture(props_or_default(props)?),
            "queue" => LayerKind::Queue(props_or_default(props)?),
            "ticker" => LayerKind::Ticker(props_or_default(props)?),
            "pattern" => LayerKind::Pattern(props_or_default(props)?),
            other => return Err(format!("unknown layer type: {other}")),
        })
    }

    pub fn type_id(&self) -> &'static str {
        match self {
            LayerKind::Text(_) => "text",
            LayerKind::Clock(_) => "clock",
            LayerKind::Timer(_) => "timer",
            LayerKind::Eta(_) => "eta",
            LayerKind::Image(_) => "image",
            LayerKind::Video(_) => "video",
            LayerKind::Capture(_) => "capture",
            LayerKind::Queue(_) => "queue",
            LayerKind::Ticker(_) => "ticker",
            LayerKind::Pattern(_) => "pattern",
        }
    }

    pub fn label_key(&self) -> &'static str {
        match self {
            LayerKind::Text(_) => "layer.type.text",
            LayerKind::Clock(_) => "layer.type.clock",
            LayerKind::Timer(_) => "layer.type.timer",
            LayerKind::Eta(_) => "layer.type.eta",
            LayerKind::Image(_) => "layer.type.image",
            LayerKind::Video(_) => "layer.type.video",
            LayerKind::Capture(_) => "layer.type.capture",
            LayerKind::Queue(_) => "layer.type.queue",
            LayerKind::Ticker(_) => "layer.type.ticker",
            LayerKind::Pattern(_) => "layer.type.pattern",
        }
    }

    pub fn style(&self) -> Option<&TextStyle> {
        match self {
            LayerKind::Text(p) => Some(&p.style),
            LayerKind::Clock(p) => Some(&p.style),
            LayerKind::Timer(p) => Some(&p.style),
            LayerKind::Eta(p) => Some(&p.style),
            LayerKind::Queue(p) => Some(&p.style),
            LayerKind::Ticker(p) => Some(&p.style),
            _ => None,
        }
    }

    pub fn style_mut(&mut self) -> Option<&mut TextStyle> {
        match self {
            LayerKind::Text(p) => Some(&mut p.style),
            LayerKind::Clock(p) => Some(&mut p.style),
            LayerKind::Timer(p) => Some(&mut p.style),
            LayerKind::Eta(p) => Some(&mut p.style),
            LayerKind::Queue(p) => Some(&mut p.style),
            LayerKind::Ticker(p) => Some(&mut p.style),
            _ => None,
        }
    }

    /// Relative asset path referenced by the layer, if any.
    pub fn asset(&self) -> Option<&str> {
        match self {
            LayerKind::Image(p) => p.asset.as_deref(),
            LayerKind::Video(p) => p.asset.as_deref(),
            _ => None,
        }
    }

    pub fn set_asset(&mut self, path: Option<String>) {
        match self {
            LayerKind::Image(p) => p.asset = path,
            LayerKind::Video(p) => p.asset = path,
            _ => {}
        }
    }

    pub fn capture(&self) -> Option<&CaptureProps> {
        match self {
            LayerKind::Capture(p) => Some(p),
            _ => None,
        }
    }

    pub fn set_capture_source(&mut self, source: CaptureSource) {
        if let LayerKind::Capture(p) = self {
            p.source = source;
        }
    }

    /// Clamp every numeric field into its documented range.
    pub fn normalize(&mut self) {
        match self {
            LayerKind::Text(p) => {
                p.style.normalize();
                if let Some(bg) = &p.background {
                    p.background = Some(Rgba::normalize_hex(bg));
                }
                if !p.padding.is_finite() || p.padding < 0.0 {
                    p.padding = 0.0;
                }
            }
            LayerKind::Clock(p) => p.style.normalize(),
            LayerKind::Timer(p) => {
                p.style.normalize();
                p.warning_color = Rgba::normalize_hex(&p.warning_color);
            }
            LayerKind::Eta(p) => p.style.normalize(),
            LayerKind::Image(_) => {}
            LayerKind::Video(p) => {
                if !p.volume.is_finite() {
                    p.volume = 1.0;
                }
                p.volume = p.volume.clamp(0.0, 1.0);
            }
            LayerKind::Capture(_) => {}
            LayerKind::Queue(p) => {
                p.style.normalize();
                p.accent = Rgba::normalize_hex(&p.accent);
                p.upcoming = p.upcoming.min(50);
            }
            LayerKind::Ticker(p) => {
                p.style.normalize();
                if !p.speed.is_finite() || p.speed <= 0.0 {
                    p.speed = 120.0;
                }
                if !p.gap.is_finite() || p.gap < 0.0 {
                    p.gap = 120.0;
                }
                if let Some(bg) = &p.background {
                    p.background = Some(Rgba::normalize_hex(bg));
                }
            }
            LayerKind::Pattern(p) => p.color = Rgba::normalize_hex(&p.color),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub id: String,
    pub name: String,
    pub visible: bool,
    pub opacity: f64,
    pub rect: Rect,
    #[serde(flatten)]
    pub kind: LayerKind,
}

impl Layer {
    pub fn new(kind: LayerKind, name: impl Into<String>, rect: Rect) -> Self {
        Self {
            id: new_id(),
            name: name.into(),
            visible: true,
            opacity: 1.0,
            rect,
            kind,
        }
    }

    /// Full-bleed layer covering a design canvas.
    pub fn full_bleed(kind: LayerKind, name: impl Into<String>, w: f64, h: f64) -> Self {
        Self::new(kind, name, Rect::new(0.0, 0.0, w, h))
    }

    pub fn normalize(&mut self, design_w: f64, design_h: f64) {
        self.rect.clamp_to(design_w, design_h);
        if !self.opacity.is_finite() {
            self.opacity = 1.0;
        }
        self.opacity = self.opacity.clamp(0.0, 1.0);
        self.kind.normalize();
    }

    pub fn translate(&mut self, dx: f64, dy: f64, design_w: f64, design_h: f64) {
        self.rect.x += dx;
        self.rect.y += dy;
        self.rect.clamp_to(design_w, design_h);
    }

    /// Hit test in design coordinates.
    pub fn hit(&self, x: f64, y: f64) -> bool {
        self.visible && self.rect.contains(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_parsing_and_fallback() {
        assert_eq!(
            Rgba::parse("#FF0000"),
            Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            }
        );
        assert_eq!(Rgba::parse("#00ff0080").a, 128);
        assert_eq!(Rgba::parse("nonsense"), Rgba::WHITE);
        assert_eq!(Rgba::parse("#12345"), Rgba::WHITE);
        assert_eq!(Rgba::normalize_hex("#abcdef"), "#ABCDEF");
        assert_eq!(Rgba::normalize_hex("zzz"), "#FFFFFF");
    }

    #[test]
    fn rect_clamps_into_design() {
        let mut rect = Rect::new(-50.0, -50.0, 5000.0, 10.0);
        rect.clamp_to(1920.0, 1080.0);
        assert_eq!(rect.x, 0.0);
        assert_eq!(rect.y, 0.0);
        assert_eq!(rect.w, 1920.0);
        assert_eq!(rect.h, 20.0);
    }

    #[test]
    fn layer_json_round_trips_all_kinds() {
        let kinds = vec![
            LayerKind::Text(TextProps::default()),
            LayerKind::Clock(ClockProps::default()),
            LayerKind::Timer(TimerProps::default()),
            LayerKind::Eta(EtaProps::default()),
            LayerKind::Image(ImageProps::default()),
            LayerKind::Video(VideoProps::default()),
            LayerKind::Capture(CaptureProps::default()),
            LayerKind::Queue(QueueProps::default()),
            LayerKind::Ticker(TickerProps::default()),
            LayerKind::Pattern(PatternProps::default()),
        ];
        for kind in kinds {
            let type_id = kind.type_id();
            let layer = Layer::new(kind, "L", Rect::new(10.0, 20.0, 300.0, 200.0));
            let json = serde_json::to_string(&layer).unwrap();
            assert!(json.contains(&format!("\"type\":\"{type_id}\"")));
            assert!(json.contains("\"props\""));
            let back: Layer = serde_json::from_str(&json).unwrap();
            assert_eq!(back, layer);
        }
    }

    #[test]
    fn text_props_read_from_plan_shaped_json() {
        let json = r##"{
            "id": "l1", "name": "Title", "visible": true, "opacity": 1.0,
            "rect": {"x": 160, "y": 160, "w": 1600, "h": 360},
            "type": "text",
            "props": {"text": "Welcome",
              "style": {"family": "Sans", "size": 96, "bold": true, "italic": false,
                "color": "#F8FAFC", "align": "left", "line_spacing": 1.1,
                "uppercase": false},
              "background": null, "padding": 0}
        }"##;
        let layer: Layer = serde_json::from_str(json).unwrap();
        assert_eq!(layer.rect.w, 1600.0);
        match &layer.kind {
            LayerKind::Text(p) => {
                assert_eq!(p.text, "Welcome");
                assert!(p.style.bold);
                assert_eq!(p.style.size, 96.0);
                assert!(p.background.is_none());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn missing_props_default_per_kind() {
        let json = r##"{"id":"l","name":"n","visible":true,"opacity":1.0,
            "rect":{"x":0,"y":0,"w":10,"h":10},"type":"ticker"}"##;
        let layer: Layer = serde_json::from_str(json).unwrap();
        match layer.kind {
            LayerKind::Ticker(p) => assert_eq!(p.speed, 120.0),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn unknown_layer_fields_and_types_are_ignored_or_rejected() {
        let json = r##"{"id":"l","name":"n","visible":true,"opacity":1.0,
            "rect":{"x":0,"y":0,"w":10,"h":10},"type":"image",
            "props":{"asset":null,"fit":"contain","future":1},"extra":true}"##;
        let layer: Layer = serde_json::from_str(json).unwrap();
        assert!(matches!(layer.kind, LayerKind::Image(_)));

        let bad = r#"{"id":"l","name":"n","visible":true,"opacity":1.0,
            "rect":{"x":0,"y":0,"w":10,"h":10},"type":"hologram","props":{}}"#;
        assert!(serde_json::from_str::<Layer>(bad).is_err());
    }

    #[test]
    fn normalize_clamps_opacity_and_volume() {
        let mut layer = Layer::new(
            LayerKind::Video(VideoProps::default()),
            "v",
            Rect::new(0.0, 0.0, 100.0, 100.0),
        );
        layer.opacity = 4.0;
        if let LayerKind::Video(p) = &mut layer.kind {
            p.volume = -2.0;
        }
        layer.normalize(1920.0, 1080.0);
        assert_eq!(layer.opacity, 1.0);
        match layer.kind {
            LayerKind::Video(p) => assert_eq!(p.volume, 0.0),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn capture_source_round_trips() {
        for source in [
            CaptureSource::Camera {
                device: "/dev/video0".into(),
                label: "Camera".into(),
                width: Some(1280),
                height: Some(720),
                fps: Some(30),
            },
            CaptureSource::X11Window {
                title: "Slides".into(),
                wm_class: "google-chrome".into(),
                xid: 123,
            },
            CaptureSource::Portal {
                label: "Screen 1".into(),
                restore_token: Some("tok".into()),
            },
        ] {
            let json = serde_json::to_string(&source).unwrap();
            let back: CaptureSource = serde_json::from_str(&json).unwrap();
            assert_eq!(back, source);
        }
    }
}
