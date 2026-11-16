//! Main operator window: header, pages, show bar, soundboard dock and shortcuts.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk::gio;
use gtk::glib;
use gtk::glib::variant::ToVariant;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use rust_i18n::t;

use crate::config::Config;
use crate::model::layer::PatternKind;
use crate::services::controller::{Controller, DeckCall, Mode, PrompterCall, SoundboardCall};
use crate::services::remote_bridge::RemoteBridge;
use crate::ui::{
    Page, design::DesignPage, dialogs, prompter_page::PrompterPage, queue_page::QueuePage,
    run::RunPage, settings::SettingsWindow, soundboard_page::SoundboardPage, widgets,
};

pub struct MainWindow {
    inner: Rc<Inner>,
}

struct Inner {
    window: adw::ApplicationWindow,
    controller: Rc<Controller>,
    overlay: adw::ToastOverlay,
    stack: adw::ViewStack,
    title: adw::WindowTitle,
    project_title: RefCell<String>,
    pages: RefCell<Vec<Rc<dyn Page>>>,
    content: gtk::Stack,
    welcome: gtk::Widget,
    show_bar: gtk::Box,
    program_label: gtk::Label,
    onair: gtk::Label,
    black_button: gtk::ToggleButton,
    freeze_button: gtk::ToggleButton,
    mute_button: gtk::ToggleButton,
    dock: gtk::Revealer,
    dock_button: gtk::ToggleButton,
    live_switch: gtk::Switch,
    live_box: gtk::Box,
    menu_button: gtk::MenuButton,
    recent_box: gtk::Box,
    weak_self: RefCell<Option<std::rc::Weak<Inner>>>,
    new_button: gtk::Button,
    open_button: gtk::Button,
    bridge: Rc<RemoteBridge>,
    soundboard: Rc<SoundboardPage>,
    prompter: Rc<PrompterPage>,
    prompter_output: RefCell<Option<(glib::WeakRef<gtk::Window>, gtk::Widget)>>,
}

impl MainWindow {
    pub fn new(app: &adw::Application, project: Option<PathBuf>, config: Config) -> Self {
        let controller = Controller::new(config, Mode::Gui);
        controller.attach_app(app);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(t!("app.window_title").to_string())
            .default_width(1440)
            .default_height(900)
            .build();

        let title = adw::WindowTitle::new(&t!("app.name").to_string(), "");
        let stack = adw::ViewStack::new();
        let overlay = adw::ToastOverlay::new();

        let show_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let program_label = gtk::Label::new(None);
        program_label.add_css_class("show-name");
        let onair = gtk::Label::new(Some(&t!("show.on_air").to_string()));
        onair.add_css_class("signifiage-onair");
        onair.set_visible(false);
        let black_button = gtk::ToggleButton::with_label(&t!("show.black").to_string());
        let freeze_button = gtk::ToggleButton::with_label(&t!("show.freeze").to_string());
        let mute_button = gtk::ToggleButton::with_label(&t!("show.mute").to_string());
        let dock = gtk::Revealer::new();
        dock.set_transition_type(gtk::RevealerTransitionType::SlideUp);
        let dock_button = gtk::ToggleButton::new();
        dock_button.set_icon_name("audio-headphones-symbolic");
        dock_button.set_tooltip_text(Some(&t!("soundboard.dock_tooltip").to_string()));
        let live_switch = widgets::switch(false);

        let recent_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let (welcome, new_button, open_button) = build_welcome(&recent_box);
        let menu_button = gtk::MenuButton::new();

        let bridge = RemoteBridge::new(controller.clone());
        let inner = Rc::new(Inner {
            window: window.clone(),
            controller: controller.clone(),
            overlay: overlay.clone(),
            stack: stack.clone(),
            title: title.clone(),
            project_title: RefCell::new(String::new()),
            pages: RefCell::new(Vec::new()),
            content: gtk::Stack::new(),
            welcome: welcome.clone(),
            show_bar: show_bar.clone(),
            program_label: program_label.clone(),
            onair: onair.clone(),
            black_button: black_button.clone(),
            freeze_button: freeze_button.clone(),
            mute_button: mute_button.clone(),
            dock: dock.clone(),
            dock_button: dock_button.clone(),
            live_switch: live_switch.clone(),
            live_box: gtk::Box::new(gtk::Orientation::Horizontal, 6),
            menu_button: menu_button.clone(),
            recent_box: recent_box.clone(),
            weak_self: RefCell::new(None),
            new_button: new_button.clone(),
            open_button: open_button.clone(),
            bridge: bridge.clone(),
            soundboard: SoundboardPage::new(controller.clone(), overlay.clone()),
            prompter: PrompterPage::new(controller.clone(), overlay.clone()),
            prompter_output: RefCell::new(None),
        });

        *inner.weak_self.borrow_mut() = Some(Rc::downgrade(&inner));

        // Pages.
        let design = DesignPage::new(controller.clone(), overlay.clone());
        let run = RunPage::new(controller.clone(), overlay.clone());
        let queue = QueuePage::new(controller.clone(), overlay.clone());
        let prompter = inner.prompter.clone();
        let soundboard = inner.soundboard.clone();

        let page_list: Vec<Rc<dyn Page>> = vec![
            design.clone() as Rc<dyn Page>,
            run.clone() as Rc<dyn Page>,
            queue.clone() as Rc<dyn Page>,
            prompter.clone() as Rc<dyn Page>,
            soundboard.clone() as Rc<dyn Page>,
        ];
        let names = ["design", "run", "queue", "prompter", "soundboard"];
        let icons = [
            "document-edit-symbolic",
            "media-playback-start-symbolic",
            "view-list-symbolic",
            "view-continuous-symbolic",
            "audio-volume-high-symbolic",
        ];
        for ((page, name), icon) in page_list.iter().zip(names.iter()).zip(icons.iter()) {
            stack.add_titled_with_icon(
                &page.widget(),
                Some(name),
                &t!(format!("page.{name}")).to_string(),
                icon,
            );
        }
        *inner.pages.borrow_mut() = page_list;

        inner.build_layout(design.clone(), soundboard.clone());

        // Controller callbacks.
        {
            let weak = Rc::downgrade(&inner);
            controller.set_toast_handler(Rc::new(move |message: &str| {
                if let Some(inner) = weak.upgrade() {
                    widgets::toast(&inner.overlay, message);
                }
            }));
        }
        {
            let weak = Rc::downgrade(&inner);
            controller.set_error_handler(Rc::new(move |title: &str, body: &str| {
                if let Some(inner) = weak.upgrade() {
                    widgets::alert(Some(inner.window.upcast_ref()), title, body);
                }
            }));
        }
        {
            let weak = Rc::downgrade(&inner);
            controller.set_changed_handler(Rc::new(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.refresh();
                }
            }));
        }

        // Web remote + page hooks.
        {
            let bridge_weak = Rc::downgrade(&inner.bridge);
            controller.set_state_publisher(Some(Rc::new(move || {
                if let Some(bridge) = bridge_weak.upgrade() {
                    bridge.publish();
                }
            })));
            inner.bridge.apply_config(&controller.config());
        }
        {
            let runtime = prompter.runtime();
            let live = controller.live();
            let controller_weak = Rc::downgrade(&controller);
            controller.set_prompter_hook(Rc::new(move |call: PrompterCall| {
                match call {
                    PrompterCall::Start => runtime.start(),
                    PrompterCall::Pause => runtime.pause(),
                    PrompterCall::Reset => runtime.reset(),
                    PrompterCall::Speed(value) => runtime.set_speed(value),
                }
                live.prompter_running.set(runtime.is_running());
                live.prompter_speed.set(runtime.speed());
                if let Some(controller) = controller_weak.upgrade() {
                    controller.publish_state();
                }
            }));
        }
        {
            let page = soundboard.clone();
            let live = controller.live();
            let controller_weak = Rc::downgrade(&controller);
            controller.set_soundboard_hook(Rc::new(move |call: SoundboardCall| {
                match call {
                    SoundboardCall::Play(pad_id) => {
                        page.play_pad(&pad_id);
                    }
                    SoundboardCall::Stop(pad_id) => {
                        page.stop_pad(&pad_id);
                    }
                    SoundboardCall::StopAll => page.stop_all(),
                    SoundboardCall::Page(index) => {
                        page.set_page(index);
                    }
                }
                if let Some(controller) = controller_weak.upgrade() {
                    *live.playing_pads.borrow_mut() = page.playing();
                    live.soundboard_page
                        .set(controller.project_clone().soundboard.active_page);
                    controller.publish_state();
                }
            }));
        }
        {
            let page = run.clone();
            controller.set_deck_hook(Rc::new(move |call: DeckCall| page.deck_call(call)));
        }

        // Tick.
        {
            let controller = controller.clone();
            let bridge = inner.bridge.clone();
            glib::timeout_add_local(Duration::from_millis(100), move || {
                controller.tick();
                bridge.tick();
                glib::ControlFlow::Continue
            });
        }

        // Open the requested/last project.
        let start = project
            .clone()
            .or_else(|| controller.config().last_project.clone());
        if let Some(path) = start
            && let Err(err) = controller.open_project(&path)
        {
            widgets::toast(
                &overlay,
                &format!(
                    "{}",
                    t!("project.error.open_failed", error = err.to_string())
                ),
            );
        }
        inner.refresh();

        let window_for_shortcuts = window.clone();
        inner.install_shortcuts(&window_for_shortcuts);
        inner.install_close_handler();

        window.present();
        Self { inner }
    }

    pub fn present(&self) {
        self.inner.window.present();
    }

    pub fn controller(&self) -> Rc<Controller> {
        self.inner.controller.clone()
    }

    /// Switch the visible page (`design`, `run`, …); no-op for unknown names.
    pub fn show_page(&self, name: &str) {
        if self.inner.stack.child_by_name(name).is_some() {
            self.inner.stack.set_visible_child_name(name);
        }
    }

    /// Capture the window contents (used by the GUI screenshot example).
    pub fn capture(&self, width: i32, height: i32) -> Option<gtk::gdk::Texture> {
        let widget: gtk::Widget = self.inner.window.clone().upcast();
        let native = widget.native()?;
        let renderer = native.renderer()?;
        let (alloc_w, alloc_h) = (widget.width(), widget.height());
        if alloc_w <= 0 || alloc_h <= 0 {
            return None;
        }
        let paintable = gtk::WidgetPaintable::new(Some(&widget));
        let snapshot = gtk::Snapshot::new();
        snapshot.scale(
            width as f32 / alloc_w as f32,
            height as f32 / alloc_h as f32,
        );
        paintable.snapshot(&snapshot, f64::from(alloc_w), f64::from(alloc_h));
        let node = snapshot.to_node()?;
        Some(renderer.render_texture(
            &node,
            Some(&gtk::graphene::Rect::new(
                0.0,
                0.0,
                width as f32,
                height as f32,
            )),
        ))
    }

    pub fn window(&self) -> &adw::ApplicationWindow {
        &self.inner.window
    }

    /// Toast overlay, shared with dialogs that report results.
    pub fn overlay(&self) -> adw::ToastOverlay {
        self.inner.overlay.clone()
    }
}

impl Inner {
    fn build_layout(self: &Rc<Self>, design: Rc<DesignPage>, soundboard: Rc<SoundboardPage>) {
        // Header.
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&self.title));

        let switcher = adw::ViewSwitcher::builder()
            .stack(&self.stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build();
        header.set_title_widget(Some(&switcher));

        self.stack.connect_visible_child_name_notify({
            let weak = Rc::downgrade(self);
            move |stack| {
                if let Some(inner) = weak.upgrade() {
                    let visible = stack.visible_child_name().map(|name| name.to_string());
                    inner
                        .live_box
                        .set_visible(visible.as_deref() == Some("design"));
                    inner.live_switch.set_active(inner.controller.live_edit());
                }
            }
        });

        self.menu_button.set_icon_name("open-menu-symbolic");
        self.bridge.apply_config(&self.controller.config());
        self.sync_prompter_output();
        {
            let runtime = self.prompter.runtime();
            let live = self.controller.live();
            live.prompter_running.set(runtime.is_running());
            live.prompter_speed.set(runtime.speed());
        }
        self.menu_button.set_menu_model(Some(&self.build_menu()));
        header.pack_end(&self.menu_button);
        let live_label = gtk::Label::new(Some(&t!("design.live").to_string()));
        live_label.set_valign(gtk::Align::Center);
        self.live_box.append(&live_label);
        self.live_box.append(&self.live_switch);
        self.live_box.set_visible(false);
        header.pack_end(&self.live_box);
        self.live_switch
            .set_tooltip_text(Some(&t!("design.live_tooltip").to_string()));
        {
            let controller = self.controller.clone();
            self.live_switch.connect_state_set(move |_, state| {
                controller.set_live_edit(state);
                glib::Propagation::Proceed
            });
        }

        // Show bar.
        let go = widgets::suggested(&t!("show.go").to_string());
        let prev = widgets::button(&t!("show.prev").to_string());
        {
            let controller = self.controller.clone();
            go.connect_clicked(move |_| {
                controller.go();
            });
        }
        {
            let controller = self.controller.clone();
            prev.connect_clicked(move |_| {
                controller.prev();
            });
        }
        {
            let controller = self.controller.clone();
            self.black_button.connect_toggled(move |button| {
                controller.set_black(button.is_active());
            });
        }
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            self.freeze_button.connect_toggled(move |button| {
                controller.set_frozen(button.is_active());
                button.set_active(controller.is_frozen());
                let _ = &overlay;
            });
        }
        {
            let controller = self.controller.clone();
            self.mute_button
                .connect_toggled(move |button| controller.set_muted(button.is_active()));
        }
        {
            let controller = self.controller.clone();
            self.dock_button.connect_toggled(move |button| {
                let _ = controller;
                button.is_active();
            });
        }

        let patterns = gtk::MenuButton::new();
        patterns.set_label(&t!("show.patterns").to_string());
        let popover = gtk::Popover::new();
        let pattern_list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        for kind in PatternKind::ALL {
            let item = widgets::button(&t!(kind.label_key()).to_string());
            let controller = self.controller.clone();
            item.connect_clicked(move |_| controller.show_pattern(kind));
            pattern_list.append(&item);
        }
        let resume = widgets::button(&t!("pattern.resume").to_string());
        {
            let controller = self.controller.clone();
            resume.connect_clicked(move |_| controller.resume_program());
        }
        pattern_list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        pattern_list.append(&resume);
        popover.set_child(Some(&pattern_list));
        patterns.set_popover(Some(&popover));

        self.show_bar.set_margin_top(6);
        self.show_bar.set_margin_bottom(6);
        self.show_bar.set_margin_start(12);
        self.show_bar.set_margin_end(12);
        self.show_bar.append(&go);
        self.show_bar.append(&prev);
        self.show_bar.append(&self.black_button);
        self.show_bar.append(&self.freeze_button);
        self.show_bar.append(&self.mute_button);
        self.show_bar.append(&self.onair);
        self.show_bar.append(&self.program_label);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        self.show_bar.append(&spacer);
        self.show_bar.append(&patterns);
        self.show_bar.append(&self.dock_button);

        // Dock.
        self.dock.set_child(Some(&soundboard.dock()));
        {
            let dock = self.dock.clone();
            self.dock_button.connect_toggled(move |button| {
                dock.set_reveal_child(button.is_active());
            });
        }

        // Workspace: stack + show bar + dock + revealer.
        let workspace = gtk::Box::new(gtk::Orientation::Vertical, 0);
        self.stack.set_vexpand(true);
        workspace.append(&self.stack);
        workspace.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        workspace.append(&self.show_bar);
        workspace.append(&self.dock);

        self.content.add_named(&self.welcome, Some("welcome"));
        self.content.add_named(&workspace, Some("workspace"));
        self.content.set_visible_child_name("welcome");

        let clamp = adw::Clamp::builder()
            .maximum_size(1800)
            .tightening_threshold(1400)
            .child(&self.content)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&clamp));

        {
            let weak = Rc::downgrade(self);
            self.new_button.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.new_project();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.open_button.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.open_project_dialog();
                }
            });
        }

        self.overlay.set_child(Some(&toolbar));
        self.window.set_content(Some(&self.overlay));

        let _ = design;
    }

    fn build_menu(&self) -> gio::Menu {
        let menu = gio::Menu::new();
        let project = gio::Menu::new();
        project.append(Some(&t!("menu.new").to_string()), Some("win.new"));
        project.append(Some(&t!("menu.open").to_string()), Some("win.open"));
        project.append(Some(&t!("menu.save").to_string()), Some("win.save"));
        project.append(Some(&t!("menu.publish").to_string()), Some("win.publish"));
        menu.append_section(None, &project);

        let settings = gio::Menu::new();
        settings.append(Some(&t!("menu.settings").to_string()), Some("win.settings"));
        settings.append(Some(&t!("menu.about").to_string()), Some("win.about"));
        menu.append_section(None, &settings);

        let recent_config = self.controller.config();
        if !recent_config.recent_projects.is_empty() {
            let recent = gio::Menu::new();
            for path in recent_config.recent_projects.iter().take(8) {
                let label = path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                let item = gio::MenuItem::new(Some(&label), Some("win.recent"));
                item.set_attribute_value("target", Some(&path.display().to_string().to_variant()));
                recent.append_item(&item);
            }
            menu.append_section(Some(&t!("menu.recent").to_string()), &recent);
        }

        let quit = gio::Menu::new();
        quit.append(Some(&t!("menu.quit").to_string()), Some("win.quit"));
        menu.append_section(None, &quit);
        menu
    }

    fn install_shortcuts(self: &Rc<Self>, window: &adw::ApplicationWindow) {
        let actions: [(&str, fn(&Inner)); 7] = [
            ("new", |inner| inner.new_project()),
            ("open", |inner| inner.open_project_dialog()),
            ("save", |inner| {
                if let Err(err) = inner.controller.save_now() {
                    widgets::toast(&inner.overlay, &err.to_string());
                }
            }),
            ("publish", |inner| {
                dialogs::publish_dialog(&inner.window, &inner.controller, &inner.overlay)
            }),
            ("settings", |inner| {
                SettingsWindow::present(inner.controller.clone(), &inner.window)
            }),
            ("about", |inner| dialogs::about(&inner.window)),
            ("quit", |inner| inner.window.close()),
        ];
        for (name, handler) in actions {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(inner) = weak.upgrade() {
                    handler(&inner);
                }
            });
            window.add_action(&action);
        }

        let recent = gio::SimpleAction::new("recent", Some(glib::VariantTy::STRING));
        {
            let weak = Rc::downgrade(self);
            recent.connect_activate(move |_, parameter| {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let Some(path) = parameter.and_then(|value| value.get::<String>()) else {
                    return;
                };
                if let Err(err) = inner.controller.open_project(std::path::Path::new(&path)) {
                    widgets::toast(&inner.overlay, &err.to_string());
                }
                inner.refresh();
            });
        }
        window.add_action(&recent);

        let controller = self.controller.clone();
        let weak = Rc::downgrade(self);
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        key.connect_key_pressed(move |_, key, _code, modifiers| {
            let Some(inner) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if is_editing(inner.window.upcast_ref()) {
                return glib::Propagation::Proceed;
            }
            let ctrl = modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let shift = modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            match (key, ctrl, shift) {
                (gtk::gdk::Key::space, false, _) => {
                    controller.go();
                    glib::Propagation::Stop
                }
                (gtk::gdk::Key::BackSpace, false, _) => {
                    controller.prev();
                    glib::Propagation::Stop
                }
                (gtk::gdk::Key::b, false, _) => {
                    controller.set_black(!controller.is_black());
                    glib::Propagation::Stop
                }
                (gtk::gdk::Key::f, false, _) => {
                    controller.set_frozen(!controller.is_frozen());
                    glib::Propagation::Stop
                }
                (gtk::gdk::Key::m, false, _) => {
                    controller.set_muted(!controller.is_muted());
                    glib::Propagation::Stop
                }
                (gtk::gdk::Key::s, false, _) => {
                    inner.dock_button.set_active(!inner.dock_button.is_active());
                    glib::Propagation::Stop
                }
                (gtk::gdk::Key::s, true, _) => {
                    if let Err(err) = controller.save_now() {
                        widgets::toast(&inner.overlay, &err.to_string());
                    }
                    glib::Propagation::Stop
                }
                (gtk::gdk::Key::z, true, false) => {
                    controller.undo();
                    glib::Propagation::Stop
                }
                (gtk::gdk::Key::z, true, true) => {
                    controller.redo();
                    glib::Propagation::Stop
                }
                (gtk::gdk::Key::n, true, _) => {
                    inner.new_project();
                    glib::Propagation::Stop
                }
                (gtk::gdk::Key::o, true, _) => {
                    inner.open_project_dialog();
                    glib::Propagation::Stop
                }
                (gtk::gdk::Key::p, true, _) => {
                    dialogs::publish_dialog(&inner.window, &controller, &inner.overlay);
                    glib::Propagation::Stop
                }
                (gtk::gdk::Key::d, true, _) => {
                    if let Some(layer) = controller.selected_layer() {
                        controller.duplicate_layer(&layer);
                    }
                    glib::Propagation::Stop
                }
                (gtk::gdk::Key::Delete, false, _) => {
                    if let Some(layer) = controller.selected_layer() {
                        controller.delete_layer(&layer);
                    }
                    glib::Propagation::Stop
                }
                (key, false, _) if hotkey_name(key).is_some() => {
                    let name = hotkey_name(key).unwrap_or_default();
                    inner.soundboard.trigger_hotkey(&name);
                    inner.sync_soundboard_state();
                    glib::Propagation::Stop
                }
                (gtk::gdk::Key::Escape, false, _) => {
                    inner.dock_button.set_active(false);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        window.add_controller(key);
    }

    fn install_close_handler(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            let Some(inner) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if inner.controller.is_open()
                && let Err(err) = inner.controller.save_now()
            {
                widgets::alert(
                    Some(inner.window.upcast_ref()),
                    &t!("project.save_status.failed").to_string(),
                    &err.to_string(),
                );
                return glib::Propagation::Stop;
            }
            if let Some(outputs) = inner.controller.outputs() {
                outputs.close_all();
            }
            glib::Propagation::Proceed
        });
    }

    fn self_weak(&self) -> Option<Rc<Inner>> {
        self.weak_self
            .borrow()
            .as_ref()
            .and_then(|weak| weak.upgrade())
    }

    fn new_project(&self) {
        dialogs::new_project(&self.window, &self.controller);
    }

    fn open_project_dialog(&self) {
        let dialog = gtk::FileDialog::builder()
            .title(t!("menu.open").to_string())
            .modal(true)
            .build();
        let weak = self.self_weak().map(|inner| Rc::downgrade(&inner));
        dialog.select_folder(Some(&self.window), gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else {
                return;
            };
            let Some(path) = file.path() else {
                return;
            };
            if let Some(inner) = weak.as_ref().and_then(|weak| weak.upgrade()) {
                if let Err(err) = inner.controller.open_project(&path) {
                    widgets::toast(&inner.overlay, &err.to_string());
                }
                inner.refresh();
            }
        });
    }

    /// Repaint chrome, pages and the welcome/recent list.
    fn refresh(&self) {
        let open = self.controller.is_open();
        self.content
            .set_visible_child_name(if open { "workspace" } else { "welcome" });

        if open {
            let name = self.controller.project_name();
            *self.project_title.borrow_mut() = name.clone();
            let status = if self.controller.is_dirty() {
                t!("app.save_status.unsaved").to_string()
            } else {
                t!("app.save_status.saved").to_string()
            };
            self.title.set_title(&name);
            self.title.set_subtitle(&status);
        } else {
            self.title.set_title(&t!("app.name").to_string());
            self.title
                .set_subtitle(&t!("welcome.no_project").to_string());
        }

        let program = self.controller.program_name();
        self.program_label.set_text(&program);
        self.onair
            .set_visible(!self.controller.program().is_black());
        self.black_button.set_active(self.controller.is_black());
        self.freeze_button.set_active(self.controller.is_frozen());
        self.mute_button.set_active(self.controller.is_muted());
        self.live_switch.set_active(self.controller.live_edit());
        let on_design = self
            .stack
            .visible_child_name()
            .map(|name| name.as_str() == "design")
            .unwrap_or(false);
        self.live_box.set_visible(on_design && open);

        self.menu_button.set_menu_model(Some(&self.build_menu()));
        for page in self.pages.borrow().iter() {
            page.refresh();
        }
        self.refresh_recent();
    }

    fn refresh_recent(&self) {
        while let Some(child) = self.recent_box.first_child() {
            self.recent_box.remove(&child);
        }
        let config = self.controller.config();
        for path in config.recent_projects.iter().take(8) {
            let button = gtk::Button::with_label(&path.display().to_string());
            button.add_css_class("flat");
            button.set_halign(gtk::Align::Start);
            let controller = self.controller.clone();
            let path = path.clone();
            let weak = self.self_weak();
            let weak = weak.map_or_else(
                || std::rc::Weak::<Inner>::new(),
                |inner| Rc::downgrade(&inner),
            );
            button.connect_clicked(move |_| {
                if let Err(err) = controller.open_project(&path)
                    && let Some(inner) = weak.upgrade()
                {
                    widgets::toast(&inner.overlay, &err.to_string());
                }
                if let Some(inner) = weak.upgrade() {
                    inner.refresh();
                }
            });
            self.recent_box.append(&button);
        }
        if config.recent_projects.is_empty() {
            self.recent_box
                .append(&widgets::dim_label(&t!("welcome.no_recent").to_string()));
        }
    }

    /// Put a `PrompterView` into the prompter output window instead of a scene view.
    fn sync_prompter_output(&self) {
        let Some(outputs) = self.controller.outputs() else {
            return;
        };
        let Some(window) = outputs.window_for(crate::ui::outputs::OutputRole::Prompter) else {
            self.prompter_output.borrow_mut().take();
            return;
        };
        let prompter = self.controller.project_clone().prompter;
        let runtime = self.prompter.runtime();
        let mut slot = self.prompter_output.borrow_mut();
        let same_window = slot
            .as_ref()
            .and_then(|(weak, _)| weak.upgrade())
            .is_some_and(|existing| existing.eq(&window));
        if !same_window {
            let view = crate::ui::prompter_view::PrompterView::with_runtime(runtime);
            view.set_settings(&prompter);
            window.set_child(Some(&view));
            *slot = Some((window.downgrade(), view.upcast()));
        } else if let Some((_, widget)) = slot.as_ref()
            && let Some(view) = widget.downcast_ref::<crate::ui::prompter_view::PrompterView>()
        {
            view.set_settings(&prompter);
        }
    }

    fn sync_soundboard_state(&self) {
        let live = self.controller.live();
        *live.playing_pads.borrow_mut() = self.soundboard.playing();
        live.soundboard_page
            .set(self.controller.project_clone().soundboard.active_page);
        self.controller.publish_state();
    }
}

fn build_welcome(recent: &gtk::Box) -> (gtk::Widget, gtk::Button, gtk::Button) {
    let page = adw::StatusPage::builder()
        .icon_name("io.github.ta4anx.Signifiage")
        .title(t!("welcome.title").to_string())
        .description(t!("welcome.no_project").to_string())
        .build();
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::Center);
    actions.set_margin_top(12);

    let new_button = widgets::suggested(&t!("welcome.new_project").to_string());
    let open_button = widgets::button(&t!("welcome.open_project").to_string());
    actions.append(&new_button);
    actions.append(&open_button);

    recent.set_halign(gtk::Align::Center);
    let holder = gtk::Box::new(gtk::Orientation::Vertical, 8);
    holder.append(&actions);
    holder.append(recent);
    page.set_child(Some(&holder));
    (page.upcast(), new_button, open_button)
}

/// F1–F12 as stored hotkey names.
fn hotkey_name(key: gtk::gdk::Key) -> Option<String> {
    let name = key.name()?;
    let number = name.strip_prefix('F')?;
    if number.is_empty() || number.len() > 2 {
        return None;
    }
    let index: u32 = number.parse().ok()?;
    (1..=12).contains(&index).then(|| format!("F{index}"))
}

fn is_editing(window: &gtk::Window) -> bool {
    let Some(focus) = gtk::prelude::GtkWindowExt::focus(window) as Option<gtk::Widget> else {
        return false;
    };
    focus.is::<gtk::Text>() || focus.is::<gtk::TextView>() || focus.is::<gtk::Entry>()
}

#[allow(dead_code)]
fn current_recent(controller: &Rc<Controller>) -> Vec<PathBuf> {
    controller.config().recent_projects
}
