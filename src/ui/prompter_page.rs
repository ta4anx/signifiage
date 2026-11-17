//! Prompter page: script editor, transport, display controls and the live
//! preview driven by the shared [`PrompterRuntime`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use rust_i18n::t;

use crate::model::Prompter;
use crate::services::controller::Controller;
use crate::ui::outputs::OutputRole;
use crate::ui::prompter_view::{PrompterRuntime, PrompterView};
use crate::ui::{Page, widgets};

pub struct PrompterPage {
    pub root: gtk::Box,
    controller: Rc<Controller>,
    overlay: adw::ToastOverlay,
    runtime: Rc<PrompterRuntime>,
    preview: PrompterView,
    editor: gtk::TextView,
    transport: gtk::Button,
    speed: gtk::Scale,
    speed_value: gtk::Label,
    font_size: gtk::SpinButton,
    family: gtk::FontDialogButton,
    line_spacing: gtk::SpinButton,
    width_pct: gtk::Scale,
    width_value: gtk::Label,
    mirror_h: gtk::Switch,
    mirror_v: gtk::Switch,
    output_label: gtk::Label,
    output_button: gtk::Button,
    preview_aspect: gtk::AspectFrame,
    updating: RefCell<bool>,
    debounce: RefCell<Option<glib::SourceId>>,
    last_text: RefCell<String>,
    transport_running: Cell<bool>,
}

/// Aspect ratio of the prompter output: the assigned monitor, else 16:9.
fn output_aspect(controller: &Controller) -> f64 {
    let config = controller.config();
    let monitors = crate::ui::outputs::monitor_list();
    if let Some(index) =
        crate::ui::outputs::resolve_monitor(config.outputs.prompter.monitor.as_ref(), &monitors)
    {
        let geometry = monitors[index].geometry();
        if geometry.width() > 0 && geometry.height() > 0 {
            return f64::from(geometry.width()) / f64::from(geometry.height());
        }
    }
    16.0 / 9.0
}

impl PrompterPage {
    pub fn new(controller: Rc<Controller>, overlay: adw::ToastOverlay) -> Rc<Self> {
        let initial = controller.with_project(|project| project.prompter.clone());
        let runtime = PrompterRuntime::new(initial.speed);
        let preview = PrompterView::with_runtime(runtime.clone());
        let mut preview_settings = initial.clone();
        preview_settings.mirror_h = false;
        preview_settings.mirror_v = false;
        preview.set_settings(&preview_settings);

        let editor = gtk::TextView::new();
        editor.set_wrap_mode(gtk::WrapMode::Word);
        editor.set_left_margin(12);
        editor.set_right_margin(12);
        editor.set_top_margin(12);
        editor.set_bottom_margin(12);
        editor.buffer().set_text(&initial.text);

        let transport = gtk::Button::with_label(&t!("prompter.start").to_string());
        transport.add_css_class("suggested-action");
        let reset = widgets::button(&t!("prompter.reset").to_string());
        {
            let runtime = runtime.clone();
            let preview = preview.clone();
            reset.connect_clicked(move |_| {
                runtime.pause();
                runtime.reset();
                preview.set_offset(0.0);
            });
        }

        let speed = gtk::Scale::with_range(gtk::Orientation::Horizontal, 10.0, 200.0, 1.0);
        speed.set_hexpand(true);
        speed.set_draw_value(false);
        speed.set_value(initial.speed);
        let speed_value = gtk::Label::new(Some(&format_speed(initial.speed)));
        speed_value.add_css_class("dim-label");
        speed_value.set_width_chars(6);
        speed_value.set_xalign(1.0);

        let font_size = widgets::spin(8.0, 512.0, 1.0, initial.font_size);
        font_size.set_digits(0);

        let family = widgets::font_button(&initial.font_family, initial.font_size);

        let line_spacing = widgets::spin(1.0, 4.0, 0.05, initial.line_spacing);
        line_spacing.set_digits(2);

        let width_pct = gtk::Scale::with_range(gtk::Orientation::Horizontal, 10.0, 100.0, 1.0);
        width_pct.set_hexpand(true);
        width_pct.set_draw_value(false);
        width_pct.set_value(initial.width_pct);
        let width_value = gtk::Label::new(Some(&format_percent(initial.width_pct)));
        width_value.add_css_class("dim-label");
        width_value.set_width_chars(6);
        width_value.set_xalign(1.0);

        let mirror_h = widgets::switch(initial.mirror_h);
        let mirror_v = widgets::switch(initial.mirror_v);

        let output_label = gtk::Label::new(None);
        output_label.add_css_class("dim-label");
        output_label.set_xalign(0.0);
        output_label.set_wrap(true);
        let output_button = widgets::button(&t!("prompter.open_output").to_string());

        // ---- Layout: editor/controls on the left, live preview on the right.
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        root.set_margin_top(12);
        root.set_margin_bottom(12);
        root.set_margin_start(12);
        root.set_margin_end(12);
        root.set_vexpand(true);
        root.set_hexpand(true);

        let left = gtk::Box::new(gtk::Orientation::Vertical, 10);
        left.set_size_request(400, -1);
        left.set_valign(gtk::Align::Fill);

        let editor_title = gtk::Label::new(Some(&t!("prompter.script").to_string()));
        editor_title.add_css_class("title-4");
        editor_title.set_xalign(0.0);
        left.append(&editor_title);

        let editor_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(240)
            .vexpand(true)
            .build();
        let editor_frame = gtk::Frame::new(None);
        editor_frame.add_css_class("card");
        editor_frame.set_child(Some(&editor));
        editor_scroll.set_child(Some(&editor_frame));
        left.append(&editor_scroll);

        let transport_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        transport_row.append(&transport);
        transport_row.append(&reset);
        left.append(&transport_row);

        let speed_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let speed_caption = gtk::Label::new(Some(&t!("prompter.speed").to_string()));
        speed_caption.set_xalign(0.0);
        speed_caption.set_width_chars(12);
        speed_row.append(&speed_caption);
        speed_row.append(&speed);
        speed_row.append(&speed_value);
        left.append(&speed_row);

        left.append(&widgets::labelled(
            &t!("prompter.font_size").to_string(),
            &font_size,
        ));
        left.append(&widgets::labelled(
            &t!("prompter.family").to_string(),
            &family,
        ));
        left.append(&widgets::labelled(
            &t!("prompter.line_spacing").to_string(),
            &line_spacing,
        ));

        let width_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let width_caption = gtk::Label::new(Some(&t!("prompter.width_pct").to_string()));
        width_caption.set_xalign(0.0);
        width_caption.set_width_chars(12);
        width_row.append(&width_caption);
        width_row.append(&width_pct);
        width_row.append(&width_value);
        left.append(&width_row);

        left.append(&widgets::labelled(
            &t!("prompter.mirror_h").to_string(),
            &mirror_h,
        ));
        left.append(&widgets::labelled(
            &t!("prompter.mirror_v").to_string(),
            &mirror_v,
        ));

        left.append(&widgets::separator());
        let output_row = gtk::Box::new(gtk::Orientation::Vertical, 6);
        output_row.append(&output_label);
        output_row.append(&output_button);
        left.append(&output_row);

        let right = gtk::Box::new(gtk::Orientation::Vertical, 6);
        right.set_hexpand(true);
        right.set_vexpand(true);
        let preview_title = gtk::Label::new(Some(&t!("prompter.preview").to_string()));
        preview_title.add_css_class("title-4");
        preview_title.set_xalign(0.0);
        right.append(&preview_title);
        // The preview must show the same shape as the prompter output, otherwise
        // the wrapped text looks different from what the audience sees.
        let preview_aspect = gtk::AspectFrame::builder()
            .ratio(output_aspect(&controller) as f32)
            .obey_child(false)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .vexpand(true)
            .child(&preview)
            .build();
        preview_aspect.add_css_class("scene-canvas");
        right.append(&preview_aspect);

        root.append(&left);
        root.append(&right);

        let page = Rc::new(Self {
            root,
            controller: controller.clone(),
            overlay,
            runtime,
            preview,
            editor,
            transport,
            speed,
            speed_value,
            font_size,
            family,
            line_spacing,
            width_pct,
            width_value,
            mirror_h,
            mirror_v,
            output_label,
            output_button,
            preview_aspect: preview_aspect.clone(),
            updating: RefCell::new(false),
            debounce: RefCell::new(None),
            last_text: RefCell::new(initial.text),
            transport_running: Cell::new(false),
        });

        page.wire_signals();
        page.refresh_state();
        page.watch_transport();
        page
    }

    fn wire_signals(self: &Rc<Self>) {
        {
            let weak = Rc::downgrade(self);
            self.root.connect_map(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.refresh_state();
                }
            });
        }

        // Text edits: debounce the write, then settle on focus-out.
        {
            let weak = Rc::downgrade(self);
            self.editor.buffer().connect_changed(move |_| {
                let Some(page) = weak.upgrade() else {
                    return;
                };
                if *page.updating.borrow() {
                    return;
                }
                page.schedule_commit();
            });
        }
        {
            let focus = gtk::EventControllerFocus::new();
            let weak = Rc::downgrade(self);
            focus.connect_leave(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.commit_text();
                }
            });
            self.editor.add_controller(focus);
        }

        // Transport.
        {
            let preview = self.preview.clone();
            let runtime = self.runtime.clone();
            self.transport.connect_clicked(move |_| {
                if runtime.is_running() {
                    runtime.pause();
                    preview.set_running(false);
                } else {
                    if preview.at_end() {
                        runtime.set_offset(0.0);
                    }
                    runtime.start();
                    preview.set_running(true);
                }
            });
        }

        // Speed.
        {
            let page = self.clone();
            self.speed.connect_value_changed(move |scale| {
                if *page.updating.borrow() {
                    return;
                }
                let value = scale.value();
                page.speed_value.set_text(&format_speed(value));
                page.write_prompter("prompter-speed", |prompter| prompter.speed = value);
            });
        }

        // Font size.
        {
            let page = self.clone();
            self.font_size.connect_value_changed(move |spin| {
                if *page.updating.borrow() {
                    return;
                }
                let value = spin.value();
                page.write_prompter("prompter-font-size", |prompter| {
                    prompter.font_size = value;
                });
            });
        }

        // Family picker.
        {
            let page = self.clone();
            self.family.connect_font_desc_notify(move |button| {
                if *page.updating.borrow() {
                    return;
                }
                let family = widgets::font_family(button);
                page.write_prompter("prompter-family", |prompter| {
                    prompter.font_family = family.clone();
                });
            });
        }

        // Line spacing.
        {
            let page = self.clone();
            self.line_spacing.connect_value_changed(move |spin| {
                if *page.updating.borrow() {
                    return;
                }
                let value = spin.value();
                page.write_prompter("prompter-line-spacing", |prompter| {
                    prompter.line_spacing = value;
                });
            });
        }

        // Text column width.
        {
            let page = self.clone();
            self.width_pct.connect_value_changed(move |scale| {
                if *page.updating.borrow() {
                    return;
                }
                let value = scale.value();
                page.width_value.set_text(&format_percent(value));
                page.write_prompter("prompter-width", |prompter| {
                    prompter.width_pct = value;
                });
            });
        }

        // Mirrors.
        {
            let page = self.clone();
            self.mirror_h.connect_active_notify(move |switch| {
                if *page.updating.borrow() {
                    return;
                }
                let active = switch.is_active();
                page.write_prompter("prompter-mirror-h", |prompter| {
                    prompter.mirror_h = active;
                });
            });
        }
        {
            let page = self.clone();
            self.mirror_v.connect_active_notify(move |switch| {
                if *page.updating.borrow() {
                    return;
                }
                let active = switch.is_active();
                page.write_prompter("prompter-mirror-v", |prompter| {
                    prompter.mirror_v = active;
                });
            });
        }

        // Output window.
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            self.output_button.connect_clicked(move |_| {
                let assigned = controller.config().outputs.prompter.monitor.is_some();
                let Some(outputs) = controller.outputs() else {
                    widgets::toast(&overlay, &t!("prompter.no_outputs").to_string());
                    return;
                };
                outputs.open_windowed(OutputRole::Prompter);
                if assigned {
                    controller.apply_output_config();
                }
                // Until the output manager hosts a `PrompterView`, this window
                // shows a plain scene view; `prompter_view::make_output_widget`
                // is the ready-made widget for that wiring.
            });
        }
    }

    fn current_prompter(&self) -> Prompter {
        self.controller
            .with_project(|project| project.prompter.clone())
    }

    /// Shared scroll runtime; the window can hand it to output/web wiring.
    pub fn runtime(&self) -> Rc<PrompterRuntime> {
        self.runtime.clone()
    }

    /// The page preview is always unmirrored; mirroring only affects the output.
    fn apply_preview_settings(&self) {
        let mut prompter = self.current_prompter();
        prompter.mirror_h = false;
        prompter.mirror_v = false;
        self.preview.set_settings(&prompter);
    }

    /// Write a prompter change: model, dirty flag and preview.
    fn write_prompter(&self, key: &'static str, mut mutate: impl FnMut(&mut Prompter)) {
        let mut next = self.current_prompter();
        mutate(&mut next);
        next.normalize();
        if next == self.current_prompter() {
            return;
        }
        self.controller.mutate(Some(key), |project| {
            mutate(&mut project.prompter);
            project.prompter.normalize();
        });
        // Prompter fields are not part of the design undo snapshot, so `mutate`
        // leaves the dirty flag alone; `mutate_quiet` records the change without
        // an undo entry, which is exactly the prompter contract.
        if !self.controller.is_dirty() {
            self.controller.mutate_quiet(|_| {});
        }
        self.apply_preview_settings();
    }

    fn schedule_commit(self: &Rc<Self>) {
        self.cancel_debounce();
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(Duration::from_millis(400), move || {
            if let Some(page) = weak.upgrade() {
                *page.debounce.borrow_mut() = None;
                page.commit_text();
            }
            glib::ControlFlow::Break
        });
        *self.debounce.borrow_mut() = Some(id);
    }

    fn cancel_debounce(&self) {
        if let Some(id) = self.debounce.borrow_mut().take() {
            id.remove();
        }
    }

    fn commit_text(&self) {
        self.cancel_debounce();
        let buffer = self.editor.buffer();
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string();
        *self.last_text.borrow_mut() = text.clone();
        if text == self.current_prompter().text {
            return;
        }
        self.write_prompter("prompter-text", |prompter| {
            prompter.text = text.clone();
        });
    }

    fn watch_transport(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(250), move || match weak.upgrade() {
            Some(page) => {
                page.update_transport();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
    }

    fn update_transport(&self) {
        let running = self.runtime.is_running();
        if running == self.transport_running.get() {
            return;
        }
        self.transport_running.set(running);
        self.transport.set_label(&if running {
            t!("prompter.pause").to_string()
        } else {
            t!("prompter.start").to_string()
        });
    }

    fn refresh_state(&self) {
        if *self.updating.borrow() {
            return;
        }
        *self.updating.borrow_mut() = true;

        let prompter = self.current_prompter();
        self.apply_preview_settings();

        let busy = self.debounce.borrow().is_some();
        let buffer = self.editor.buffer();
        let buffer_text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string();
        let focused = self.editor.has_focus();
        if !busy && !focused && buffer_text != prompter.text {
            buffer.set_text(&prompter.text);
            *self.last_text.borrow_mut() = prompter.text.clone();
        }

        if (self.speed.value() - prompter.speed).abs() > f64::EPSILON {
            self.speed.set_value(prompter.speed);
        }
        self.speed_value.set_text(&format_speed(prompter.speed));

        if (self.font_size.value() - prompter.font_size).abs() > f64::EPSILON {
            self.font_size.set_value(prompter.font_size);
        }
        let current_family = widgets::font_family(&self.family);
        let current_size = widgets::font_size(&self.family);
        if current_family != prompter.font_family || (current_size - prompter.font_size).abs() > 0.5
        {
            widgets::set_font(&self.family, &prompter.font_family, prompter.font_size);
        }

        if (self.line_spacing.value() - prompter.line_spacing).abs() > 1e-3 {
            self.line_spacing.set_value(prompter.line_spacing);
        }
        if (self.width_pct.value() - prompter.width_pct).abs() > f64::EPSILON {
            self.width_pct.set_value(prompter.width_pct);
        }
        self.width_value
            .set_text(&format_percent(prompter.width_pct));

        if self.mirror_h.is_active() != prompter.mirror_h {
            self.mirror_h.set_active(prompter.mirror_h);
        }
        if self.mirror_v.is_active() != prompter.mirror_v {
            self.mirror_v.set_active(prompter.mirror_v);
        }

        let monitor = self.controller.config().outputs.prompter.monitor.clone();
        let detail = match monitor {
            Some(monitor) => {
                let description = if !monitor.model.trim().is_empty() {
                    monitor.model.clone()
                } else if let Some(connector) = &monitor.connector {
                    connector.clone()
                } else {
                    format!("{}", monitor.index + 1)
                };
                t!("prompter.assigned_to", monitor = description).to_string()
            }
            None => t!("prompter.unassigned").to_string(),
        };
        self.output_label
            .set_text(&format!("{}: {}", t!("prompter.assignment"), detail));
        self.output_button
            .set_label(&t!("prompter.open_output").to_string());

        self.update_transport();

        *self.updating.borrow_mut() = false;
    }
}

fn format_speed(value: f64) -> String {
    format!("{:.0} px/s", value.round())
}

fn format_percent(value: f64) -> String {
    format!("{:.0} %", value.round())
}

impl Page for PrompterPage {
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    fn refresh(&self) {
        self.preview_aspect
            .set_ratio(output_aspect(&self.controller) as f32);
        self.refresh_state();
    }
}
