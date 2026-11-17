//! Layer inspector: scene properties when nothing is selected, and the common
//! plus kind-specific controls of the selected layer.
//!
//! The panel is rebuilt only when the selection identity changes (scene, layer,
//! layer kind or capture backend). Everything else updates the existing widget
//! values, so typing never destroys the focused widget or its cursor.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use rust_i18n::t;

use crate::model::layer::{
    CaptureProps, CaptureSource, ClockPreset, Fit, Layer, LayerKind, PatternKind, TextAlign,
    TextStyle, TimerMode,
};
use crate::model::project::{Project, Scene, TimerDef};
use crate::services::assets::{self, AssetKind};
use crate::services::controller::Controller;
use crate::services::x11;
use crate::ui::widgets;

type Updater = Box<dyn Fn()>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetFilter {
    Image,
    Video,
}

impl AssetFilter {
    fn matches(self, kind: AssetKind) -> bool {
        match self {
            AssetFilter::Image => kind == AssetKind::Image,
            AssetFilter::Video => kind == AssetKind::Video,
        }
    }

    fn label(self) -> String {
        match self {
            AssetFilter::Image => t!("layer.type.image").to_string(),
            AssetFilter::Video => t!("layer.type.video").to_string(),
        }
    }

    fn extensions(self) -> &'static [&'static str] {
        match self {
            AssetFilter::Image => &assets::IMAGE_EXTENSIONS,
            AssetFilter::Video => &assets::VIDEO_EXTENSIONS,
        }
    }
}

pub struct Inspector {
    pub panel: gtk::Box,
    controller: Rc<Controller>,
    overlay: adw::ToastOverlay,
    content: gtk::Box,
    signature: RefCell<String>,
    updaters: RefCell<Vec<Updater>>,
    updating: Rc<Cell<bool>>,
}

impl Inspector {
    pub fn new(controller: Rc<Controller>, overlay: adw::ToastOverlay) -> Self {
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
        panel.set_width_request(300);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(12);
        content.set_margin_bottom(18);
        content.set_margin_start(12);
        content.set_margin_end(12);

        let heading = gtk::Label::new(Some(&t!("inspector.title").to_string()));
        heading.set_xalign(0.0);
        heading.add_css_class("heading");
        content.append(&heading);
        content.append(&widgets::dim_label(&t!("welcome.no_project").to_string()));

        let scrolled = gtk::ScrolledWindow::builder()
            .child(&content)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_width(280)
            .build();
        panel.append(&scrolled);

        Self {
            panel,
            controller,
            overlay,
            content,
            signature: RefCell::new(String::new()),
            updaters: RefCell::new(Vec::new()),
            updating: Rc::new(Cell::new(false)),
        }
    }

    pub fn refresh(&self) {
        let signature = self.signature();
        let changed = {
            let mut current = self.signature.borrow_mut();
            if *current == signature {
                false
            } else {
                *current = signature;
                true
            }
        };
        if changed {
            let scene = self.controller.editing_scene();
            let layer = self
                .controller
                .selected_layer()
                .and_then(|id| self.controller.layer(&id));
            self.rebuild(scene, layer);
            return;
        }
        self.updating.set(true);
        for updater in self.updaters.borrow().iter() {
            updater();
        }
        self.updating.set(false);
    }

    fn signature(&self) -> String {
        if !self.controller.is_open() {
            return "closed".to_string();
        }
        let scene_id = self.controller.editing_scene_id().unwrap_or_default();
        match self
            .controller
            .selected_layer()
            .and_then(|id| self.controller.layer(&id))
        {
            Some(layer) => {
                let mut signature =
                    format!("layer|{scene_id}|{}|{}", layer.id, layer.kind.type_id());
                if let LayerKind::Capture(props) = &layer.kind {
                    signature.push('|');
                    signature.push_str(props.source.kind_id());
                }
                signature
            }
            None => format!("scene|{scene_id}"),
        }
    }

    fn rebuild(&self, scene: Option<Scene>, layer: Option<Layer>) {
        self.updating.set(true);
        self.updaters.borrow_mut().clear();
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }

        let heading = gtk::Label::new(Some(&t!("inspector.title").to_string()));
        heading.set_xalign(0.0);
        heading.add_css_class("heading");
        self.content.append(&heading);

        if !self.controller.is_open() {
            self.content
                .append(&widgets::dim_label(&t!("welcome.no_project").to_string()));
        } else if let Some(layer) = layer {
            self.build_layer(&layer);
        } else {
            self.build_scene(scene);
        }
        self.updating.set(false);
    }

    fn push_updater(&self, updater: impl Fn() + 'static) {
        self.updaters.borrow_mut().push(Box::new(updater));
    }

    fn add_section(&self, title: &str) -> gtk::Box {
        let (outer, body) = widgets::section(title);
        self.content.append(&outer);
        body
    }

    // ------------------------------------------------------------- scene

    fn build_scene(&self, scene: Option<Scene>) {
        let body = self.add_section(&t!("inspector.scene").to_string());
        let Some(scene) = scene else {
            body.append(&widgets::dim_label(
                &t!("inspector.no_selection").to_string(),
            ));
            return;
        };
        let scene_id = scene.id.clone();

        let name = widgets::entry(&scene.name);
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = scene_id.clone();
            name.connect_changed(move |entry| {
                if updating.get() {
                    return;
                }
                let value = entry.text().to_string();
                controller.mutate(Some("scene-name"), |project| {
                    if let Some(scene) = project.scene_mut(&id) {
                        scene.name = value.clone();
                    }
                });
            });
        }
        body.append(&widgets::field(&t!("inspector.name").to_string(), &name));

        let color = widgets::color_button(&scene.background);
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = scene_id.clone();
            color.connect_notify_local(Some("rgba"), move |button, _| {
                if updating.get() {
                    return;
                }
                let hex = widgets::color_hex(button);
                controller.mutate(Some("scene-background"), |project| {
                    if let Some(scene) = project.scene_mut(&id) {
                        scene.background = hex.clone();
                    }
                });
            });
        }
        body.append(&widgets::labelled(
            &t!("inspector.background").to_string(),
            &color,
        ));

        let advance = widgets::switch(scene.advance_on_media_end);
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = scene_id.clone();
            advance.connect_notify_local(Some("active"), move |switch, _| {
                if updating.get() {
                    return;
                }
                let value = switch.is_active();
                controller.mutate(Some("scene-advance"), |project| {
                    if let Some(scene) = project.scene_mut(&id) {
                        scene.advance_on_media_end = value;
                    }
                });
            });
        }
        body.append(&widgets::labelled(
            &t!("inspector.advance_on_media_end").to_string(),
            &advance,
        ));

        {
            let controller = self.controller.clone();
            let name = name.clone();
            let color = color.clone();
            let advance = advance.clone();
            self.push_updater(move || {
                let Some(scene) = controller.editing_scene() else {
                    return;
                };
                set_entry_text(&name, &scene.name);
                set_color_value(&color, &scene.background);
                set_switch_state(&advance, scene.advance_on_media_end);
            });
        }
    }

    // ------------------------------------------------------------- layer

    fn build_layer(&self, layer: &Layer) {
        self.build_common(layer);
        match &layer.kind {
            LayerKind::Text(props) => self.build_text(layer, props),
            LayerKind::Clock(props) => self.build_clock(layer, props),
            LayerKind::Timer(props) => self.build_timer(layer, props),
            LayerKind::Eta(props) => self.build_eta(layer, props),
            LayerKind::Image(props) => self.build_image(layer, props),
            LayerKind::Video(props) => self.build_video(layer, props),
            LayerKind::Capture(props) => self.build_capture(layer, props),
            LayerKind::Queue(props) => self.build_queue(layer, props),
            LayerKind::Ticker(props) => self.build_ticker(layer, props),
            LayerKind::Pattern(props) => self.build_pattern(layer, props),
        }
    }

    fn build_common(&self, layer: &Layer) {
        let body = self.add_section(&t!("inspector.layer").to_string());
        body.append(&widgets::dim_label(&t!(layer.kind.label_key()).to_string()));

        let name = widgets::entry(&layer.name);
        bind_entry(
            &name,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-name",
            |layer, value| layer.name = value.to_string(),
        );
        body.append(&widgets::field(&t!("inspector.name").to_string(), &name));

        let x = widgets::spin(0.0, 7680.0, 1.0, layer.rect.x);
        let y = widgets::spin(0.0, 7680.0, 1.0, layer.rect.y);
        let width = widgets::spin(0.0, 7680.0, 1.0, layer.rect.w);
        let height = widgets::spin(0.0, 7680.0, 1.0, layer.rect.h);
        bind_spin(
            &x,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-x",
            |layer, value| layer.rect.x = value,
        );
        bind_spin(
            &y,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-y",
            |layer, value| layer.rect.y = value,
        );
        bind_spin(
            &width,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-width",
            |layer, value| layer.rect.w = value,
        );
        bind_spin(
            &height,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-height",
            |layer, value| layer.rect.h = value,
        );
        let grid = gtk::Grid::builder()
            .column_spacing(8)
            .row_spacing(6)
            .build();
        grid.attach(
            &widgets::field(&t!("inspector.x").to_string(), &x),
            0,
            0,
            1,
            1,
        );
        grid.attach(
            &widgets::field(&t!("inspector.y").to_string(), &y),
            1,
            0,
            1,
            1,
        );
        grid.attach(
            &widgets::field(&t!("inspector.width").to_string(), &width),
            0,
            1,
            1,
            1,
        );
        grid.attach(
            &widgets::field(&t!("inspector.height").to_string(), &height),
            1,
            1,
            1,
            1,
        );
        body.append(&widgets::field(
            &t!("inspector.position").to_string(),
            &grid,
        ));

        let opacity = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        opacity.set_digits(2);
        opacity.set_draw_value(true);
        opacity.set_value_pos(gtk::PositionType::Right);
        opacity.set_hexpand(true);
        opacity.set_value(layer.opacity);
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = layer.id.clone();
            opacity.connect_value_changed(move |scale| {
                if updating.get() {
                    return;
                }
                let value = scale.value();
                controller.update_layer(&id, Some("inspector-opacity"), |layer| {
                    layer.opacity = value;
                });
            });
        }
        body.append(&widgets::labelled(
            &t!("inspector.opacity").to_string(),
            &opacity,
        ));

        let visible = widgets::switch(layer.visible);
        bind_switch(
            &visible,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-visible",
            |layer, value| layer.visible = value,
        );
        body.append(&widgets::labelled(
            &t!("inspector.visible").to_string(),
            &visible,
        ));

        {
            let controller = self.controller.clone();
            let id = layer.id.clone();
            let name = name.clone();
            let x = x.clone();
            let y = y.clone();
            let width = width.clone();
            let height = height.clone();
            let opacity = opacity.clone();
            let visible = visible.clone();
            self.push_updater(move || {
                let Some(layer) = controller.layer(&id) else {
                    return;
                };
                set_entry_text(&name, &layer.name);
                set_spin_value(&x, layer.rect.x);
                set_spin_value(&y, layer.rect.y);
                set_spin_value(&width, layer.rect.w);
                set_spin_value(&height, layer.rect.h);
                set_scale_value(&opacity, layer.opacity);
                set_switch_state(&visible, layer.visible);
            });
        }
    }

    // -------------------------------------------------------------- text

    fn build_text(&self, layer: &Layer, props: &crate::model::layer::TextProps) {
        let body = self.add_section(&t!("layer.type.text").to_string());

        let this_view = gtk::TextView::new();
        this_view.set_wrap_mode(gtk::WrapMode::WordChar);
        this_view.set_accepts_tab(false);
        this_view.buffer().set_text(&props.text);
        let text_scroll = gtk::ScrolledWindow::builder()
            .child(&this_view)
            .min_content_height(110)
            .has_frame(true)
            .build();
        body.append(&widgets::field(
            &t!("inspector.text").to_string(),
            &text_scroll,
        ));
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = layer.id.clone();
            this_view.buffer().connect_changed(move |buffer| {
                if updating.get() {
                    return;
                }
                let value = buffer
                    .text(&buffer.start_iter(), &buffer.end_iter(), false)
                    .to_string();
                controller.update_layer(&id, Some("inspector-text"), |layer| {
                    if let LayerKind::Text(props) = &mut layer.kind {
                        props.text = value.clone();
                    }
                });
            });
        }

        let (background_row, background_switch, background_color) =
            option_color_row(props.background.as_deref());
        body.append(&widgets::labelled(
            &t!("inspector.background").to_string(),
            &background_row,
        ));
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = layer.id.clone();
            let color = background_color.clone();
            background_switch.connect_notify_local(Some("active"), move |switch, _| {
                if updating.get() {
                    return;
                }
                let enabled = switch.is_active();
                color.set_sensitive(enabled);
                let hex = widgets::color_hex(&color);
                controller.update_layer(&id, Some("inspector-bg-enabled"), |layer| {
                    if let LayerKind::Text(props) = &mut layer.kind {
                        props.background = if enabled { Some(hex.clone()) } else { None };
                    }
                });
            });
        }
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = layer.id.clone();
            background_color.connect_notify_local(Some("rgba"), move |button, _| {
                if updating.get() {
                    return;
                }
                let hex = widgets::color_hex(button);
                controller.update_layer(&id, Some("inspector-bg-color"), |layer| {
                    if let LayerKind::Text(props) = &mut layer.kind {
                        props.background = Some(hex.clone());
                    }
                });
            });
        }

        let padding = widgets::spin(0.0, 400.0, 1.0, props.padding);
        bind_spin(
            &padding,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-padding",
            |layer, value| {
                if let LayerKind::Text(props) = &mut layer.kind {
                    props.padding = value;
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.padding").to_string(),
            &padding,
        ));

        self.style_controls(&body, layer, &props.style);

        {
            let controller = self.controller.clone();
            let id = layer.id.clone();
            let buffer = this_view.buffer();
            let background_switch = background_switch.clone();
            let background_color = background_color.clone();
            let padding = padding.clone();
            self.push_updater(move || {
                let Some(layer) = controller.layer(&id) else {
                    return;
                };
                let LayerKind::Text(props) = &layer.kind else {
                    return;
                };
                if buffer.text(&buffer.start_iter(), &buffer.end_iter(), false) != props.text {
                    buffer.set_text(&props.text);
                }
                set_switch_state(&background_switch, props.background.is_some());
                background_color.set_sensitive(props.background.is_some());
                if let Some(hex) = &props.background {
                    set_color_value(&background_color, hex);
                }
                set_spin_value(&padding, props.padding);
            });
        }
    }

    // ------------------------------------------------------------- clock

    fn build_clock(&self, layer: &Layer, props: &crate::model::layer::ClockProps) {
        let body = self.add_section(&t!("layer.type.clock").to_string());
        let presets = [
            ClockPreset::H24,
            ClockPreset::H24Seconds,
            ClockPreset::H12,
            ClockPreset::H12Seconds,
        ];
        let labels: Vec<String> = presets
            .iter()
            .map(|preset| t!(clock_preset_key(*preset)).to_string())
            .collect();
        let selected = presets
            .iter()
            .position(|preset| *preset == props.preset)
            .unwrap_or(0);
        let preset = widgets::dropdown(&labels, selected);
        bind_dropdown(
            &preset,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-clock-preset",
            move |layer, index| {
                if let LayerKind::Clock(props) = &mut layer.kind
                    && let Some(preset) = presets.get(index)
                {
                    props.preset = *preset;
                }
            },
        );
        body.append(&widgets::field(
            &t!("inspector.preset").to_string(),
            &preset,
        ));

        let show_date = widgets::switch(props.show_date);
        bind_switch(
            &show_date,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-show-date",
            |layer, value| {
                if let LayerKind::Clock(props) = &mut layer.kind {
                    props.show_date = value;
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.show_date").to_string(),
            &show_date,
        ));

        self.style_controls(&body, layer, &props.style);
        self.push_kind_updater(layer, move |layer| {
            if let LayerKind::Clock(props) = &layer.kind {
                if let Some(index) = presets.iter().position(|preset| *preset == props.preset) {
                    set_dropdown_index(&preset, index);
                }
                set_switch_state(&show_date, props.show_date);
            }
        });
    }

    // ------------------------------------------------------------- timer

    fn build_timer(&self, layer: &Layer, props: &crate::model::layer::TimerProps) {
        let body = self.add_section(&t!("layer.type.timer").to_string());
        let options = self
            .controller
            .with_project(|project| timer_options(project, &props.timer_id));
        let options = Rc::new(RefCell::new(options));
        let labels: Vec<String> = options
            .borrow()
            .iter()
            .map(|(_, label)| label.clone())
            .collect();
        let selected = options
            .borrow()
            .iter()
            .position(|(id, _)| id == &props.timer_id)
            .unwrap_or(0);
        let timer = widgets::dropdown(&labels, selected);
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = layer.id.clone();
            let options_signal = options.clone();
            timer.connect_selected_notify(move |dropdown| {
                if updating.get() {
                    return;
                }
                let index = dropdown.selected() as usize;
                let value = options_signal
                    .borrow()
                    .get(index)
                    .map(|(id, _)| id.clone())
                    .unwrap_or_default();
                controller.update_layer(&id, Some("inspector-timer"), |layer| {
                    if let LayerKind::Timer(props) = &mut layer.kind {
                        props.timer_id = value.clone();
                    }
                });
            });
        }
        body.append(&widgets::field(&t!("inspector.timer").to_string(), &timer));

        let modes = [TimerMode::Elapsed, TimerMode::Remaining, TimerMode::EndAt];
        let mode_labels: Vec<String> = modes
            .iter()
            .map(|mode| t!(timer_mode_key(*mode)).to_string())
            .collect();
        let mode_selected = modes
            .iter()
            .position(|mode| *mode == props.mode)
            .unwrap_or(0);
        let mode = widgets::dropdown(&mode_labels, mode_selected);
        bind_dropdown(
            &mode,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-timer-mode",
            move |layer, index| {
                if let LayerKind::Timer(props) = &mut layer.kind
                    && let Some(mode) = modes.get(index)
                {
                    props.mode = *mode;
                }
            },
        );
        body.append(&widgets::field(&t!("inspector.mode").to_string(), &mode));

        let warning = widgets::color_button(&props.warning_color);
        bind_color(
            &warning,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-warning-color",
            |layer, hex| {
                if let LayerKind::Timer(props) = &mut layer.kind {
                    props.warning_color = hex.to_string();
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.warning_color").to_string(),
            &warning,
        ));

        self.style_controls(&body, layer, &props.style);
        {
            let controller = self.controller.clone();
            let id = layer.id.clone();
            let options = options.clone();
            let timer = timer.clone();
            self.push_updater(move || {
                let Some(layer) = controller.layer(&id) else {
                    return;
                };
                let LayerKind::Timer(props) = &layer.kind else {
                    return;
                };
                let desired =
                    controller.with_project(|project| timer_options(project, &props.timer_id));
                let changed = {
                    let mut current = options.borrow_mut();
                    if *current != desired {
                        *current = desired;
                        true
                    } else {
                        false
                    }
                };
                if changed {
                    let labels: Vec<String> = options
                        .borrow()
                        .iter()
                        .map(|(_, label)| label.clone())
                        .collect();
                    let refs: Vec<&str> = labels.iter().map(|label| label.as_str()).collect();
                    timer.set_model(Some(&gtk::StringList::new(&refs)));
                }
                if let Some(index) = options
                    .borrow()
                    .iter()
                    .position(|(id, _)| id == &props.timer_id)
                {
                    set_dropdown_index(&timer, index);
                }
                if let Some(index) = modes.iter().position(|mode| *mode == props.mode) {
                    set_dropdown_index(&mode, index);
                }
                set_color_value(&warning, &props.warning_color);
            });
        }
    }

    // --------------------------------------------------------------- eta

    fn build_eta(&self, layer: &Layer, props: &crate::model::layer::EtaProps) {
        let body = self.add_section(&t!("layer.type.eta").to_string());
        let label = widgets::entry(&props.label);
        label.set_placeholder_text(Some(&t!("layer.eta.default_label").to_string()));
        bind_entry(
            &label,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-eta-label",
            |layer, value| {
                if let LayerKind::Eta(props) = &mut layer.kind {
                    props.label = value.to_string();
                }
            },
        );
        body.append(&widgets::field(&t!("inspector.label").to_string(), &label));
        body.append(&widgets::dim_label(&t!("inspector.label_hint").to_string()));

        self.style_controls(&body, layer, &props.style);
        self.push_kind_updater(layer, move |layer| {
            if let LayerKind::Eta(props) = &layer.kind {
                set_entry_text(&label, &props.label);
            }
        });
    }

    // ------------------------------------------------------------- image

    fn build_image(&self, layer: &Layer, props: &crate::model::layer::ImageProps) {
        let body = self.add_section(&t!("layer.type.image").to_string());
        self.asset_row(&body, layer, AssetFilter::Image, props.asset.as_deref());
        self.fit_row(&body, layer, props.fit);
    }

    // ------------------------------------------------------------- video

    fn build_video(&self, layer: &Layer, props: &crate::model::layer::VideoProps) {
        let body = self.add_section(&t!("layer.type.video").to_string());
        self.asset_row(&body, layer, AssetFilter::Video, props.asset.as_deref());
        self.fit_row(&body, layer, props.fit);

        let r#loop = widgets::switch(props.r#loop);
        bind_switch(
            &r#loop,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-loop",
            |layer, value| {
                if let LayerKind::Video(props) = &mut layer.kind {
                    props.r#loop = value;
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.loop").to_string(),
            &r#loop,
        ));

        let volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        volume.set_digits(2);
        volume.set_draw_value(true);
        volume.set_value_pos(gtk::PositionType::Right);
        volume.set_hexpand(true);
        volume.set_value(props.volume);
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = layer.id.clone();
            volume.connect_value_changed(move |scale| {
                if updating.get() {
                    return;
                }
                let value = scale.value();
                controller.update_layer(&id, Some("inspector-volume"), |layer| {
                    if let LayerKind::Video(props) = &mut layer.kind {
                        props.volume = value;
                    }
                });
            });
        }
        body.append(&widgets::labelled(
            &t!("inspector.volume").to_string(),
            &volume,
        ));

        self.push_kind_updater(layer, move |layer| {
            if let LayerKind::Video(props) = &layer.kind {
                set_switch_state(&r#loop, props.r#loop);
                set_scale_value(&volume, props.volume);
            }
        });
    }

    // ----------------------------------------------------------- capture

    fn build_capture(&self, layer: &Layer, props: &CaptureProps) {
        let body = self.add_section(&t!("layer.type.capture").to_string());

        let kinds = ["camera", "x11_window", "portal"];
        let labels = vec![
            t!("capture.type.camera").to_string(),
            t!("capture.type.x11_window").to_string(),
            t!("capture.type.portal").to_string(),
        ];
        let selected = kinds
            .iter()
            .position(|kind| *kind == props.source.kind_id())
            .unwrap_or(0);
        let source_type = widgets::dropdown(&labels, selected);
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = layer.id.clone();
            source_type.connect_selected_notify(move |dropdown| {
                if updating.get() {
                    return;
                }
                let source = match dropdown.selected() {
                    1 => CaptureSource::X11Window {
                        title: String::new(),
                        wm_class: String::new(),
                        xid: 0,
                    },
                    2 => CaptureSource::Portal {
                        label: String::new(),
                        restore_token: None,
                    },
                    _ => CaptureSource::Camera {
                        device: String::new(),
                        label: String::new(),
                        width: None,
                        height: None,
                        fps: None,
                    },
                };
                // Switching the source kind rebuilds the panel; defer the edit
                // so the widget is not destroyed inside its own signal.
                let controller = controller.clone();
                let id = id.clone();
                gtk::glib::idle_add_local_once(move || {
                    controller.update_layer(&id, Some("inspector-capture-type"), |layer| {
                        layer.kind.set_capture_source(source.clone());
                    });
                });
            });
        }
        body.append(&widgets::field(
            &t!("inspector.source").to_string(),
            &source_type,
        ));

        let mirror = widgets::switch(props.mirror);
        bind_switch(
            &mirror,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-mirror",
            |layer, value| {
                if let LayerKind::Capture(props) = &mut layer.kind {
                    props.mirror = value;
                }
            },
        );

        match &props.source {
            CaptureSource::Camera {
                device,
                width,
                height,
                fps,
                ..
            } => {
                let device_entry = widgets::entry(device);
                device_entry.set_placeholder_text(Some("/dev/video0"));
                bind_entry(
                    &device_entry,
                    &self.controller,
                    &self.updating,
                    &layer.id,
                    "inspector-capture-device",
                    |layer, value| {
                        if let LayerKind::Capture(props) = &mut layer.kind
                            && let CaptureSource::Camera { device, .. } = &mut props.source
                        {
                            *device = value.to_string();
                        }
                    },
                );
                body.append(&widgets::field(
                    &t!("inspector.device").to_string(),
                    &device_entry,
                ));

                body.append(&widgets::dim_label(&t!("capture.advanced").to_string()));
                let width_spin = widgets::spin(0.0, 7680.0, 1.0, width.map_or(0.0, f64::from));
                let height_spin = widgets::spin(0.0, 7680.0, 1.0, height.map_or(0.0, f64::from));
                let fps_spin = widgets::spin(0.0, 240.0, 1.0, fps.map_or(0.0, f64::from));
                bind_spin(
                    &width_spin,
                    &self.controller,
                    &self.updating,
                    &layer.id,
                    "inspector-capture-width",
                    |layer, value| {
                        if let LayerKind::Capture(props) = &mut layer.kind
                            && let CaptureSource::Camera { width, .. } = &mut props.source
                        {
                            *width = option_u32(value);
                        }
                    },
                );
                bind_spin(
                    &height_spin,
                    &self.controller,
                    &self.updating,
                    &layer.id,
                    "inspector-capture-height",
                    |layer, value| {
                        if let LayerKind::Capture(props) = &mut layer.kind
                            && let CaptureSource::Camera { height, .. } = &mut props.source
                        {
                            *height = option_u32(value);
                        }
                    },
                );
                bind_spin(
                    &fps_spin,
                    &self.controller,
                    &self.updating,
                    &layer.id,
                    "inspector-capture-fps",
                    |layer, value| {
                        if let LayerKind::Capture(props) = &mut layer.kind
                            && let CaptureSource::Camera { fps, .. } = &mut props.source
                        {
                            *fps = option_u32(value);
                        }
                    },
                );
                let advanced = gtk::Grid::builder()
                    .column_spacing(8)
                    .row_spacing(6)
                    .build();
                advanced.attach(
                    &widgets::field(&t!("capture.width").to_string(), &width_spin),
                    0,
                    0,
                    1,
                    1,
                );
                advanced.attach(
                    &widgets::field(&t!("capture.height").to_string(), &height_spin),
                    1,
                    0,
                    1,
                    1,
                );
                advanced.attach(
                    &widgets::field(&t!("capture.fps").to_string(), &fps_spin),
                    0,
                    1,
                    1,
                    1,
                );
                body.append(&advanced);

                let test = widgets::button(&t!("capture.test").to_string());
                {
                    let controller = self.controller.clone();
                    let id = layer.id.clone();
                    test.connect_clicked(move |button| {
                        let parent = button
                            .root()
                            .and_then(|root| root.downcast::<gtk::Window>().ok());
                        if let Some(parent) = parent {
                            crate::services::capture::test_window(&parent, controller.clone(), &id);
                        }
                    });
                }
                body.append(&test);

                {
                    let controller = self.controller.clone();
                    let id = layer.id.clone();
                    self.push_updater(move || {
                        let Some(layer) = controller.layer(&id) else {
                            return;
                        };
                        let LayerKind::Capture(props) = &layer.kind else {
                            return;
                        };
                        let CaptureSource::Camera {
                            device,
                            width,
                            height,
                            fps,
                            ..
                        } = &props.source
                        else {
                            return;
                        };
                        set_entry_text(&device_entry, device);
                        set_spin_value(&width_spin, width.map_or(0.0, f64::from));
                        set_spin_value(&height_spin, height.map_or(0.0, f64::from));
                        set_spin_value(&fps_spin, fps.map_or(0.0, f64::from));
                    });
                }
            }
            CaptureSource::X11Window {
                title,
                wm_class,
                xid,
            } => {
                let title_entry = widgets::entry(title);
                bind_entry(
                    &title_entry,
                    &self.controller,
                    &self.updating,
                    &layer.id,
                    "inspector-capture-title",
                    |layer, value| {
                        if let LayerKind::Capture(props) = &mut layer.kind
                            && let CaptureSource::X11Window { title, .. } = &mut props.source
                        {
                            *title = value.to_string();
                        }
                    },
                );
                body.append(&widgets::field(
                    &t!("inspector.window_title").to_string(),
                    &title_entry,
                ));

                let class_entry = widgets::entry(wm_class);
                bind_entry(
                    &class_entry,
                    &self.controller,
                    &self.updating,
                    &layer.id,
                    "inspector-capture-class",
                    |layer, value| {
                        if let LayerKind::Capture(props) = &mut layer.kind
                            && let CaptureSource::X11Window { wm_class, .. } = &mut props.source
                        {
                            *wm_class = value.to_string();
                        }
                    },
                );
                body.append(&widgets::field(
                    &t!("inspector.window_class").to_string(),
                    &class_entry,
                ));

                let xid_entry = widgets::entry(&xid.to_string());
                bind_entry(
                    &xid_entry,
                    &self.controller,
                    &self.updating,
                    &layer.id,
                    "inspector-capture-xid",
                    |layer, value| {
                        if let Ok(parsed) = value.trim().parse::<u32>()
                            && let LayerKind::Capture(props) = &mut layer.kind
                            && let CaptureSource::X11Window { xid, .. } = &mut props.source
                        {
                            *xid = parsed;
                        }
                    },
                );
                body.append(&widgets::field(
                    &t!("inspector.xid").to_string(),
                    &xid_entry,
                ));

                let pick = widgets::button(&t!("capture.pick_window").to_string());
                {
                    let controller = self.controller.clone();
                    let overlay = self.overlay.clone();
                    let id = layer.id.clone();
                    pick.connect_clicked(move |button| {
                        let parent = button
                            .root()
                            .and_then(|root| root.downcast::<gtk::Window>().ok());
                        pick_x11_window(parent.as_ref(), &controller, &overlay, &id);
                    });
                }
                body.append(&pick);

                {
                    let controller = self.controller.clone();
                    let id = layer.id.clone();
                    self.push_updater(move || {
                        let Some(layer) = controller.layer(&id) else {
                            return;
                        };
                        let LayerKind::Capture(props) = &layer.kind else {
                            return;
                        };
                        let CaptureSource::X11Window {
                            title,
                            wm_class,
                            xid,
                        } = &props.source
                        else {
                            return;
                        };
                        set_entry_text(&title_entry, title);
                        set_entry_text(&class_entry, wm_class);
                        set_entry_text(&xid_entry, &xid.to_string());
                    });
                }
            }
            CaptureSource::Portal { label, .. } => {
                let label_entry = widgets::entry(label);
                label_entry.set_placeholder_text(Some(&t!("capture.unset").to_string()));
                bind_entry(
                    &label_entry,
                    &self.controller,
                    &self.updating,
                    &layer.id,
                    "inspector-capture-portal-label",
                    |layer, value| {
                        if let LayerKind::Capture(props) = &mut layer.kind
                            && let CaptureSource::Portal { label, .. } = &mut props.source
                        {
                            *label = value.to_string();
                        }
                    },
                );
                body.append(&widgets::field(
                    &t!("inspector.label").to_string(),
                    &label_entry,
                ));

                {
                    let controller = self.controller.clone();
                    let id = layer.id.clone();
                    self.push_updater(move || {
                        let Some(layer) = controller.layer(&id) else {
                            return;
                        };
                        let LayerKind::Capture(props) = &layer.kind else {
                            return;
                        };
                        let CaptureSource::Portal { label, .. } = &props.source else {
                            return;
                        };
                        set_entry_text(&label_entry, label);
                    });
                }
            }
        }

        body.append(&widgets::labelled(
            &t!("inspector.mirror").to_string(),
            &mirror,
        ));
        self.fit_row(&body, layer, props.fit);

        {
            let controller = self.controller.clone();
            let id = layer.id.clone();
            self.push_updater(move || {
                let Some(layer) = controller.layer(&id) else {
                    return;
                };
                if let LayerKind::Capture(props) = &layer.kind {
                    set_switch_state(&mirror, props.mirror);
                }
            });
        }
    }

    // ------------------------------------------------------------- queue

    fn build_queue(&self, layer: &Layer, props: &crate::model::layer::QueueProps) {
        let body = self.add_section(&t!("layer.type.queue").to_string());

        let title = widgets::entry(&props.title);
        title.set_placeholder_text(Some(&t!("queue.default_title").to_string()));
        bind_entry(
            &title,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-queue-title",
            |layer, value| {
                if let LayerKind::Queue(props) = &mut layer.kind {
                    props.title = value.to_string();
                }
            },
        );
        body.append(&widgets::field(
            &t!("inspector.queue_title").to_string(),
            &title,
        ));

        let empty = widgets::entry(&props.empty_text);
        empty.set_placeholder_text(Some(&t!("queue.display_empty").to_string()));
        bind_entry(
            &empty,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-queue-empty",
            |layer, value| {
                if let LayerKind::Queue(props) = &mut layer.kind {
                    props.empty_text = value.to_string();
                }
            },
        );
        body.append(&widgets::field(
            &t!("inspector.queue_empty").to_string(),
            &empty,
        ));

        let upcoming = widgets::int_spin(1, 50, props.upcoming as i32);
        bind_spin(
            &upcoming,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-queue-upcoming",
            |layer, value| {
                if let LayerKind::Queue(props) = &mut layer.kind {
                    props.upcoming = value.max(1.0) as u32;
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.upcoming").to_string(),
            &upcoming,
        ));

        let show_ticket = widgets::switch(props.show_ticket);
        bind_switch(
            &show_ticket,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-queue-ticket",
            |layer, value| {
                if let LayerKind::Queue(props) = &mut layer.kind {
                    props.show_ticket = value;
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.show_ticket").to_string(),
            &show_ticket,
        ));

        let show_last_called = widgets::switch(props.show_last_called);
        bind_switch(
            &show_last_called,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-queue-last-called",
            |layer, value| {
                if let LayerKind::Queue(props) = &mut layer.kind {
                    props.show_last_called = value;
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.show_last_called").to_string(),
            &show_last_called,
        ));

        let show_next = widgets::switch(props.show_next);
        bind_switch(
            &show_next,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-queue-show-next",
            |layer, value| {
                if let LayerKind::Queue(props) = &mut layer.kind {
                    props.show_next = value;
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.show_next").to_string(),
            &show_next,
        ));

        let show_note = widgets::switch(props.show_note);
        bind_switch(
            &show_note,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-queue-note",
            |layer, value| {
                if let LayerKind::Queue(props) = &mut layer.kind {
                    props.show_note = value;
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.show_note").to_string(),
            &show_note,
        ));

        let accent = widgets::color_button(&props.accent);
        bind_color(
            &accent,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-queue-accent",
            |layer, hex| {
                if let LayerKind::Queue(props) = &mut layer.kind {
                    props.accent = hex.to_string();
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.accent").to_string(),
            &accent,
        ));

        self.style_controls(&body, layer, &props.style);
        self.push_kind_updater(layer, move |layer| {
            if let LayerKind::Queue(props) = &layer.kind {
                set_entry_text(&title, &props.title);
                set_entry_text(&empty, &props.empty_text);
                set_spin_value(&upcoming, f64::from(props.upcoming));
                set_switch_state(&show_ticket, props.show_ticket);
                set_switch_state(&show_note, props.show_note);
                set_color_value(&accent, &props.accent);
            }
        });
    }

    // ------------------------------------------------------------ ticker

    fn build_ticker(&self, layer: &Layer, props: &crate::model::layer::TickerProps) {
        let body = self.add_section(&t!("layer.type.ticker").to_string());

        let text = widgets::entry(&props.text);
        bind_entry(
            &text,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-ticker-text",
            |layer, value| {
                if let LayerKind::Ticker(props) = &mut layer.kind {
                    props.text = value.to_string();
                }
            },
        );
        body.append(&widgets::field(&t!("inspector.text").to_string(), &text));

        let speed = widgets::spin(10.0, 600.0, 1.0, props.speed);
        bind_spin(
            &speed,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-ticker-speed",
            |layer, value| {
                if let LayerKind::Ticker(props) = &mut layer.kind {
                    props.speed = value;
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.speed").to_string(),
            &speed,
        ));

        let gap = widgets::spin(0.0, 2000.0, 1.0, props.gap);
        bind_spin(
            &gap,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-ticker-gap",
            |layer, value| {
                if let LayerKind::Ticker(props) = &mut layer.kind {
                    props.gap = value;
                }
            },
        );
        body.append(&widgets::labelled(&t!("inspector.gap").to_string(), &gap));

        let (background_row, background_switch, background_color) =
            option_color_row(props.background.as_deref());
        body.append(&widgets::labelled(
            &t!("inspector.background").to_string(),
            &background_row,
        ));
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = layer.id.clone();
            let color = background_color.clone();
            background_switch.connect_notify_local(Some("active"), move |switch, _| {
                if updating.get() {
                    return;
                }
                let enabled = switch.is_active();
                color.set_sensitive(enabled);
                let hex = widgets::color_hex(&color);
                controller.update_layer(&id, Some("inspector-bg-enabled"), |layer| {
                    if let LayerKind::Ticker(props) = &mut layer.kind {
                        props.background = if enabled { Some(hex.clone()) } else { None };
                    }
                });
            });
        }
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = layer.id.clone();
            background_color.connect_notify_local(Some("rgba"), move |button, _| {
                if updating.get() {
                    return;
                }
                let hex = widgets::color_hex(button);
                controller.update_layer(&id, Some("inspector-bg-color"), |layer| {
                    if let LayerKind::Ticker(props) = &mut layer.kind {
                        props.background = Some(hex.clone());
                    }
                });
            });
        }

        self.style_controls(&body, layer, &props.style);
        {
            let controller = self.controller.clone();
            let id = layer.id.clone();
            self.push_updater(move || {
                let Some(layer) = controller.layer(&id) else {
                    return;
                };
                let LayerKind::Ticker(props) = &layer.kind else {
                    return;
                };
                set_entry_text(&text, &props.text);
                set_spin_value(&speed, props.speed);
                set_spin_value(&gap, props.gap);
                set_switch_state(&background_switch, props.background.is_some());
                background_color.set_sensitive(props.background.is_some());
                if let Some(hex) = &props.background {
                    set_color_value(&background_color, hex);
                }
            });
        }
    }

    // ----------------------------------------------------------- pattern

    fn build_pattern(&self, layer: &Layer, props: &crate::model::layer::PatternProps) {
        let body = self.add_section(&t!("layer.type.pattern").to_string());
        let labels: Vec<String> = PatternKind::ALL
            .iter()
            .map(|pattern| t!(pattern.label_key()).to_string())
            .collect();
        let selected = PatternKind::ALL
            .iter()
            .position(|pattern| *pattern == props.pattern)
            .unwrap_or(0);
        let pattern = widgets::dropdown(&labels, selected);
        bind_dropdown(
            &pattern,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-pattern",
            move |layer, index| {
                if let LayerKind::Pattern(props) = &mut layer.kind
                    && let Some(kind) = PatternKind::ALL.get(index)
                {
                    props.pattern = *kind;
                }
            },
        );
        body.append(&widgets::field(
            &t!("inspector.pattern").to_string(),
            &pattern,
        ));

        let color = widgets::color_button(&props.color);
        bind_color(
            &color,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-pattern-color",
            |layer, hex| {
                if let LayerKind::Pattern(props) = &mut layer.kind {
                    props.color = hex.to_string();
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.color").to_string(),
            &color,
        ));

        self.push_kind_updater(layer, move |layer| {
            if let LayerKind::Pattern(props) = &layer.kind {
                if let Some(index) = PatternKind::ALL
                    .iter()
                    .position(|pattern| *pattern == props.pattern)
                {
                    set_dropdown_index(&pattern, index);
                }
                set_color_value(&color, &props.color);
            }
        });
    }

    // ------------------------------------------------------- shared bits

    fn push_kind_updater(&self, layer: &Layer, update: impl Fn(&Layer) + 'static) {
        let controller = self.controller.clone();
        let id = layer.id.clone();
        self.push_updater(move || {
            if let Some(layer) = controller.layer(&id) {
                update(&layer);
            }
        });
    }

    fn fit_row(&self, body: &gtk::Box, layer: &Layer, fit: Fit) {
        let fits = [Fit::Contain, Fit::Cover, Fit::Stretch];
        let labels = vec![
            t!("inspector.fit_contain").to_string(),
            t!("inspector.fit_cover").to_string(),
            t!("inspector.fit_stretch").to_string(),
        ];
        let selected = fits.iter().position(|value| *value == fit).unwrap_or(0);
        let dropdown = widgets::dropdown(&labels, selected);
        bind_dropdown(
            &dropdown,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-fit",
            move |layer, index| {
                if let Some(fit) = fits.get(index) {
                    match &mut layer.kind {
                        LayerKind::Image(props) => props.fit = *fit,
                        LayerKind::Video(props) => props.fit = *fit,
                        LayerKind::Capture(props) => props.fit = *fit,
                        _ => {}
                    }
                }
            },
        );
        body.append(&widgets::field(&t!("inspector.fit").to_string(), &dropdown));
        self.push_kind_updater(layer, move |layer| {
            let current = match &layer.kind {
                LayerKind::Image(props) => props.fit,
                LayerKind::Video(props) => props.fit,
                LayerKind::Capture(props) => props.fit,
                _ => return,
            };
            if let Some(index) = fits.iter().position(|value| *value == current) {
                set_dropdown_index(&dropdown, index);
            }
        });
    }

    fn asset_row(
        &self,
        body: &gtk::Box,
        layer: &Layer,
        filter: AssetFilter,
        current: Option<&str>,
    ) {
        let options = Rc::new(RefCell::new(asset_options(
            &self.controller,
            filter,
            current,
        )));
        let labels: Vec<String> = options
            .borrow()
            .iter()
            .map(|(_, label)| label.clone())
            .collect();
        let selected = options
            .borrow()
            .iter()
            .position(|(rel, _)| rel.as_deref() == current)
            .unwrap_or(0);
        let dropdown = widgets::dropdown(&labels, selected);
        dropdown.set_hexpand(true);
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = layer.id.clone();
            let options_signal = options.clone();
            dropdown.connect_selected_notify(move |dropdown| {
                if updating.get() {
                    return;
                }
                let index = dropdown.selected() as usize;
                let value = options_signal
                    .borrow()
                    .get(index)
                    .and_then(|(rel, _)| rel.clone());
                controller.update_layer(&id, Some("inspector-asset"), |layer| {
                    layer.kind.set_asset(value.clone());
                });
            });
        }

        let import = widgets::button(&t!("inspector.import").to_string());
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            let id = layer.id.clone();
            import.connect_clicked(move |_| {
                pick_asset_files(
                    &overlay,
                    &filter.label(),
                    filter.extensions(),
                    &controller,
                    &id,
                );
            });
        }

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&dropdown);
        row.append(&import);
        body.append(&widgets::field(&t!("inspector.asset").to_string(), &row));

        {
            let controller = self.controller.clone();
            let id = layer.id.clone();
            self.push_updater(move || {
                let Some(layer) = controller.layer(&id) else {
                    return;
                };
                let current = layer.kind.asset().map(|path| path.to_string());
                let desired = asset_options(&controller, filter, current.as_deref());
                let changed = {
                    let mut options = options.borrow_mut();
                    if *options != desired {
                        *options = desired;
                        true
                    } else {
                        false
                    }
                };
                if changed {
                    let labels: Vec<String> = options
                        .borrow()
                        .iter()
                        .map(|(_, label)| label.clone())
                        .collect();
                    let refs: Vec<&str> = labels.iter().map(|label| label.as_str()).collect();
                    dropdown.set_model(Some(&gtk::StringList::new(&refs)));
                }
                if let Some(index) = options
                    .borrow()
                    .iter()
                    .position(|(rel, _)| rel.as_deref() == current.as_deref())
                {
                    set_dropdown_index(&dropdown, index);
                }
            });
        }
    }

    fn style_controls(&self, body: &gtk::Box, layer: &Layer, style: &TextStyle) {
        body.append(&widgets::separator());
        let caption = gtk::Label::new(Some(&t!("inspector.style").to_string()));
        caption.set_xalign(0.0);
        caption.add_css_class("caption");
        body.append(&caption);

        let font = widgets::font_button(&style.family, style.size);
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = layer.id.clone();
            font.connect_notify_local(Some("font-desc"), move |button, _| {
                if updating.get() {
                    return;
                }
                let family = widgets::font_family(button);
                let size = widgets::font_size(button);
                controller.update_layer(&id, Some("inspector-style-font"), |layer| {
                    if let Some(style) = layer.kind.style_mut() {
                        style.family = family.clone();
                        style.size = size;
                    }
                });
            });
        }
        body.append(&widgets::labelled(
            &t!("inspector.family").to_string(),
            &font,
        ));

        let bold = gtk::CheckButton::with_label(&t!("inspector.bold").to_string());
        bold.set_active(style.bold);
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = layer.id.clone();
            bold.connect_notify_local(Some("active"), move |check, _| {
                if updating.get() {
                    return;
                }
                let value = check.is_active();
                controller.update_layer(&id, Some("inspector-style-bold"), |layer| {
                    if let Some(style) = layer.kind.style_mut() {
                        style.bold = value;
                    }
                });
            });
        }
        let italic = gtk::CheckButton::with_label(&t!("inspector.italic").to_string());
        italic.set_active(style.italic);
        {
            let controller = self.controller.clone();
            let updating = self.updating.clone();
            let id = layer.id.clone();
            italic.connect_notify_local(Some("active"), move |check, _| {
                if updating.get() {
                    return;
                }
                let value = check.is_active();
                controller.update_layer(&id, Some("inspector-style-italic"), |layer| {
                    if let Some(style) = layer.kind.style_mut() {
                        style.italic = value;
                    }
                });
            });
        }
        let emphasis = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        emphasis.append(&bold);
        emphasis.append(&italic);
        body.append(&emphasis);

        let alignments = [TextAlign::Left, TextAlign::Center, TextAlign::Right];
        let align_labels = vec![
            t!("inspector.align_left").to_string(),
            t!("inspector.align_center").to_string(),
            t!("inspector.align_right").to_string(),
        ];
        let align_selected = alignments
            .iter()
            .position(|value| *value == style.align)
            .unwrap_or(0);
        let align = widgets::dropdown(&align_labels, align_selected);
        bind_dropdown(
            &align,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-style-align",
            move |layer, index| {
                if let Some(value) = alignments.get(index)
                    && let Some(style) = layer.kind.style_mut()
                {
                    style.align = *value;
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.align").to_string(),
            &align,
        ));

        let spacing = widgets::spin(1.0, 4.0, 0.05, style.line_spacing);
        spacing.set_digits(2);
        bind_spin(
            &spacing,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-style-spacing",
            |layer, value| {
                if let Some(style) = layer.kind.style_mut() {
                    style.line_spacing = value;
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.line_spacing").to_string(),
            &spacing,
        ));

        let uppercase = widgets::switch(style.uppercase);
        bind_switch(
            &uppercase,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-style-uppercase",
            |layer, value| {
                if let Some(style) = layer.kind.style_mut() {
                    style.uppercase = value;
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.uppercase").to_string(),
            &uppercase,
        ));

        let color = widgets::color_button(&style.color);
        bind_color(
            &color,
            &self.controller,
            &self.updating,
            &layer.id,
            "inspector-style-color",
            |layer, hex| {
                if let Some(style) = layer.kind.style_mut() {
                    style.color = hex.to_string();
                }
            },
        );
        body.append(&widgets::labelled(
            &t!("inspector.color").to_string(),
            &color,
        ));

        {
            let controller = self.controller.clone();
            let id = layer.id.clone();
            self.push_updater(move || {
                let Some(style) = controller
                    .layer(&id)
                    .and_then(|layer| layer.kind.style().cloned())
                else {
                    return;
                };
                if widgets::font_family(&font) != style.family
                    || (widgets::font_size(&font) - style.size).abs() > 0.01
                {
                    widgets::set_font(&font, &style.family, style.size);
                }
                set_check_state(&bold, style.bold);
                set_check_state(&italic, style.italic);
                if let Some(index) = alignments.iter().position(|value| *value == style.align) {
                    set_dropdown_index(&align, index);
                }
                set_spin_value(&spacing, style.line_spacing);
                set_switch_state(&uppercase, style.uppercase);
                set_color_value(&color, &style.color);
            });
        }
    }
}

// ---------------------------------------------------------------- helpers

fn clock_preset_key(preset: ClockPreset) -> &'static str {
    match preset {
        ClockPreset::H24 => "inspector.preset_24h",
        ClockPreset::H24Seconds => "inspector.preset_24h_seconds",
        ClockPreset::H12 => "inspector.preset_12h",
        ClockPreset::H12Seconds => "inspector.preset_12h_seconds",
    }
}

fn timer_mode_key(mode: TimerMode) -> &'static str {
    match mode {
        TimerMode::Elapsed => "inspector.mode_elapsed",
        TimerMode::Remaining => "inspector.mode_remaining",
        TimerMode::EndAt => "inspector.mode_end_at",
    }
}

fn option_u32(value: f64) -> Option<u32> {
    if value.is_finite() && value >= 1.0 {
        Some(value as u32)
    } else {
        None
    }
}

fn format_duration(secs: u32) -> String {
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn timer_options(project: &Project, current: &str) -> Vec<(String, String)> {
    let mut options = Vec::new();
    let current = current.trim();
    if current.is_empty() {
        options.push((String::new(), t!("inspector.timer_none").to_string()));
    } else if !project.timers.iter().any(|timer| timer.id == current) {
        options.push((
            current.to_string(),
            t!("inspector.timer_missing").to_string(),
        ));
    }
    options.extend(project.timers.iter().map(timer_option));
    if options.is_empty() {
        options.push((String::new(), t!("inspector.timer_none").to_string()));
    }
    options
}

fn timer_option(timer: &TimerDef) -> (String, String) {
    let name = if timer.name.trim().is_empty() {
        timer.id.clone()
    } else {
        timer.name.clone()
    };
    let suffix = match timer.duration_secs {
        Some(secs) => format_duration(secs),
        None => t!("inspector.timer_count_up").to_string(),
    };
    (timer.id.clone(), format!("{name} · {suffix}"))
}

fn asset_options(
    controller: &Rc<Controller>,
    filter: AssetFilter,
    current: Option<&str>,
) -> Vec<(Option<String>, String)> {
    let mut options = vec![(None, t!("inspector.asset_none").to_string())];
    if let Some(dir) = controller.project_dir() {
        for entry in assets::list(&dir) {
            if filter.matches(entry.kind) {
                options.push((Some(entry.rel), entry.name));
            }
        }
    }
    if let Some(current) = current
        && !options
            .iter()
            .any(|(rel, _)| rel.as_deref() == Some(current))
    {
        options.push((
            Some(current.to_string()),
            t!("inspector.asset_missing").to_string(),
        ));
    }
    options
}

fn option_color_row(current: Option<&str>) -> (gtk::Box, gtk::Switch, gtk::ColorDialogButton) {
    let enabled = current.is_some();
    let switch = widgets::switch(enabled);
    let color = widgets::color_button(current.unwrap_or("#000000"));
    color.set_sensitive(enabled);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&switch);
    row.append(&color);
    (row, switch, color)
}

fn set_entry_text(entry: &gtk::Entry, text: &str) {
    if entry.text() != text {
        entry.set_text(text);
    }
}

fn set_spin_value(spin: &gtk::SpinButton, value: f64) {
    // Never rewrite the widget the user is typing into: the model clamps
    // out-of-range numbers, and replacing the text mid-edit would move the
    // cursor. The next refresh (after focus moves) syncs the display.
    if spin.has_focus() {
        return;
    }
    if (spin.value() - value).abs() > 1e-9 {
        spin.set_value(value);
    }
}

fn set_scale_value(scale: &gtk::Scale, value: f64) {
    if (scale.value() - value).abs() > 1e-9 {
        scale.set_value(value);
    }
}

fn set_switch_state(switch: &gtk::Switch, active: bool) {
    if switch.is_active() != active {
        switch.set_active(active);
    }
}

fn set_check_state(check: &gtk::CheckButton, active: bool) {
    if check.is_active() != active {
        check.set_active(active);
    }
}

fn set_dropdown_index(dropdown: &gtk::DropDown, index: usize) {
    if dropdown.selected() as usize != index {
        dropdown.set_selected(index as u32);
    }
}

fn set_color_value(button: &gtk::ColorDialogButton, hex: &str) {
    button.set_rgba(&widgets::gdk_color(hex));
}

fn bind_entry(
    entry: &gtk::Entry,
    controller: &Rc<Controller>,
    updating: &Rc<Cell<bool>>,
    layer_id: &str,
    key: &str,
    apply: impl Fn(&mut Layer, &str) + 'static,
) {
    let controller = controller.clone();
    let updating = updating.clone();
    let layer_id = layer_id.to_string();
    let key = key.to_string();
    entry.connect_changed(move |entry| {
        if updating.get() {
            return;
        }
        let value = entry.text().to_string();
        controller.update_layer(&layer_id, Some(key.as_str()), |layer| {
            apply(layer, &value);
        });
    });
}

fn bind_spin(
    spin: &gtk::SpinButton,
    controller: &Rc<Controller>,
    updating: &Rc<Cell<bool>>,
    layer_id: &str,
    key: &str,
    apply: impl Fn(&mut Layer, f64) + 'static,
) {
    let editing_controller = controller.clone();
    let editing_updating = updating.clone();
    let editing_layer_id = layer_id.to_string();
    let key = key.to_string();
    spin.connect_value_changed(move |spin| {
        if editing_updating.get() {
            return;
        }
        let value = spin.value();
        editing_controller.update_layer(&editing_layer_id, Some(key.as_str()), |layer| {
            apply(layer, value);
        });
    });

    // Normalisation may clamp the typed number; once the user leaves the
    // field, resync the display with the committed model value.
    let focus = gtk::EventControllerFocus::new();
    let focus_controller = controller.clone();
    let focus_updating = updating.clone();
    let focus_layer_id = layer_id.to_string();
    focus.connect_leave(move |_| {
        if focus_updating.get() {
            return;
        }
        // Only resync when this layer is still the selection; a blur caused by
        // switching layers must not select the old one again.
        if focus_controller.selected_layer().as_deref() != Some(focus_layer_id.as_str())
            || focus_controller.layer(&focus_layer_id).is_none()
        {
            return;
        }
        focus_controller.select_layer(Some(&focus_layer_id));
    });
    spin.add_controller(focus);
}

fn bind_switch(
    switch: &gtk::Switch,
    controller: &Rc<Controller>,
    updating: &Rc<Cell<bool>>,
    layer_id: &str,
    key: &str,
    apply: impl Fn(&mut Layer, bool) + 'static,
) {
    let controller = controller.clone();
    let updating = updating.clone();
    let layer_id = layer_id.to_string();
    let key = key.to_string();
    switch.connect_notify_local(Some("active"), move |switch, _| {
        if updating.get() {
            return;
        }
        let value = switch.is_active();
        controller.update_layer(&layer_id, Some(key.as_str()), |layer| {
            apply(layer, value);
        });
    });
}

fn bind_dropdown(
    dropdown: &gtk::DropDown,
    controller: &Rc<Controller>,
    updating: &Rc<Cell<bool>>,
    layer_id: &str,
    key: &str,
    apply: impl Fn(&mut Layer, usize) + 'static,
) {
    let controller = controller.clone();
    let updating = updating.clone();
    let layer_id = layer_id.to_string();
    let key = key.to_string();
    dropdown.connect_selected_notify(move |dropdown| {
        if updating.get() {
            return;
        }
        let index = dropdown.selected() as usize;
        controller.update_layer(&layer_id, Some(key.as_str()), |layer| {
            apply(layer, index);
        });
    });
}

fn bind_color(
    button: &gtk::ColorDialogButton,
    controller: &Rc<Controller>,
    updating: &Rc<Cell<bool>>,
    layer_id: &str,
    key: &str,
    apply: impl Fn(&mut Layer, &str) + 'static,
) {
    let controller = controller.clone();
    let updating = updating.clone();
    let layer_id = layer_id.to_string();
    let key = key.to_string();
    button.connect_notify_local(Some("rgba"), move |button, _| {
        if updating.get() {
            return;
        }
        let hex = widgets::color_hex(button);
        controller.update_layer(&layer_id, Some(key.as_str()), |layer| {
            apply(layer, &hex);
        });
    });
}

fn pick_asset_files(
    overlay: &adw::ToastOverlay,
    title: &str,
    extensions: &[&str],
    controller: &Rc<Controller>,
    layer_id: &str,
) {
    let Some(dir) = controller.project_dir() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title(title.to_string())
        .modal(true)
        .build();
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(title));
    for extension in extensions {
        filter.add_suffix(extension);
    }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    dialog.set_filters(Some(&filters));
    dialog.set_default_filter(Some(&filter));

    let parent = overlay
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    let controller = controller.clone();
    let overlay = overlay.clone();
    let layer_id = layer_id.to_string();
    dialog.open_multiple(parent.as_ref(), gio::Cancellable::NONE, move |result| {
        let Ok(files) = result else {
            return;
        };
        let paths: Vec<PathBuf> = (0..files.n_items())
            .filter_map(|index| files.item(index))
            .filter_map(|item| item.downcast::<gio::File>().ok())
            .filter_map(|file| file.path())
            .collect();
        if paths.is_empty() {
            return;
        }
        let (imported, skipped) = assets::import_files(&dir, &paths);
        if let Some(first) = imported.first() {
            let first = first.clone();
            controller.update_layer(&layer_id, Some("inspector-asset-import"), |layer| {
                layer.kind.set_asset(Some(first.clone()));
            });
        }
        if !imported.is_empty() {
            widgets::toast(
                &overlay,
                &t!("assets.imported", count = imported.len()).to_string(),
            );
            controller.mark_assets_changed();
        }
        if !skipped.is_empty() {
            widgets::toast(
                &overlay,
                &t!("assets.skipped", count = skipped.len()).to_string(),
            );
        }
    });
}

fn pick_x11_window(
    parent: Option<&gtk::Window>,
    controller: &Rc<Controller>,
    overlay: &adw::ToastOverlay,
    layer_id: &str,
) {
    let windows = match x11::list_windows(std::process::id()) {
        Ok(windows) => windows,
        Err(err) => {
            widgets::toast(
                overlay,
                &t!("capture.x11_error", message = err.to_string()).to_string(),
            );
            return;
        }
    };
    if windows.is_empty() {
        widgets::toast(overlay, &t!("capture.x11_empty").to_string());
        return;
    }
    let windows = Rc::new(windows);

    let window = gtk::Window::builder()
        .title(t!("capture.x11_title").to_string())
        .modal(true)
        .default_width(480)
        .default_height(420)
        .build();
    if let Some(parent) = parent {
        window.set_transient_for(Some(parent));
    }
    let header = gtk::HeaderBar::new();
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("boxed-list");
    for info in windows.iter() {
        let row = gtk::ListBoxRow::new();
        let line = gtk::Box::new(gtk::Orientation::Vertical, 2);
        line.set_margin_top(6);
        line.set_margin_bottom(6);
        line.set_margin_start(8);
        line.set_margin_end(8);
        let title = if info.title.trim().is_empty() {
            info.wm_class.clone()
        } else {
            info.title.clone()
        };
        let heading = gtk::Label::new(Some(&title));
        heading.set_xalign(0.0);
        heading.set_ellipsize(gtk::pango::EllipsizeMode::End);
        line.append(&heading);
        let detail = format!("{} · {}×{}", info.wm_class, info.width, info.height);
        line.append(&widgets::dim_label(&detail));
        row.set_child(Some(&line));
        list.append(&row);
    }
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .min_content_height(280)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.append(&scrolled);
    let close = widgets::button(&t!("common.cancel").to_string());
    {
        let window = window.downgrade();
        close.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
    body.append(&close);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));
    window.set_child(Some(&toolbar));

    {
        let window = window.downgrade();
        let controller = controller.clone();
        let layer_id = layer_id.to_string();
        let windows = windows.clone();
        list.connect_row_activated(move |_, row| {
            let index = row.index() as usize;
            let Some(info) = windows.get(index) else {
                return;
            };
            let title = info.title.clone();
            let wm_class = info.wm_class.clone();
            let xid = info.xid;
            controller.update_layer(&layer_id, Some("inspector-capture-x11"), |layer| {
                layer.kind.set_capture_source(CaptureSource::X11Window {
                    title,
                    wm_class,
                    xid,
                });
            });
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }

    window.present();
}
