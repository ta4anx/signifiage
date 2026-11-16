//! Soundboard page: page tabs, a 12-pad grid, the pad editor and the compact
//! dock used by the main window's revealer.
//!
//! The page owns the [`SoundboardEngine`]; the main window only calls
//! [`SoundboardPage::trigger_hotkey`], [`SoundboardPage::stop_all`] and mounts
//! [`SoundboardPage::dock`].

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Duration;

use gtk::gio;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use rust_i18n::t;

use crate::model::Project;
use crate::model::soundboard as board;
use crate::services::assets::{self, AssetKind};
use crate::services::controller::{Controller, SoundboardCall};
use crate::services::soundboard::SoundboardEngine;
use crate::ui::{Page, widgets};

/// Pads per row in the main grid.
const COLUMNS: usize = 4;

#[derive(Clone, PartialEq)]
struct PadSig {
    id: String,
    label: String,
    color: String,
    hotkey: Option<String>,
    armed: bool,
}

#[derive(Clone, PartialEq)]
struct PageSig {
    name: String,
    pads: Vec<PadSig>,
}

#[derive(Clone, PartialEq)]
struct BoardSig {
    active: usize,
    pages: Vec<PageSig>,
}

fn board_signature(soundboard: &board::Soundboard) -> BoardSig {
    BoardSig {
        active: soundboard.active_page,
        pages: soundboard
            .pages
            .iter()
            .map(|page| PageSig {
                name: page.name.clone(),
                pads: page
                    .pads
                    .iter()
                    .map(|pad| PadSig {
                        id: pad.id.clone(),
                        label: pad.label.clone(),
                        color: pad.color.clone(),
                        hotkey: pad.hotkey.clone(),
                        armed: pad.is_armed(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

pub struct SoundboardPage {
    pub root: gtk::Box,
    controller: Rc<Controller>,
    engine: RefCell<SoundboardEngine>,
    tabs: gtk::Box,
    grid: gtk::Grid,
    board_stack: gtk::Stack,
    dock_box: gtk::Box,
    add_button: gtk::Button,
    remove_button: gtk::Button,
    stop_button: gtk::Button,
    mute_chip: gtk::Label,
    css: gtk::CssProvider,
    signature: RefCell<Option<BoardSig>>,
    buttons: RefCell<HashMap<String, gtk::Button>>,
    dock_buttons: RefCell<HashMap<String, gtk::Button>>,
    playing: RefCell<HashSet<String>>,
    last_sink: RefCell<Option<String>>,
    last_mute: Cell<bool>,
    last_project_dir: RefCell<Option<PathBuf>>,
    editor: RefCell<Option<(String, adw::Window)>>,
    weak_self: RefCell<Weak<Self>>,
}

impl SoundboardPage {
    pub fn new(controller: Rc<Controller>, _overlay: adw::ToastOverlay) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_top(16);
        root.set_margin_bottom(16);
        root.set_margin_start(16);
        root.set_margin_end(16);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tabs.add_css_class("linked");
        header.append(&tabs);

        let add_button =
            widgets::icon_button("list-add-symbolic", &t!("soundboard.add_page").to_string());
        let remove_button = widgets::icon_button(
            "list-remove-symbolic",
            &t!("soundboard.remove_page").to_string(),
        );
        header.append(&add_button);
        header.append(&remove_button);

        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        header.append(&spacer);

        let mute_chip = gtk::Label::new(Some(&t!("soundboard.muted").to_string()));
        mute_chip.add_css_class("signifiage-badge");
        mute_chip.add_css_class("error");
        mute_chip.set_tooltip_text(Some(&t!("soundboard.master_mute").to_string()));
        mute_chip.set_visible(false);
        header.append(&mute_chip);

        let stop_button = widgets::button(&t!("soundboard.stop_all").to_string());
        stop_button.add_css_class("destructive-action");
        stop_button.set_tooltip_text(Some(&t!("soundboard.stop_all_tooltip").to_string()));
        header.append(&stop_button);

        let grid = gtk::Grid::new();
        grid.set_column_spacing(8);
        grid.set_row_spacing(8);
        grid.set_column_homogeneous(true);
        grid.set_row_homogeneous(true);
        grid.set_hexpand(true);
        grid.set_vexpand(true);
        grid.set_margin_top(4);
        grid.set_margin_bottom(4);
        grid.set_margin_start(4);
        grid.set_margin_end(4);

        let placeholder = adw::StatusPage::builder()
            .icon_name("audio-speakers-symbolic")
            .title(t!("soundboard.no_project").to_string())
            .description(t!("welcome.no_project").to_string())
            .build();

        let board_stack = gtk::Stack::new();
        board_stack.set_vexpand(true);
        board_stack.add_named(&grid, Some("board"));
        board_stack.add_named(&placeholder, Some("empty"));
        board_stack.set_visible_child_name("empty");

        root.append(&header);
        root.append(&board_stack);

        let dock_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        dock_box.set_margin_top(6);
        dock_box.set_margin_bottom(6);
        dock_box.set_margin_start(12);
        dock_box.set_margin_end(12);

        let css = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        let page = Rc::new(Self {
            root,
            controller: controller.clone(),
            engine: RefCell::new(SoundboardEngine::new()),
            tabs,
            grid,
            board_stack,
            dock_box,
            add_button: add_button.clone(),
            remove_button: remove_button.clone(),
            stop_button: stop_button.clone(),
            mute_chip,
            css,
            signature: RefCell::new(None),
            buttons: RefCell::new(HashMap::new()),
            dock_buttons: RefCell::new(HashMap::new()),
            playing: RefCell::new(HashSet::new()),
            last_sink: RefCell::new(controller.config().audio_sink),
            last_mute: Cell::new(controller.is_muted()),
            last_project_dir: RefCell::new(controller.project_dir()),
            editor: RefCell::new(None),
            weak_self: RefCell::new(Weak::new()),
        });
        *page.weak_self.borrow_mut() = Rc::downgrade(&page);

        // Web remote actions are routed through the controller hook; the page
        // owns the engine, so it is the only handler that can play pads.
        {
            let weak = Rc::downgrade(&page);
            controller.set_soundboard_hook(Rc::new(move |call: SoundboardCall| {
                if let Some(page) = weak.upgrade() {
                    page.on_soundboard_call(call);
                }
            }));
        }

        {
            let weak = Rc::downgrade(&page);
            add_button.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.add_page();
                }
            });
        }
        {
            let weak = Rc::downgrade(&page);
            remove_button.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.remove_active_page();
                }
            });
        }
        {
            let weak = Rc::downgrade(&page);
            stop_button.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.stop_all();
                }
            });
        }

        {
            let weak = Rc::downgrade(&page);
            glib::timeout_add_local(Duration::from_millis(100), move || {
                let Some(page) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                page.tick();
                glib::ControlFlow::Continue
            });
        }

        // Refresh through the trait so a page created before a project is open
        // shows the placeholder until the window reopens one.
        Page::refresh(page.as_ref());
        page
    }

    // -------------------------------------------------------------- hotkeys

    /// Play the active page's pad bound to `key` (`F1`–`F12`). An unarmed pad
    /// opens its editor instead. The main window calls this only while no text
    /// widget owns the keyboard focus.
    pub fn trigger_hotkey(&self, key: &str) {
        if !self.controller.is_open() {
            return;
        }
        let pad = self.controller.with_project(|project| {
            let soundboard = &project.soundboard;
            let page = soundboard.active()?;
            let index = page.pad_for_hotkey(key)?;
            page.pads.get(index).cloned()
        });
        let Some(pad) = pad else {
            return;
        };
        if pad.is_armed() {
            self.start_pad(&pad);
        } else if let Some(page) = self.weak_self.borrow().upgrade() {
            page.open_editor(&pad.id);
        }
    }

    /// Play a pad by id (dock and web remote). Unarmed pads are no-ops here so a
    /// remote action never opens a dialog on the operator screen.
    pub fn play_pad(&self, pad_id: &str) {
        if !self.controller.is_open() {
            return;
        }
        let Some(pad) = self
            .controller
            .with_project(|project| project.soundboard.pad(pad_id).cloned())
        else {
            return;
        };
        if pad.is_armed() {
            self.start_pad(&pad);
        }
    }

    /// Stop one pad by id.
    pub fn stop_pad(&self, pad_id: &str) {
        self.engine.borrow_mut().stop(pad_id);
        self.apply_playing();
    }

    /// Ids of the pads currently playing, sorted (used by the web snapshot).
    pub fn playing(&self) -> Vec<String> {
        self.engine.borrow().playing()
    }

    /// Select a soundboard page; returns false when the index does not exist.
    pub fn set_page(&self, index: usize) -> bool {
        if !self.controller.is_open() {
            return false;
        }
        let exists = self
            .controller
            .with_project(|project| index < project.soundboard.pages.len());
        if !exists {
            return false;
        }
        let current = self
            .controller
            .with_project(|project| project.soundboard.active_page);
        if current != index {
            self.commit(move |project| project.soundboard.active_page = index);
        }
        true
    }

    /// Route a web-remote soundboard command to the engine.
    fn on_soundboard_call(&self, call: SoundboardCall) {
        match call {
            SoundboardCall::Play(pad_id) => self.play_pad(&pad_id),
            SoundboardCall::Stop(pad_id) => self.stop_pad(&pad_id),
            SoundboardCall::StopAll => self.stop_all(),
            SoundboardCall::Page(index) => {
                self.set_page(index);
            }
        }
    }

    /// Stop every playing pad (Stop All button, dock and the window shortcut).
    pub fn stop_all(&self) {
        self.engine.borrow_mut().stop_all();
        self.apply_playing();
    }

    /// Compact single-row pad strip mounted in the window's bottom revealer.
    pub fn dock(&self) -> gtk::Widget {
        self.dock_box.clone().upcast()
    }

    // --------------------------------------------------------------- state

    /// Soundboard edits are not design-owned, so `mutate` would neither mark the
    /// project dirty nor notify the window. `mutate_quiet` marks it dirty and
    /// `mark_assets_changed` refreshes views and the window chrome.
    fn commit<R>(&self, f: impl FnOnce(&mut Project) -> R) -> R {
        let result = self.controller.mutate_quiet(f);
        let active = self
            .controller
            .with_project(|project| project.soundboard.active_page);
        self.controller.live().soundboard_page.set(active);
        self.controller.mark_assets_changed();
        result
    }

    fn refresh(&self) {
        let project_dir = self.controller.project_dir();
        if self.last_project_dir.borrow().as_ref() != project_dir.as_ref() {
            self.last_project_dir.replace(project_dir);
            self.engine.borrow_mut().stop_all();
            self.playing.borrow_mut().clear();
            self.controller.live().playing_pads.replace(Vec::new());
            if let Some((_, window)) = self.editor.borrow().as_ref()
                && window.is_visible()
            {
                window.close();
            }
        }

        let open = self.controller.is_open();
        self.board_stack
            .set_visible_child_name(if open { "board" } else { "empty" });
        self.add_button.set_sensitive(open);
        self.stop_button.set_sensitive(open);
        if !open {
            self.remove_button.set_sensitive(false);
            self.clear_board();
            self.mute_chip.set_visible(false);
            self.controller.live().soundboard_page.set(0);
            return;
        }

        let soundboard = self
            .controller
            .with_project(|project| project.soundboard.clone());
        self.controller
            .live()
            .soundboard_page
            .set(soundboard.active_page);
        let signature = board_signature(&soundboard);
        let changed = self.signature.borrow().as_ref() != Some(&signature);
        if changed {
            *self.signature.borrow_mut() = Some(signature);
            self.rebuild_css(&soundboard);
            self.rebuild_tabs(&soundboard);
            self.rebuild_grid(&soundboard);
            self.rebuild_dock(&soundboard);
            self.playing.borrow_mut().clear();
        }
        self.remove_button.set_sensitive(soundboard.pages.len() > 1);
        self.mute_chip.set_visible(self.controller.is_muted());
        self.apply_playing();
    }

    fn clear_board(&self) {
        *self.signature.borrow_mut() = None;
        self.buttons.borrow_mut().clear();
        self.dock_buttons.borrow_mut().clear();
        self.playing.borrow_mut().clear();
        for container in [&self.tabs, &self.dock_box] {
            while let Some(child) = container.first_child() {
                container.remove(&child);
            }
        }
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
        self.css.load_from_string("");
    }

    fn tick(&self) {
        let error = {
            let mut engine = self.engine.borrow_mut();
            let _stopped = engine.poll();
            engine.take_error()
        };
        if let Some(error) = error {
            self.controller
                .toast(&t!("soundboard.play_failed", message = error).to_string());
        }

        let sink = self.controller.config().audio_sink;
        if *self.last_sink.borrow() != sink {
            *self.last_sink.borrow_mut() = sink;
            self.engine.borrow_mut().stop_all();
        }

        let muted = self.controller.is_muted();
        if self.last_mute.get() != muted {
            self.last_mute.set(muted);
            self.engine.borrow_mut().set_master_mute(muted);
        }
        if self.mute_chip.is_visible() != muted {
            self.mute_chip.set_visible(muted);
        }

        // Removing an asset deletes the file; a pad bound to it must stop.
        let playing = self.engine.borrow().playing();
        if !playing.is_empty() {
            let base = self.controller.project_dir();
            let mut missing = Vec::new();
            for pad_id in playing {
                let asset = self.controller.with_project(|project| {
                    project
                        .soundboard
                        .pad(&pad_id)
                        .and_then(|pad| pad.asset.clone())
                });
                let available = asset
                    .as_deref()
                    .and_then(|asset| crate::services::media::resolve_asset(base.as_ref(), asset))
                    .is_some_and(|path| path.is_file());
                if !available {
                    missing.push(pad_id);
                }
            }
            if !missing.is_empty() {
                let mut engine = self.engine.borrow_mut();
                for pad_id in &missing {
                    engine.stop(pad_id);
                }
            }
        }

        self.apply_playing();
    }

    fn apply_playing(&self) {
        let playing_ids = self.engine.borrow().playing();
        let playing: HashSet<String> = playing_ids.iter().cloned().collect();
        if *self.playing.borrow() == playing {
            return;
        }
        *self.playing.borrow_mut() = playing.clone();
        self.controller.live().playing_pads.replace(playing_ids);
        for (pad_id, button) in self.buttons.borrow().iter() {
            set_playing_class(button, playing.contains(pad_id));
        }
        for (pad_id, button) in self.dock_buttons.borrow().iter() {
            set_playing_class(button, playing.contains(pad_id));
        }
    }

    // ---------------------------------------------------------------- pages

    fn add_page(&self) {
        if !self.controller.is_open() {
            return;
        }
        self.commit(|project| project.soundboard.add_page());
    }

    fn remove_active_page(&self) {
        if !self.controller.is_open() {
            return;
        }
        let Some((index, pad_ids)) = self.controller.with_project(|project| {
            let soundboard = &project.soundboard;
            if soundboard.pages.len() <= 1 {
                return None;
            }
            let page = soundboard.pages.get(soundboard.active_page)?;
            Some((
                soundboard.active_page,
                page.pads
                    .iter()
                    .map(|pad| pad.id.clone())
                    .collect::<Vec<_>>(),
            ))
        }) else {
            self.controller
                .toast(&t!("soundboard.last_page").to_string());
            return;
        };
        for pad_id in &pad_ids {
            self.engine.borrow_mut().stop(pad_id);
        }
        self.commit(move |project| project.soundboard.remove_page(index));
        self.apply_playing();
    }

    fn on_page_toggled(&self, index: usize, button: &gtk::ToggleButton) {
        if !self.controller.is_open() {
            return;
        }
        let current = self
            .controller
            .with_project(|project| project.soundboard.active_page);
        if button.is_active() {
            if current != index {
                self.commit(move |project| project.soundboard.active_page = index);
            }
        } else if current == index {
            // A grouped toggle must not leave the page without an active tab.
            button.set_active(true);
        }
    }

    fn rebuild_tabs(&self, soundboard: &board::Soundboard) {
        while let Some(child) = self.tabs.first_child() {
            self.tabs.remove(&child);
        }
        let mut group: Option<gtk::ToggleButton> = None;
        for (index, page) in soundboard.pages.iter().enumerate() {
            let button = gtk::ToggleButton::with_label(&page.name);
            if let Some(first) = &group {
                button.set_group(Some(first));
            } else {
                group = Some(button.clone());
            }
            button.set_active(index == soundboard.active_page);
            let weak = self.weak_self.borrow().clone();
            button.connect_toggled(move |button| {
                if let Some(page) = weak.upgrade() {
                    page.on_page_toggled(index, button);
                }
            });
            self.tabs.append(&button);
        }
    }

    // ------------------------------------------------------------- pad grid

    fn rebuild_grid(&self, soundboard: &board::Soundboard) {
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
        self.buttons.borrow_mut().clear();
        let Some(page) = soundboard.pages.get(soundboard.active_page) else {
            return;
        };
        for (index, pad) in page.pads.iter().take(board::PADS_PER_PAGE).enumerate() {
            let (widget, button) = self.build_pad(pad, soundboard.active_page, index);
            self.grid.attach(
                &widget,
                (index % COLUMNS) as i32,
                (index / COLUMNS) as i32,
                1,
                1,
            );
            self.buttons.borrow_mut().insert(pad.id.clone(), button);
        }
    }

    fn build_pad(
        &self,
        pad: &board::Pad,
        page_index: usize,
        pad_index: usize,
    ) -> (gtk::Widget, gtk::Button) {
        let button = gtk::Button::new();
        button.add_css_class("soundboard-pad");
        button.add_css_class(&format!("sb-bg-{page_index}-{pad_index}"));
        button.set_hexpand(true);
        button.set_vexpand(true);
        button.set_tooltip_text(Some(&t!("soundboard.pad_tooltip").to_string()));

        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.set_valign(gtk::Align::Center);
        let hotkey = gtk::Label::new(pad.hotkey.as_deref());
        hotkey.add_css_class("caption");
        hotkey.set_xalign(0.0);
        hotkey.set_halign(gtk::Align::Start);
        content.append(&hotkey);

        let label_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        label_row.set_halign(gtk::Align::Center);
        label_row.set_valign(gtk::Align::Center);
        if !pad.is_armed() {
            let muted = gtk::Image::from_icon_name("audio-volume-muted-symbolic");
            muted.set_pixel_size(16);
            muted.set_tooltip_text(Some(&t!("soundboard.pad_unarmed").to_string()));
            label_row.append(&muted);
        }
        let label = gtk::Label::new(Some(&pad.label));
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        label.set_justify(gtk::Justification::Center);
        label.add_css_class("heading");
        label_row.append(&label);
        content.append(&label_row);
        button.set_child(Some(&content));

        {
            let weak = self.weak_self.borrow().clone();
            let pad_id = pad.id.clone();
            button.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.on_pad_clicked(&pad_id);
                }
            });
        }
        {
            let weak = self.weak_self.borrow().clone();
            let pad_id = pad.id.clone();
            let gesture = gtk::GestureClick::new();
            gesture.set_button(3);
            gesture.connect_pressed(move |_, _, _, _| {
                if let Some(page) = weak.upgrade() {
                    page.open_editor(&pad_id);
                }
            });
            button.add_controller(gesture);
        }

        let overlay = gtk::Overlay::new();
        overlay.set_hexpand(true);
        overlay.set_vexpand(true);
        overlay.set_child(Some(&button));

        let edit = gtk::Button::new();
        edit.set_icon_name("document-edit-symbolic");
        edit.add_css_class("flat");
        edit.add_css_class("circular");
        edit.set_halign(gtk::Align::End);
        edit.set_valign(gtk::Align::Start);
        edit.set_margin_top(2);
        edit.set_margin_end(2);
        edit.set_tooltip_text(Some(&t!("soundboard.edit_pad").to_string()));
        {
            let weak = self.weak_self.borrow().clone();
            let pad_id = pad.id.clone();
            edit.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.open_editor(&pad_id);
                }
            });
        }
        overlay.add_overlay(&edit);
        (overlay.upcast(), button)
    }

    fn on_pad_clicked(&self, pad_id: &str) {
        if !self.controller.is_open() {
            return;
        }
        let Some(pad) = self
            .controller
            .with_project(|project| project.soundboard.pad(pad_id).cloned())
        else {
            return;
        };
        if pad.is_armed() {
            self.start_pad(&pad);
        } else if let Some(page) = self.weak_self.borrow().upgrade() {
            page.open_editor(pad_id);
        }
    }

    fn start_pad(&self, pad: &board::Pad) {
        let base = self.controller.project_dir();
        let sink = self.controller.config().audio_sink;
        let muted = self.controller.is_muted();
        let error = {
            let mut engine = self.engine.borrow_mut();
            engine.play(pad, base.as_ref(), sink.as_deref(), muted);
            engine.take_error()
        };
        if let Some(error) = error {
            self.controller
                .toast(&t!("soundboard.play_failed", message = error).to_string());
        }
        self.apply_playing();
    }

    // ----------------------------------------------------------------- dock

    fn rebuild_dock(&self, soundboard: &board::Soundboard) {
        while let Some(child) = self.dock_box.first_child() {
            self.dock_box.remove(&child);
        }
        self.dock_buttons.borrow_mut().clear();
        if let Some(page) = soundboard.pages.get(soundboard.active_page) {
            for (index, pad) in page.pads.iter().take(board::PADS_PER_PAGE).enumerate() {
                let button = gtk::Button::new();
                button.add_css_class("sb-dock-pad");
                button.add_css_class(&format!("sb-bg-{}-{index}", soundboard.active_page));
                button.set_tooltip_text(Some(&t!("soundboard.pad_tooltip").to_string()));

                let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                row.set_halign(gtk::Align::Center);
                if !pad.is_armed() {
                    let muted = gtk::Image::from_icon_name("audio-volume-muted-symbolic");
                    muted.set_pixel_size(12);
                    row.append(&muted);
                }
                let label = gtk::Label::new(Some(&pad.label));
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                label.set_max_width_chars(14);
                row.append(&label);
                button.set_child(Some(&row));

                {
                    let weak = self.weak_self.borrow().clone();
                    let pad_id = pad.id.clone();
                    button.connect_clicked(move |_| {
                        if let Some(page) = weak.upgrade() {
                            page.on_pad_clicked(&pad_id);
                        }
                    });
                }
                {
                    let weak = self.weak_self.borrow().clone();
                    let pad_id = pad.id.clone();
                    let gesture = gtk::GestureClick::new();
                    gesture.set_button(3);
                    gesture.connect_pressed(move |_, _, _, _| {
                        if let Some(page) = weak.upgrade() {
                            page.open_editor(&pad_id);
                        }
                    });
                    button.add_controller(gesture);
                }
                self.dock_box.append(&button);
                self.dock_buttons
                    .borrow_mut()
                    .insert(pad.id.clone(), button);
            }
        }
        let stop = widgets::button(&t!("soundboard.stop_all").to_string());
        stop.add_css_class("destructive-action");
        {
            let weak = self.weak_self.borrow().clone();
            stop.connect_clicked(move |_| {
                if let Some(page) = weak.upgrade() {
                    page.stop_all();
                }
            });
        }
        self.dock_box.append(&stop);
    }

    // ------------------------------------------------------------------ css

    fn rebuild_css(&self, soundboard: &board::Soundboard) {
        let mut css = String::from(
            ".soundboard-pad.playing { animation: sb-pad-glow 1s ease-in-out infinite; }\n\
             @keyframes sb-pad-glow {\n\
               0% { box-shadow: inset 0 0 0 3px rgba(255, 255, 255, 0.95); }\n\
               50% { box-shadow: inset 0 0 0 3px rgba(255, 255, 255, 0.15); }\n\
               100% { box-shadow: inset 0 0 0 3px rgba(255, 255, 255, 0.95); }\n\
             }\n\
             .sb-dock-pad { min-width: 64px; min-height: 34px; padding: 2px 8px; }\n\
             .sb-dock-pad.playing { box-shadow: inset 0 0 0 3px #22c55e; }\n",
        );
        for (page_index, page) in soundboard.pages.iter().enumerate() {
            for (pad_index, pad) in page.pads.iter().enumerate() {
                let rgba = crate::ui::scene_view::gdk_rgba_str(&pad.color);
                let red = (rgba.red() * 255.0).round() as u8;
                let green = (rgba.green() * 255.0).round() as u8;
                let blue = (rgba.blue() * 255.0).round() as u8;
                let alpha = rgba.alpha();
                let luma =
                    0.2126 * f64::from(red) + 0.7152 * f64::from(green) + 0.0722 * f64::from(blue);
                let foreground = if luma > 150.0 { "#000000" } else { "#FFFFFF" };
                css.push_str(&format!(
                    "button.sb-bg-{page_index}-{pad_index} {{ background-color: rgba({red}, {green}, {blue}, {alpha:.3}); background-image: none; color: {foreground}; }}\n"
                ));
            }
        }
        self.css.load_from_string(&css);
    }

    // ----------------------------------------------------------- pad editor

    fn open_editor(&self, pad_id: &str) {
        let Some(pad) = self
            .controller
            .with_project(|project| project.soundboard.pad(pad_id).cloned())
        else {
            return;
        };
        {
            let editor = self.editor.borrow();
            if let Some((open_pad, window)) = editor.as_ref()
                && window.is_visible()
            {
                if open_pad == pad_id {
                    window.present();
                    return;
                }
                window.close();
            }
        }
        let Some(project_dir) = self.controller.project_dir() else {
            return;
        };
        let parent = self
            .root
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok());

        let mut builder = adw::Window::builder()
            .title(t!("soundboard.edit_pad").to_string())
            .modal(true)
            .default_width(440);
        if let Some(parent) = &parent {
            builder = builder.transient_for(parent);
        }
        let window = builder.build();
        *self.editor.borrow_mut() = Some((pad.id.clone(), window.clone()));

        let header = adw::HeaderBar::new();
        let cancel = gtk::Button::with_label(&t!("soundboard.cancel").to_string());
        header.pack_start(&cancel);
        let save = gtk::Button::with_label(&t!("soundboard.save").to_string());
        save.add_css_class("suggested-action");
        header.pack_end(&save);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(12);
        content.set_margin_bottom(16);
        content.set_margin_start(16);
        content.set_margin_end(16);

        let label_entry = widgets::entry(&pad.label);
        content.append(&widgets::field(
            &t!("soundboard.pad_label").to_string(),
            &label_entry,
        ));

        let color_button = widgets::color_button(&pad.color);
        content.append(&widgets::labelled(
            &t!("soundboard.pad_color").to_string(),
            &color_button,
        ));

        let mut labels: Vec<String> = vec![t!("soundboard.no_asset").to_string()];
        let mut rels: Vec<String> = vec![String::new()];
        for asset in assets::list(&project_dir)
            .into_iter()
            .filter(|asset| asset.kind == AssetKind::Audio)
        {
            labels.push(asset.name);
            rels.push(asset.rel);
        }
        let mut selected = 0usize;
        if let Some(current) = pad.asset.as_deref() {
            if let Some(index) = rels.iter().position(|rel| rel == current) {
                selected = index;
            } else {
                let name = current.rsplit('/').next().unwrap_or(current).to_string();
                labels.push(format!("{} ({})", name, t!("soundboard.asset_missing")));
                rels.push(current.to_string());
                selected = labels.len() - 1;
            }
        }
        let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let string_list = gtk::StringList::new(&label_refs);
        let dropdown = gtk::DropDown::new(Some(string_list.clone()), gtk::Expression::NONE);
        dropdown.set_selected(selected as u32);
        dropdown.set_hexpand(true);
        let import = gtk::Button::with_label(&t!("soundboard.import").to_string());
        let asset_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        asset_row.append(&dropdown);
        asset_row.append(&import);
        content.append(&widgets::field(
            &t!("soundboard.pad_asset").to_string(),
            &asset_row,
        ));
        let rels = Rc::new(RefCell::new(rels));

        {
            let controller = self.controller.clone();
            let project_dir = project_dir.clone();
            let string_list = string_list.clone();
            let rels = rels.clone();
            let dropdown = dropdown.clone();
            let window = window.clone();
            import.connect_clicked(move |_| {
                let dialog = gtk::FileDialog::builder()
                    .title(t!("soundboard.import_title").to_string())
                    .modal(true)
                    .build();
                let filter = gtk::FileFilter::new();
                filter.set_name(Some(&t!("soundboard.audio_filter").to_string()));
                for extension in assets::AUDIO_EXTENSIONS {
                    filter.add_suffix(extension);
                }
                let filters = gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));
                dialog.set_default_filter(Some(&filter));

                let controller = controller.clone();
                let project_dir = project_dir.clone();
                let string_list = string_list.clone();
                let rels = rels.clone();
                let dropdown = dropdown.clone();
                dialog.open(Some(&window), gio::Cancellable::NONE, move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        return;
                    };
                    match assets::import_file(&project_dir, &path) {
                        Ok(rel) => {
                            let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
                            rels.borrow_mut().push(rel);
                            string_list.append(&name);
                            dropdown.set_selected(string_list.n_items().saturating_sub(1));
                            controller.mark_assets_changed();
                            controller.toast(&t!("soundboard.imported").to_string());
                        }
                        Err(err) => controller.error(
                            &t!("soundboard.import_failed_title").to_string(),
                            &err.to_string(),
                        ),
                    }
                });
            });
        }

        let volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        volume.set_digits(2);
        volume.set_value(pad.volume);
        volume.set_hexpand(true);
        volume.set_draw_value(true);
        content.append(&widgets::field(
            &t!("soundboard.pad_volume").to_string(),
            &volume,
        ));

        let loop_switch = widgets::switch(pad.r#loop);
        content.append(&widgets::labelled(
            &t!("soundboard.pad_loop").to_string(),
            &loop_switch,
        ));

        let hotkey_labels: Vec<String> = std::iter::once(t!("soundboard.no_hotkey").to_string())
            .chain(board::HOTKEYS.iter().map(|key| (*key).to_string()))
            .collect();
        let hotkey_refs: Vec<&str> = hotkey_labels.iter().map(String::as_str).collect();
        let hotkey_dropdown = gtk::DropDown::new(
            Some(gtk::StringList::new(&hotkey_refs)),
            gtk::Expression::NONE,
        );
        let hotkey_index = pad
            .hotkey
            .as_deref()
            .and_then(|key| board::HOTKEYS.iter().position(|entry| *entry == key))
            .map_or(0, |index| index + 1);
        hotkey_dropdown.set_selected(hotkey_index as u32);
        content.append(&widgets::field(
            &t!("soundboard.pad_hotkey").to_string(),
            &hotkey_dropdown,
        ));

        let error_label = gtk::Label::new(None);
        error_label.add_css_class("error");
        error_label.set_xalign(0.0);
        error_label.set_wrap(true);
        error_label.set_visible(false);
        content.append(&error_label);

        toolbar.set_content(Some(&content));
        window.set_content(Some(&toolbar));

        {
            let window = window.clone();
            cancel.connect_clicked(move |_| window.close());
        }

        {
            let controller = self.controller.clone();
            let weak = self.weak_self.borrow().clone();
            let window = window.clone();
            let pad_id = pad.id.clone();
            let old_asset = pad.asset.clone();
            let label_entry = label_entry.clone();
            let color_button = color_button.clone();
            let dropdown = dropdown.clone();
            let rels = rels.clone();
            let volume = volume.clone();
            let loop_switch = loop_switch.clone();
            let hotkey_dropdown = hotkey_dropdown.clone();
            let error_label = error_label.clone();
            save.connect_clicked(move |_| {
                let hotkey = selected_hotkey(&hotkey_dropdown);
                if let Some(key) = hotkey.as_deref() {
                    let taken = controller.with_project(|project| {
                        project.soundboard.hotkey_taken(key, Some(&pad_id))
                    });
                    if taken {
                        error_label.set_text(&t!("soundboard.hotkey_taken").to_string());
                        error_label.set_visible(true);
                        return;
                    }
                }
                error_label.set_visible(false);

                let index = dropdown.selected() as usize;
                let asset = rels
                    .borrow()
                    .get(index)
                    .cloned()
                    .filter(|rel| !rel.is_empty());
                let label = label_entry.text().to_string();
                let color = widgets::color_hex(&color_button);
                let volume_value = volume.value();
                let looped = loop_switch.is_active();

                let Some(page) = weak.upgrade() else {
                    return;
                };
                let asset_for_save = asset.clone();
                let save_pad_id = pad_id.clone();
                page.commit(move |project| {
                    if let Some(pad) = project.soundboard.pad_mut(&save_pad_id) {
                        pad.label = label;
                        pad.color = color;
                        pad.asset = asset_for_save;
                        pad.volume = volume_value;
                        pad.r#loop = looped;
                        pad.hotkey = hotkey;
                        pad.normalize();
                    }
                });
                if old_asset != asset {
                    page.engine.borrow_mut().stop(&pad_id);
                }
                page.apply_playing();
                window.close();
            });
        }

        window.present();
    }
}

impl Page for SoundboardPage {
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    fn refresh(&self) {
        SoundboardPage::refresh(self);
    }
}

fn set_playing_class(button: &gtk::Button, playing: bool) {
    if playing {
        button.add_css_class("playing");
    } else {
        button.remove_css_class("playing");
    }
}

fn selected_hotkey(dropdown: &gtk::DropDown) -> Option<String> {
    let index = dropdown.selected() as usize;
    index
        .checked_sub(1)
        .and_then(|index| board::HOTKEYS.get(index))
        .map(|key| (*key).to_string())
}
