//! `SceneView`: the single scene renderer used by the editor canvas, the preview
//! output, the program output and (indirectly) the prompter.
//!
//! Rendering contract (see the project plan): black widget background, content
//! rect for the design aspect with fit/fill/stretch, optional rotation around the
//! centre, layers bottom-to-top with per-layer opacity and clipping, Pango text,
//! media paintables for video/capture, live queue/timer/clock/ETA text, ticker
//! animation, and editor overlays in screen space.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gtk::gdk;
use gtk::glib;
use gtk::graphene;
use gtk::pango;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk4 as gtk;

use crate::model::layer::{
    CaptureProps, ClockProps, EtaProps, Fit, ImageProps, Layer, LayerKind, PatternKind,
    PatternProps, QueueProps, Rgba, TextProps, TextStyle, TickerProps, TimerMode, TimerProps,
    VideoProps,
};
use crate::model::project::{OutputScale, Scene};
use crate::services::timers;
use crate::ui::scene_view::{self, ContentGeometry, fit_rect, gdk_rgba, gdk_rgba_str, text_layout};

/// How a scene view is being used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Editor,
    Preview,
    Program,
    Prompter,
}

impl ViewMode {
    /// Program is the only mode that renders live capture/video with audio.
    pub fn is_output(self) -> bool {
        matches!(self, ViewMode::Program | ViewMode::Preview)
    }

    pub fn plays_media(self) -> bool {
        matches!(self, ViewMode::Program)
    }
}

/// Formatted timer strings, produced by the controller from the timer service.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimerTexts {
    pub elapsed: String,
    pub remaining: String,
    pub end_at: String,
}

impl TimerTexts {
    pub fn for_mode(&self, mode: TimerMode) -> &str {
        match mode {
            TimerMode::Elapsed => &self.elapsed,
            TimerMode::Remaining => &self.remaining,
            TimerMode::EndAt => &self.end_at,
        }
    }
}

/// One queue entry as rendered on screen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueueEntry {
    pub ticket: String,
    pub name: String,
    pub note: String,
}

/// Live data the view needs in order to draw; the controller refreshes it.
#[derive(Debug, Clone, Default)]
pub struct RenderData {
    pub current: Option<QueueEntry>,
    /// Previously finished call, drawn at the bottom when tracking is enabled.
    pub last_called: Option<QueueEntry>,
    /// Seconds since the current call started, while the glow animation runs.
    pub glow_phase: Option<f64>,
    pub upcoming: Vec<QueueEntry>,
    pub timers: HashMap<String, TimerTexts>,
    /// Remaining time of the committed playlist item, formatted.
    pub eta: Option<String>,
    /// Clock override, mainly for the render example and tests.
    pub now: Option<chrono::DateTime<chrono::Local>>,
}

impl RenderData {
    pub fn now(&self) -> chrono::DateTime<chrono::Local> {
        self.now.unwrap_or_else(chrono::Local::now)
    }

    pub fn timer_text(&self, id: &str, mode: TimerMode) -> String {
        match self.timers.get(id) {
            Some(texts) => {
                let text = texts.for_mode(mode);
                if text.is_empty() {
                    timers::MISSING.to_string()
                } else {
                    text.to_string()
                }
            }
            None => timers::MISSING.to_string(),
        }
    }
}

/// Supplies live paintables for video and capture layers.
pub trait MediaProvider {
    fn paintable(&self, layer_id: &str) -> Option<gdk::Paintable>;
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SceneView {
        pub mode: Cell<ViewMode>,
        pub design: Cell<(f64, f64)>,
        pub background: RefCell<String>,
        pub scale: Cell<OutputScale>,
        pub rotation: Cell<u16>,
        pub scene: RefCell<Option<Scene>>,
        pub data: RefCell<RenderData>,
        pub selection: RefCell<Option<String>>,
        pub grid: Cell<bool>,
        pub grid_spacing: Cell<f64>,
        pub safe_area: Cell<bool>,
        pub show_overlays: Cell<bool>,
        pub zoom: Cell<f64>,
        pub media: RefCell<Option<Rc<dyn MediaProvider>>>,
        pub frozen: RefCell<Option<gdk::Texture>>,
        pub ticker_offsets: RefCell<HashMap<String, f64>>,
        pub textures: RefCell<HashMap<PathBuf, (i64, gdk::Texture)>>,
        pub asset_base: RefCell<Option<PathBuf>>,
        pub tick: Cell<Option<gtk::TickCallbackId>>,
        pub timeout: Cell<Option<glib::SourceId>>,
        pub last_frame_us: Cell<i64>,
        pub media_links: RefCell<Vec<gdk::Paintable>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SceneView {
        const NAME: &'static str = "SignifiageSceneView";
        type Type = super::SceneView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for SceneView {}

    impl WidgetImpl for SceneView {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let obj = self.obj();
            if self.mode.get() == ViewMode::Editor && self.zoom.get() > 0.0 {
                let (dw, dh) = self.design.get();
                let zoom = self.zoom.get();
                let value = match orientation {
                    gtk::Orientation::Horizontal => dw * zoom,
                    _ => dh * zoom,
                };
                let value = value.round().clamp(0.0, i32::MAX as f64) as i32;
                return (0, value, -1, -1);
            }
            let _ = obj;
            (0, 0, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let width = widget.width() as f64;
            let height = widget.height() as f64;
            snapshot.append_color(&gdk::RGBA::BLACK, &scene_view::full_rect(width, height));

            if let Some(texture) = self.frozen.borrow().as_ref() {
                snapshot.append_texture(texture, &scene_view::full_rect(width, height));
                return;
            }

            let geo = scene_view::content_geometry(
                (width, height),
                self.design.get(),
                self.scale.get(),
                self.rotation.get(),
                self.zoom.get(),
                self.mode.get(),
            );

            let context = widget.create_pango_context();
            let scene = self.scene.borrow();
            let data = self.data.borrow();

            snapshot.save();
            if geo.rotated() {
                let cx = width / 2.0;
                let cy = height / 2.0;
                snapshot.translate(&graphene::Point::new(cx as f32, cy as f32));
                snapshot.rotate(f32::from(geo.rotation));
                snapshot.translate(&graphene::Point::new(-cx as f32, -cy as f32));
            }
            snapshot.push_clip(&scene_view::rect(geo.x, geo.y, geo.w, geo.h));

            let painter = Painter {
                snapshot,
                context: context.clone(),
                geo,
                mode: self.mode.get(),
                data: &data,
                offsets: &self.ticker_offsets.borrow(),
                media: self.media.borrow().clone(),
                textures: &self.textures,
                asset_base: self.asset_base.borrow().clone(),
            };

            let background = scene
                .as_ref()
                .map(|s| s.background.clone())
                .unwrap_or_else(|| self.background.borrow().clone());
            snapshot.append_color(
                &gdk_rgba_str(&background),
                &scene_view::rect(geo.x, geo.y, geo.w, geo.h),
            );

            if let Some(scene) = scene.as_ref() {
                for layer in scene.layers.iter().filter(|layer| layer.visible) {
                    if layer.opacity <= 0.0 {
                        continue;
                    }
                    snapshot.push_opacity(layer.opacity.clamp(0.0, 1.0));
                    painter.layer(layer);
                    snapshot.pop();
                }
            }
            drop(painter);
            snapshot.pop();
            snapshot.restore();

            // Editor overlays are drawn in screen space, never rotated.
            if self.mode.get() == ViewMode::Editor {
                self.draw_overlays(snapshot, geo, scene.as_ref(), &context);
            }
        }
    }

    impl SceneView {
        fn draw_overlays(
            &self,
            snapshot: &gtk::Snapshot,
            geo: ContentGeometry,
            scene: Option<&Scene>,
            context: &pango::Context,
        ) {
            let accent = gdk::RGBA::new(0.22, 0.74, 0.97, 0.9);
            let faint = gdk::RGBA::new(1.0, 1.0, 1.0, 0.08);

            if self.grid.get() && self.show_overlays.get() {
                let grid = self.grid_spacing(geo);
                if grid >= 6.0 {
                    let mut x = geo.x + grid;
                    while x < geo.x + geo.w {
                        snapshot.append_color(&faint, &scene_view::rect(x, geo.y, 1.0, geo.h));
                        x += grid;
                    }
                    let mut y = geo.y + grid;
                    while y < geo.y + geo.h {
                        snapshot.append_color(&faint, &scene_view::rect(geo.x, y, geo.w, 1.0));
                        y += grid;
                    }
                }
            }

            if self.safe_area.get() && self.show_overlays.get() {
                let inset_x = geo.w * 0.05;
                let inset_y = geo.h * 0.05;
                outline(
                    snapshot,
                    geo.x + inset_x,
                    geo.y + inset_y,
                    geo.w - inset_x * 2.0,
                    geo.h - inset_y * 2.0,
                    &gdk::RGBA::new(0.22, 0.74, 0.97, 0.35),
                    1.0,
                );
            }

            let Some(scene) = scene else {
                return;
            };
            let Some(selected) = self.selection.borrow().clone() else {
                return;
            };
            let Some(layer) = scene.layer(&selected) else {
                return;
            };
            let (x, y) = geo.to_view(layer.rect.x, layer.rect.y);
            let w = layer.rect.w * geo.sx;
            let h = layer.rect.h * geo.sy;
            outline(snapshot, x, y, w, h, &accent, 2.0);

            let handle = 10.0;
            for (hx, hy) in [
                (x - handle / 2.0, y - handle / 2.0),
                (x + w - handle / 2.0, y - handle / 2.0),
                (x - handle / 2.0, y + h - handle / 2.0),
                (x + w - handle / 2.0, y + h - handle / 2.0),
            ] {
                snapshot.append_color(&accent, &scene_view::rect(hx, hy, handle, handle));
            }
            let _ = context;
        }

        fn grid_spacing(&self, geo: ContentGeometry) -> f64 {
            self.grid_spacing.get().max(1.0) * geo.sx
        }
    }

    struct Painter<'a> {
        snapshot: &'a gtk::Snapshot,
        context: pango::Context,
        geo: ContentGeometry,
        mode: ViewMode,
        data: &'a RenderData,
        offsets: &'a HashMap<String, f64>,
        media: Option<Rc<dyn MediaProvider>>,
        textures: &'a RefCell<HashMap<PathBuf, (i64, gdk::Texture)>>,
        asset_base: Option<PathBuf>,
    }

    impl<'a> Painter<'a> {
        fn layer(&self, layer: &Layer) {
            let (x, y) = self.geo.to_view(layer.rect.x, layer.rect.y);
            let w = layer.rect.w * self.geo.sx;
            let h = layer.rect.h * self.geo.sy;
            self.snapshot.push_clip(&scene_view::rect(x, y, w, h));
            match &layer.kind {
                LayerKind::Text(props) => self.text(props, x, y, w, h),
                LayerKind::Clock(props) => self.clock(props, x, y, w, h),
                LayerKind::Timer(props) => self.timer(props, x, y, w, h),
                LayerKind::Eta(props) => self.eta(props, x, y, w, h),
                LayerKind::Image(props) => self.image(props, &layer.id, x, y, w, h),
                LayerKind::Video(props) => self.video(props, &layer.id, x, y, w, h),
                LayerKind::Capture(props) => self.capture(props, &layer.id, x, y, w, h),
                LayerKind::Queue(props) => self.queue(props, x, y, w, h),
                LayerKind::Ticker(props) => self.ticker(props, &layer.id, x, y, w, h),
                LayerKind::Pattern(props) => self.pattern(props, x, y, w, h),
            }
            self.snapshot.pop();
        }

        fn fill(&self, x: f64, y: f64, w: f64, h: f64, color: gdk::RGBA) {
            self.snapshot
                .append_color(&color, &scene_view::rect(x, y, w, h));
        }

        /// Draw a Pango layout inside the rect; returns the consumed height.
        fn layout(
            &self,
            text: &str,
            style: &TextStyle,
            x: f64,
            y: f64,
            w: f64,
            h: f64,
            color: Option<Rgba>,
            vcenter: bool,
        ) -> f64 {
            if text.is_empty() {
                return 0.0;
            }
            let layout = text_layout(&self.context, text, style, w, self.geo.sx);
            let (lw, lh) = scene_view::layout_size(&layout);
            let oy = if vcenter {
                y + ((h - lh) / 2.0).max(0.0)
            } else {
                y
            };
            let ox = x;
            let color = gdk_rgba(color.unwrap_or_else(|| style.rgba()));
            self.snapshot.save();
            self.snapshot
                .translate(&graphene::Point::new(ox as f32, oy as f32));
            self.snapshot.append_layout(&layout, &color);
            self.snapshot.restore();
            let _ = lw;
            lh
        }

        fn paint(&self, paintable: &gdk::Paintable, x: f64, y: f64, w: f64, h: f64) {
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            self.snapshot.save();
            self.snapshot
                .translate(&graphene::Point::new(x as f32, y as f32));
            paintable.snapshot(self.snapshot, w, h);
            self.snapshot.restore();
        }

        fn placeholder(&self, x: f64, y: f64, w: f64, h: f64, label: &str) {
            let muted = gdk::RGBA::new(0.4, 0.45, 0.55, 1.0);
            outline(
                self.snapshot,
                x + 1.0,
                y + 1.0,
                w - 2.0,
                h - 2.0,
                &muted,
                1.0,
            );
            let style = TextStyle {
                size: (w * 0.035).clamp(11.0, 40.0),
                align: crate::model::layer::TextAlign::Center,
                color: "#94A3B8".to_string(),
                ..TextStyle::default()
            };
            self.layout(label, &style, x + w * 0.05, y, w * 0.9, h, None, true);
        }

        fn text(&self, props: &TextProps, x: f64, y: f64, w: f64, h: f64) {
            if let Some(background) = &props.background {
                let padding = props.padding.max(0.0) * self.geo.sx;
                self.fill(
                    x - padding,
                    y - padding,
                    w + padding * 2.0,
                    h + padding * 2.0,
                    gdk_rgba_str(background),
                );
            }
            self.layout(&props.text, &props.style, x, y, w, h, None, true);
        }

        fn clock(&self, props: &ClockProps, x: f64, y: f64, w: f64, h: f64) {
            let now = self.data.now();
            let mut text = timers::format_clock(props.preset, now);
            if props.show_date {
                text.push('\n');
                text.push_str(&timers::format_date(now));
            }
            self.layout(&text, &props.style, x, y, w, h, None, true);
        }

        fn timer(&self, props: &TimerProps, x: f64, y: f64, w: f64, h: f64) {
            let text = self.data.timer_text(&props.timer_id, props.mode);
            let overtime = props.mode == TimerMode::Remaining && text.starts_with('+');
            let color = if overtime {
                Some(Rgba::parse(&props.warning_color))
            } else {
                None
            };
            self.layout(&text, &props.style, x, y, w, h, color, true);
        }

        fn eta(&self, props: &EtaProps, x: f64, y: f64, w: f64, h: f64) {
            let value = self
                .data
                .eta
                .clone()
                .unwrap_or_else(|| timers::MISSING.to_string());
            let label_style = TextStyle {
                size: props.style.size * 0.45,
                ..props.style.clone()
            };
            let label_h = self.layout(
                &props.display_label(),
                &label_style,
                x,
                y,
                w,
                h * 0.4,
                None,
                false,
            );
            self.layout(
                &value,
                &props.style,
                x,
                y + label_h,
                w,
                h - label_h,
                None,
                false,
            );
        }

        fn image(&self, props: &ImageProps, _layer_id: &str, x: f64, y: f64, w: f64, h: f64) {
            let Some(asset) = props.asset.as_deref() else {
                self.placeholder(x, y, w, h, &rust_i18n::t!("layer.image.no_asset"));
                return;
            };
            match self.load_texture(asset) {
                Some(texture) => {
                    let size = (f64::from(texture.width()), f64::from(texture.height()));
                    let scale = match props.fit {
                        Fit::Contain => OutputScale::Fit,
                        Fit::Cover => OutputScale::Fill,
                        Fit::Stretch => OutputScale::Stretch,
                    };
                    let (ix, iy, iw, ih) = fit_rect((x, y, w, h), size, scale);
                    self.paint(texture.upcast_ref(), ix, iy, iw, ih);
                }
                None => self.placeholder(x, y, w, h, asset),
            }
        }

        fn load_texture(&self, asset: &str) -> Option<gdk::Texture> {
            let path = self.resolve_asset(asset);
            let mtime = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);
            if let Some((_stamp, texture)) = self.textures.borrow().get(&path) {
                return Some(texture.clone());
            }
            let stamp = std::fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(mtime);
            let texture = gdk::Texture::from_filename(&path).ok().or_else(|| {
                gdk_pixbuf::Pixbuf::from_file(&path)
                    .ok()
                    .map(|pixbuf| gdk::Texture::for_pixbuf(&pixbuf))
            })?;
            self.textures
                .borrow_mut()
                .insert(path, (stamp, texture.clone()));
            Some(texture)
        }

        /// Resolve a stored asset path against the project directory.
        fn resolve_asset(&self, asset: &str) -> PathBuf {
            let path = PathBuf::from(asset);
            if path.is_absolute() {
                return path;
            }
            match self.asset_base.as_ref() {
                Some(base) => base.join(path),
                None => path,
            }
        }

        fn video(&self, props: &VideoProps, layer_id: &str, x: f64, y: f64, w: f64, h: f64) {
            if self.mode == ViewMode::Editor {
                let label = props
                    .asset
                    .clone()
                    .unwrap_or_else(|| rust_i18n::t!("layer.video.no_asset").to_string());
                self.placeholder(x, y, w, h, &label);
                return;
            }
            match self
                .media
                .as_ref()
                .and_then(|provider| provider.paintable(layer_id))
            {
                Some(paintable) => {
                    let size = (
                        f64::from(paintable.intrinsic_width()),
                        f64::from(paintable.intrinsic_height()),
                    );
                    let scale = match props.fit {
                        Fit::Contain => OutputScale::Fit,
                        Fit::Cover => OutputScale::Fill,
                        Fit::Stretch => OutputScale::Stretch,
                    };
                    let (ix, iy, iw, ih) = fit_rect((x, y, w, h), size, scale);
                    self.paint(&paintable, ix, iy, iw, ih);
                }
                None => {
                    let label = props
                        .asset
                        .clone()
                        .unwrap_or_else(|| rust_i18n::t!("layer.video.no_asset").to_string());
                    self.placeholder(x, y, w, h, &label);
                }
            }
        }

        fn capture(&self, props: &CaptureProps, layer_id: &str, x: f64, y: f64, w: f64, h: f64) {
            if !self.mode.plays_media() {
                let label = format!(
                    "{}: {}",
                    rust_i18n::t!("layer.type.capture"),
                    props.source.label()
                );
                self.placeholder(x, y, w, h, &label);
                return;
            }
            match self
                .media
                .as_ref()
                .and_then(|provider| provider.paintable(layer_id))
            {
                Some(paintable) => {
                    let size = (
                        f64::from(paintable.intrinsic_width()),
                        f64::from(paintable.intrinsic_height()),
                    );
                    let scale = match props.fit {
                        Fit::Contain => OutputScale::Fit,
                        Fit::Cover => OutputScale::Fill,
                        Fit::Stretch => OutputScale::Stretch,
                    };
                    let (ix, iy, iw, ih) = fit_rect((x, y, w, h), size, scale);
                    if props.mirror {
                        self.snapshot.save();
                        self.snapshot
                            .translate(&graphene::Point::new((ix + iw / 2.0) as f32, 0.0));
                        self.snapshot.scale(-1.0, 1.0);
                        self.snapshot
                            .translate(&graphene::Point::new(-(ix + iw / 2.0) as f32, 0.0));
                        self.paint(&paintable, ix, iy, iw, ih);
                        self.snapshot.restore();
                    } else {
                        self.paint(&paintable, ix, iy, iw, ih);
                    }
                }
                None => {
                    let label = format!(
                        "{}: {}",
                        rust_i18n::t!("layer.type.capture"),
                        props.source.label()
                    );
                    self.placeholder(x, y, w, h, &label);
                }
            }
        }

        fn queue(&self, props: &QueueProps, x: f64, y: f64, w: f64, h: f64) {
            let accent = Rgba::parse(&props.accent);
            let pad = (w * 0.06).min(h * 0.1);
            let inner_x = x + pad;
            let inner_w = w - pad * 2.0;
            let mut cursor = y + h * 0.08;
            let max_y = y + h - h * 0.02;

            // Title, small caps in the accent colour.
            let title_style = TextStyle {
                size: props.style.size * 0.42,
                bold: true,
                uppercase: true,
                align: crate::model::layer::TextAlign::Center,
                color: accent.to_hex(),
                ..props.style.clone()
            };
            cursor += self.layout(
                &props.display_title(),
                &title_style,
                inner_x,
                cursor,
                inner_w,
                h * 0.08,
                Some(accent),
                false,
            );
            cursor += h * 0.02;
            self.fill(inner_x, cursor, inner_w, 3.0, gdk_rgba(accent));
            cursor += h * 0.05;

            // "Next in line" preview (privacy switch hides the waiting names).
            if props.show_next
                && let Some(next) = self.data.upcoming.first()
            {
                let next_style = TextStyle {
                    size: props.style.size * 0.42,
                    bold: true,
                    uppercase: true,
                    align: crate::model::layer::TextAlign::Center,
                    color: "#94A3B8".to_string(),
                    ..props.style.clone()
                };
                let text = if props.show_ticket && !next.ticket.is_empty() {
                    format!(
                        "{} {}",
                        rust_i18n::t!("queue.next_in_line"),
                        format!(
                            "{}  {}",
                            crate::model::queue::pad_ticket(&next.ticket),
                            next.name
                        )
                    )
                } else {
                    format!("{} {}", rust_i18n::t!("queue.next_in_line"), next.name)
                };
                let used = self.layout(
                    &text,
                    &next_style,
                    inner_x,
                    cursor,
                    inner_w,
                    h * 0.07,
                    None,
                    false,
                );
                cursor += used + h * 0.015;
            }

            // Breathing halo behind the called name while the glow runs.
            if let Some(phase) = self.data.glow_phase
                && self.data.current.is_some()
            {
                let pulse = 0.5 + 0.5 * (phase * std::f64::consts::TAU * 0.8).sin();
                let alpha = (0.05 + 0.10 * pulse) as f32;
                let halo = gdk::RGBA::new(
                    f32::from(accent.r) / 255.0,
                    f32::from(accent.g) / 255.0,
                    f32::from(accent.b) / 255.0,
                    alpha,
                );
                self.fill(
                    inner_x - pad * 0.15,
                    cursor - h * 0.005,
                    inner_w + pad * 0.3,
                    (h * 0.42).min(max_y - cursor),
                    halo,
                );
            }

            match &self.data.current {
                Some(current) => {
                    if props.show_ticket && !current.ticket.is_empty() {
                        let ticket_style = TextStyle {
                            size: props.style.size * 1.9,
                            bold: true,
                            align: crate::model::layer::TextAlign::Center,
                            color: accent.to_hex(),
                            ..props.style.clone()
                        };
                        let used = self.layout(
                            &crate::model::queue::pad_ticket(&current.ticket),
                            &ticket_style,
                            inner_x,
                            cursor,
                            inner_w,
                            h * 0.25,
                            Some(accent),
                            false,
                        );
                        cursor += used + h * 0.01;
                    }
                    let name_style = TextStyle {
                        size: props.style.size * 1.15,
                        bold: true,
                        align: crate::model::layer::TextAlign::Center,
                        ..props.style.clone()
                    };
                    let used = self.layout(
                        &current.name,
                        &name_style,
                        inner_x,
                        cursor,
                        inner_w,
                        h * 0.18,
                        None,
                        false,
                    );
                    cursor += used + h * 0.01;
                    if props.show_note && !current.note.is_empty() {
                        let note_style = TextStyle {
                            size: props.style.size * 0.55,
                            bold: false,
                            align: crate::model::layer::TextAlign::Center,
                            color: "#94A3B8".to_string(),
                            ..props.style.clone()
                        };
                        let used = self.layout(
                            &current.note,
                            &note_style,
                            inner_x,
                            cursor,
                            inner_w,
                            h * 0.1,
                            None,
                            false,
                        );
                        cursor += used;
                    }
                }
                None => {
                    let empty_style = TextStyle {
                        size: props.style.size * 0.8,
                        bold: false,
                        align: crate::model::layer::TextAlign::Center,
                        color: "#94A3B8".to_string(),
                        ..props.style.clone()
                    };
                    self.layout(
                        &props.display_empty(),
                        &empty_style,
                        inner_x,
                        cursor,
                        inner_w,
                        (max_y - cursor).max(0.0),
                        None,
                        true,
                    );
                }
            }

            // Last called (tracking switch) — right under the current call.
            let mut row_cursor = cursor + h * 0.02;
            if props.show_last_called
                && let Some(last) = self.data.last_called.as_ref()
            {
                let last_style = TextStyle {
                    size: props.style.size * 0.4,
                    bold: false,
                    uppercase: true,
                    align: crate::model::layer::TextAlign::Center,
                    color: "#64748B".to_string(),
                    ..props.style.clone()
                };
                let text = if props.show_ticket && !last.ticket.is_empty() {
                    format!(
                        "{} {}",
                        rust_i18n::t!("queue.last_called"),
                        format!(
                            "{}  {}",
                            crate::model::queue::pad_ticket(&last.ticket),
                            last.name
                        )
                    )
                } else {
                    format!("{} {}", rust_i18n::t!("queue.last_called"), last.name)
                };
                let used = self.layout(
                    &text,
                    &last_style,
                    inner_x,
                    row_cursor,
                    inner_w,
                    h * 0.06,
                    None,
                    false,
                );
                row_cursor += used + h * 0.015;
            }

            // Upcoming waiting rows (hidden entirely by the privacy switch).
            let row_style = TextStyle {
                size: props.style.size * 0.55,
                bold: false,
                align: crate::model::layer::TextAlign::Center,
                color: "#CBD5E1".to_string(),
                ..props.style.clone()
            };
            let rows: &[QueueEntry] = if props.show_next {
                &self.data.upcoming
            } else {
                &[]
            };
            for entry in rows.iter().take(props.upcoming as usize) {
                if row_cursor + h * 0.06 > max_y {
                    break;
                }
                let text = if !props.show_ticket || entry.ticket.is_empty() {
                    entry.name.clone()
                } else {
                    format!(
                        "{}   {}",
                        crate::model::queue::pad_ticket(&entry.ticket),
                        entry.name
                    )
                };
                let used = self.layout(
                    &text,
                    &row_style,
                    inner_x,
                    row_cursor,
                    inner_w,
                    h * 0.07,
                    None,
                    false,
                );
                row_cursor += used + h * 0.006;
            }
        }

        fn ticker(&self, props: &TickerProps, layer_id: &str, x: f64, y: f64, w: f64, h: f64) {
            if let Some(background) = &props.background {
                self.fill(x, y, w, h, gdk_rgba_str(background));
            }
            let style = TextStyle {
                align: crate::model::layer::TextAlign::Left,
                ..props.style.clone()
            };
            let gap = props.gap.max(0.0) * self.geo.sx;
            let text_width =
                scene_view::text_extent(&self.context, &props.text, &style, self.geo.sx);
            let cycle = (text_width + gap).max(1.0);
            let offset = self.offsets.get(layer_id).copied().unwrap_or(0.0) % cycle;
            let line_h = {
                let layout = text_layout(
                    &self.context,
                    &props.text,
                    &style,
                    text_width + 2.0,
                    self.geo.sx,
                );
                scene_view::layout_size(&layout).1
            };
            let ty = y + ((h - line_h) / 2.0).max(0.0);
            let mut cursor = x - offset;
            let mut guard = 0;
            while cursor < x + w && guard < 64 {
                self.layout(
                    &props.text,
                    &style,
                    cursor,
                    ty,
                    text_width + 2.0,
                    line_h,
                    None,
                    false,
                );
                cursor += cycle;
                guard += 1;
            }
        }

        fn pattern(&self, props: &PatternProps, x: f64, y: f64, w: f64, h: f64) {
            let color = Rgba::parse(&props.color);
            match props.pattern {
                PatternKind::Grid => {
                    let faint = gdk::RGBA::new(
                        f32::from(color.r) / 255.0,
                        f32::from(color.g) / 255.0,
                        f32::from(color.b) / 255.0,
                        0.25,
                    );
                    let step_x = w / 12.0;
                    let step_y = h / 8.0;
                    let mut i = 1;
                    while i < 12 {
                        self.fill(x + step_x * f64::from(i), y, 1.0, h, faint);
                        i += 1;
                    }
                    let mut j = 1;
                    while j < 8 {
                        self.fill(x, y + step_y * f64::from(j), w, 1.0, faint);
                        j += 1;
                    }
                    self.fill(x + w / 2.0 - 1.0, y, 2.0, h, gdk_rgba(color));
                    self.fill(x, y + h / 2.0 - 1.0, w, 2.0, gdk_rgba(color));
                }
                PatternKind::Zebra => {
                    for i in 0..8 {
                        let step = w / 8.0;
                        let shade = if i % 2 == 0 {
                            gdk_rgba(color)
                        } else {
                            gdk::RGBA::new(
                                f32::from(color.r) / 255.0,
                                f32::from(color.g) / 255.0,
                                f32::from(color.b) / 255.0,
                                0.25,
                            )
                        };
                        self.fill(x + step * f64::from(i), y, step, h, shade);
                    }
                }
                PatternKind::Colorbars => {
                    let bars = [
                        gdk::RGBA::new(1.0, 1.0, 1.0, 1.0),
                        gdk::RGBA::new(1.0, 1.0, 0.0, 1.0),
                        gdk::RGBA::new(0.0, 1.0, 1.0, 1.0),
                        gdk::RGBA::new(0.0, 1.0, 0.0, 1.0),
                        gdk::RGBA::new(1.0, 0.0, 1.0, 1.0),
                        gdk::RGBA::new(1.0, 0.0, 0.0, 1.0),
                        gdk::RGBA::new(0.0, 0.0, 1.0, 1.0),
                        gdk::RGBA::new(0.0, 0.0, 0.0, 1.0),
                    ];
                    let step = w / 8.0;
                    for (i, bar) in bars.iter().enumerate() {
                        self.fill(x + step * i as f64, y, step, h, *bar);
                    }
                }
                PatternKind::Focus => {
                    self.fill(x, y, w, h, gdk::RGBA::new(0.02, 0.02, 0.03, 1.0));
                    let cx = x + w / 2.0;
                    let cy = y + h / 2.0;
                    self.fill(cx - 1.0, y, 2.0, h, gdk_rgba(color));
                    self.fill(x, cy - 1.0, w, 2.0, gdk_rgba(color));
                    let radius = w.min(h) * 0.15;
                    outline(
                        self.snapshot,
                        cx - radius,
                        cy - radius,
                        radius * 2.0,
                        radius * 2.0,
                        &gdk_rgba(color),
                        2.0,
                    );
                    let bracket = w.min(h) * 0.05;
                    for (bx, by, sx, sy) in [
                        (x, y, 1.0, 1.0),
                        (x + w, y, -1.0, 1.0),
                        (x, y + h, 1.0, -1.0),
                        (x + w, y + h, -1.0, -1.0),
                    ] {
                        self.fill(
                            bx.min(bx + bracket * sx),
                            by.min(by + bracket * sy) - if sy < 0.0 { 2.0 } else { 0.0 },
                            bracket,
                            2.0,
                            gdk_rgba(color),
                        );
                    }
                }
            }
        }
    }

    /// Draw a 1px/2px outline as four filled rects.
    pub(crate) fn outline(
        snapshot: &gtk::Snapshot,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        color: &gdk::RGBA,
        thickness: f64,
    ) {
        snapshot.append_color(color, &scene_view::rect(x, y, w, thickness));
        snapshot.append_color(color, &scene_view::rect(x, y + h - thickness, w, thickness));
        snapshot.append_color(color, &scene_view::rect(x, y, thickness, h));
        snapshot.append_color(color, &scene_view::rect(x + w - thickness, y, thickness, h));
    }
}

glib::wrapper! {
    pub struct SceneView(ObjectSubclass<imp::SceneView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for SceneView {
    fn default() -> Self {
        Self::new(ViewMode::Editor)
    }
}

impl SceneView {
    pub fn new(mode: ViewMode) -> Self {
        let view: Self = glib::Object::builder().build();
        view.imp().mode.set(mode);
        view.imp().design.set((1920.0, 1080.0));
        view.imp().background.replace("#000000".to_string());
        view.imp().scale.set(OutputScale::Fit);
        view.imp().grid.set(true);
        view.imp().grid_spacing.set(10.0);
        view.imp().safe_area.set(true);
        view.imp().show_overlays.set(mode == ViewMode::Editor);
        view.set_hexpand(true);
        view.set_vexpand(true);
        view.set_focusable(mode == ViewMode::Editor);

        let weak = view.downgrade();
        view.add_tick_callback(move |widget, clock| {
            if let Some(view) = weak.upgrade() {
                view.on_tick(clock.frame_time());
            }
            let _ = widget;
            glib::ControlFlow::Continue
        });
        // The tick callback above only advances tickers that exist; keep it but
        // disable work in `on_tick` when nothing animates.
        view
    }

    pub fn mode(&self) -> ViewMode {
        self.imp().mode.get()
    }

    fn on_tick(&self, frame_us: i64) {
        let imp = self.imp();
        let last = imp.last_frame_us.get();
        imp.last_frame_us.set(frame_us);
        if last == 0 {
            return;
        }
        let dt = ((frame_us - last) as f64 / 1_000_000.0).clamp(0.0, 0.25);
        let scene = imp.scene.borrow();
        let Some(scene) = scene.as_ref() else {
            return;
        };
        let tickers: Vec<(String, f64)> = scene
            .layers
            .iter()
            .filter(|layer| layer.visible)
            .filter_map(|layer| match &layer.kind {
                LayerKind::Ticker(props) => Some((layer.id.clone(), props.speed)),
                _ => None,
            })
            .collect();
        if tickers.is_empty() {
            return;
        }
        {
            let mut offsets = imp.ticker_offsets.borrow_mut();
            for (id, speed) in tickers {
                let entry = offsets.entry(id).or_insert(0.0);
                *entry += speed * dt;
            }
        }
        self.queue_draw();
    }

    /// Design size, scale mode and rotation.
    pub fn set_design(
        &self,
        width: u32,
        height: u32,
        background: &str,
        scale: OutputScale,
        rotation: u16,
    ) {
        let imp = self.imp();
        imp.design.set((f64::from(width), f64::from(height)));
        imp.background.replace(background.to_string());
        imp.scale.set(scale);
        imp.rotation.set(rotation);
        self.queue_resize();
        self.queue_draw();
    }

    /// Replace the rendered scene (`None` renders black).
    pub fn set_scene(&self, scene: Option<Scene>) {
        let imp = self.imp();
        let previous = imp.scene.borrow().as_ref().map(|s| s.id.clone());
        let next = scene.as_ref().map(|s| s.id.clone());
        if previous == next {
            // Same scene: keep ticker offsets, just refresh the content.
            imp.scene.replace(scene);
        } else {
            imp.scene.replace(scene);
            imp.ticker_offsets.borrow_mut().clear();
        }
        self.ensure_timeout();
        self.queue_draw();
    }

    pub fn scene(&self) -> Option<Scene> {
        self.imp().scene.borrow().clone()
    }

    pub fn set_render_data(&self, data: RenderData) {
        let was_glowing = self.imp().data.borrow().glow_phase.is_some();
        let is_glowing = data.glow_phase.is_some();
        self.imp().data.replace(data);
        if was_glowing != is_glowing {
            self.ensure_timeout();
        }
        self.queue_draw();
    }

    pub fn render_data(&self) -> RenderData {
        self.imp().data.borrow().clone()
    }

    pub fn set_selection(&self, id: Option<&str>) {
        self.imp()
            .selection
            .replace(id.map(|value| value.to_string()));
        self.queue_draw();
    }

    pub fn selection(&self) -> Option<String> {
        self.imp().selection.borrow().clone()
    }

    /// Grid step in design pixels (from the project).
    pub fn set_grid_spacing(&self, spacing: f64) {
        self.imp().grid_spacing.set(spacing.max(1.0));
        self.queue_draw();
    }

    pub fn set_overlays(&self, grid: bool, safe_area: bool) {
        self.imp().grid.set(grid);
        self.imp().safe_area.set(safe_area);
        self.queue_draw();
    }

    pub fn set_zoom(&self, zoom: f64) {
        self.imp().zoom.set(zoom.max(0.0));
        self.queue_resize();
        self.queue_draw();
    }

    pub fn zoom(&self) -> f64 {
        self.imp().zoom.get()
    }

    /// Project directory used to resolve relative `assets/...` paths.
    pub fn set_asset_base(&self, base: Option<PathBuf>) {
        self.imp().asset_base.replace(base);
        self.imp().textures.borrow_mut().clear();
        self.queue_draw();
    }

    pub fn set_media_provider(&self, provider: Option<Rc<dyn MediaProvider>>) {
        self.imp().media.replace(provider);
        self.queue_draw();
    }

    /// Re-read paintables and connect invalidation for live frames.
    pub fn refresh_media_links(&self) {
        let imp = self.imp();
        let provider = imp.media.borrow().clone();
        let Some(provider) = provider else {
            imp.media_links.borrow_mut().clear();
            return;
        };
        let scene = imp.scene.borrow();
        let Some(scene) = scene.as_ref() else {
            return;
        };
        let mut links = Vec::new();
        for layer in &scene.layers {
            if !matches!(layer.kind, LayerKind::Video(_) | LayerKind::Capture(_)) {
                continue;
            }
            let Some(paintable) = provider.paintable(&layer.id) else {
                continue;
            };
            let weak = self.downgrade();
            paintable.connect_invalidate_contents(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.queue_draw();
                }
            });
            links.push(paintable);
        }
        *imp.media_links.borrow_mut() = links;
        self.queue_draw();
    }

    pub fn set_frozen_texture(&self, texture: Option<gdk::Texture>) {
        self.imp().frozen.replace(texture);
        self.ensure_timeout();
        self.queue_draw();
    }

    pub fn is_frozen(&self) -> bool {
        self.imp().frozen.borrow().is_some()
    }

    /// Current content placement in widget coordinates.
    pub fn geometry(&self) -> ContentGeometry {
        let imp = self.imp();
        scene_view::content_geometry(
            (self.width() as f64, self.height() as f64),
            imp.design.get(),
            imp.scale.get(),
            imp.rotation.get(),
            imp.zoom.get(),
            imp.mode.get(),
        )
    }

    pub fn design_to_view(&self, x: f64, y: f64) -> (f64, f64) {
        self.geometry().to_view(x, y)
    }

    pub fn view_to_design(&self, x: f64, y: f64) -> (f64, f64) {
        self.geometry().to_design(x, y)
    }

    /// Attach/refresh the 4 fps redraw timer for clock/timer/ETA/queue layers.
    pub fn ensure_timeout(&self) {
        let imp = self.imp();
        if let Some(id) = imp.timeout.take() {
            id.remove();
        }
        if imp.frozen.borrow().is_some() {
            return;
        }
        let needs_tick = {
            let scene = imp.scene.borrow();
            scene.as_ref().is_some_and(|scene| {
                scene.layers.iter().any(|layer| {
                    matches!(
                        layer.kind,
                        LayerKind::Clock(_)
                            | LayerKind::Timer(_)
                            | LayerKind::Eta(_)
                            | LayerKind::Queue(_)
                    )
                })
            })
        };
        if !needs_tick {
            return;
        }
        let weak = self.downgrade();
        let id =
            glib::timeout_add_local(Duration::from_millis(250), move || match weak.upgrade() {
                Some(view) => {
                    view.queue_draw();
                    glib::ControlFlow::Continue
                }
                None => glib::ControlFlow::Break,
            });
        imp.timeout.set(Some(id));
    }

    /// Render the current widget content into a texture (freeze + example).
    pub fn render_to_texture(&self, width: i32, height: i32) -> Option<gdk::Texture> {
        if width <= 0 || height <= 0 {
            return None;
        }
        let native = self.native()?;
        let renderer = native.renderer()?;
        let (alloc_w, alloc_h) = (self.width(), self.height());
        if alloc_w <= 0 || alloc_h <= 0 {
            return None;
        }
        let paintable = gtk::WidgetPaintable::new(Some(self));
        let snapshot = gtk::Snapshot::new();
        snapshot.scale(
            width as f32 / alloc_w as f32,
            height as f32 / alloc_h as f32,
        );
        paintable.snapshot(&snapshot, f64::from(alloc_w), f64::from(alloc_h));
        let node = snapshot.to_node()?;
        let texture = renderer.render_texture(
            &node,
            Some(&graphene::Rect::new(0.0, 0.0, width as f32, height as f32)),
        );
        Some(texture)
    }
}
