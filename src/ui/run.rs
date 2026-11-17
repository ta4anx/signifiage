//! Run page: preview monitor, transport, timers, quick queue and patterns.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use rust_i18n::t;

use crate::model::layer::{CaptureSource, LayerKind, PatternKind};
use crate::services::controller::{Controller, DeckCall, ProgramSource, ViewRole};
use crate::services::deck::{DeckBackend, DeckController};
use crate::ui::render::{SceneView, ViewMode};
use crate::ui::{Page, widgets};

/// Commands sent to the deck worker thread.
enum DeckRequest {
    Next { keysym: u32, target: Option<u32> },
    Prev { keysym: u32, target: Option<u32> },
}

enum DeckResult {
    Ok,
    Failed(String),
}

struct DeckWorker {
    tx: std::sync::mpsc::Sender<DeckRequest>,
    rx: std::sync::mpsc::Receiver<DeckResult>,
}

impl DeckWorker {
    fn spawn() -> Option<Self> {
        let backend = if crate::services::x11::display_available() {
            DeckBackend::X11
        } else {
            DeckBackend::Portal
        };
        let (req_tx, req_rx) = std::sync::mpsc::channel::<DeckRequest>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<DeckResult>();
        let spawned = std::thread::Builder::new()
            .name("signifiage-deck".to_string())
            .spawn(move || {
                let controller = DeckController::new(backend);
                while let Ok(request) = req_rx.recv() {
                    let outcome = match request {
                        DeckRequest::Next { keysym, target } => controller.next(keysym, target),
                        DeckRequest::Prev { keysym, target } => controller.prev(keysym, target),
                    };
                    let result = match outcome {
                        Ok(()) => DeckResult::Ok,
                        Err(err) => DeckResult::Failed(err.message().to_string()),
                    };
                    if res_tx.send(result).is_err() {
                        break;
                    }
                }
            });
        match spawned {
            Ok(_) => Some(Self {
                tx: req_tx,
                rx: res_rx,
            }),
            Err(err) => {
                tracing::warn!("cannot start the deck worker: {err}");
                None
            }
        }
    }
}

pub struct RunPage {
    pub root: gtk::Box,
    controller: Rc<Controller>,
    overlay: adw::ToastOverlay,
    preview: SceneView,
    program_label: gtk::Label,
    status_label: gtk::Label,
    remaining_label: gtk::Label,
    playlist_list: gtk::ListBox,
    playlist_signature: RefCell<String>,
    timer_box: gtk::Box,
    timer_signature: RefCell<String>,
    auto_switch: gtk::Switch,
    loop_switch: gtk::Switch,
    black_button: gtk::ToggleButton,
    freeze_button: gtk::ToggleButton,
    mute_button: gtk::ToggleButton,

    deck_card: gtk::Box,
    deck_status: gtk::Label,
    deck_worker: RefCell<Option<DeckWorker>>,
    deck_error_at: RefCell<Option<std::time::Instant>>,
    updating: RefCell<bool>,
}

impl RunPage {
    pub fn new(controller: Rc<Controller>, overlay: adw::ToastOverlay) -> Rc<Self> {
        let preview = SceneView::new(ViewMode::Preview);
        preview.set_hexpand(true);
        preview.set_vexpand(true);

        let program_label = gtk::Label::new(None);
        program_label.add_css_class("title-1");
        program_label.set_xalign(0.0);
        let status_label = gtk::Label::new(None);
        status_label.add_css_class("dim-label");
        status_label.set_xalign(0.0);
        let remaining_label = gtk::Label::new(None);
        remaining_label.add_css_class("mono-time");
        remaining_label.set_xalign(1.0);
        remaining_label.set_hexpand(true);

        let black_button = gtk::ToggleButton::with_label(&t!("show.black").to_string());
        let freeze_button = gtk::ToggleButton::with_label(&t!("show.freeze").to_string());
        let mute_button = gtk::ToggleButton::with_label(&t!("show.mute").to_string());

        let auto_switch = widgets::switch(true);
        auto_switch.set_tooltip_text(Some(&t!("show.auto_advance").to_string()));
        let loop_switch = widgets::switch(true);
        loop_switch.set_tooltip_text(Some(&t!("show.loop").to_string()));

        let playlist_list = gtk::ListBox::new();
        playlist_list.set_selection_mode(gtk::SelectionMode::Single);
        playlist_list.add_css_class("boxed-list");

        let timer_box = gtk::Box::new(gtk::Orientation::Vertical, 6);

        let page = Rc::new(Self {
            root: gtk::Box::new(gtk::Orientation::Vertical, 0),
            controller: controller.clone(),
            overlay: overlay.clone(),
            preview: preview.clone(),
            program_label: program_label.clone(),
            status_label: status_label.clone(),
            remaining_label: remaining_label.clone(),
            playlist_list: playlist_list.clone(),
            playlist_signature: RefCell::new(String::new()),
            timer_box: timer_box.clone(),
            timer_signature: RefCell::new(String::new()),
            auto_switch: auto_switch.clone(),
            loop_switch: loop_switch.clone(),
            black_button: black_button.clone(),
            freeze_button: freeze_button.clone(),
            mute_button: mute_button.clone(),

            deck_card: gtk::Box::new(gtk::Orientation::Horizontal, 6),
            deck_status: gtk::Label::new(None),
            deck_worker: RefCell::new(DeckWorker::spawn()),
            deck_error_at: RefCell::new(None),
            updating: RefCell::new(false),
        });

        controller.attach_view(ViewRole::Preview, &preview);

        // Preview area.
        let preview_frame = gtk::Frame::new(None);
        preview_frame.add_css_class("scene-canvas");
        preview_frame.set_child(Some(&preview));
        preview_frame.set_vexpand(true);
        preview_frame.set_hexpand(true);

        // Transport.
        let go = widgets::suggested(&t!("show.go").to_string());
        let prev = widgets::button(&t!("show.prev").to_string());
        let patterns = build_patterns_menu(&controller);

        {
            let controller = controller.clone();
            go.connect_clicked(move |_| {
                if !controller.go() {
                    // `go` already toasts when there is nothing to take.
                }
            });
        }
        {
            let controller = controller.clone();
            prev.connect_clicked(move |_| {
                controller.prev();
            });
        }
        {
            let controller = controller.clone();
            black_button.connect_toggled(move |button| {
                controller.set_black(button.is_active());
            });
        }
        {
            let controller = controller.clone();
            freeze_button.connect_toggled(move |button| controller.set_frozen(button.is_active()));
        }
        {
            let controller = controller.clone();
            mute_button.connect_toggled(move |button| controller.set_muted(button.is_active()));
        }
        {
            let controller = controller.clone();
            let overlay = overlay.clone();
            auto_switch.connect_state_set(move |_, state| {
                controller.set_auto(state);
                overlay.add_toast(adw::Toast::new(&t!("show.auto_advance_hint").to_string()));
                gtk::glib::Propagation::Proceed
            });
        }
        {
            let controller = controller.clone();
            loop_switch.connect_state_set(move |_, state| {
                let playlist_id = controller.preview_state().playlist_id;
                controller.mutate(Some("playlist-loop"), |project| {
                    if let Some(playlist) = project.playlist_mut(&playlist_id) {
                        playlist.r#loop = state;
                    }
                });
                gtk::glib::Propagation::Proceed
            });
        }

        let transport = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        transport.set_margin_top(8);
        transport.set_margin_bottom(8);
        transport.set_margin_start(12);
        transport.set_margin_end(12);
        transport.append(&go);
        transport.append(&prev);
        transport.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        transport.append(&black_button);
        transport.append(&freeze_button);
        transport.append(&mute_button);
        transport.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        transport.append(&widgets::inline(&t!("show.auto").to_string(), &auto_switch));
        transport.append(&widgets::inline(&t!("show.loop").to_string(), &loop_switch));
        transport.append(&patterns);
        transport.append(&widgets::dim_label(&t!("show.shortcuts_hint").to_string()));

        // Header with program name and remaining time.
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.set_margin_top(12);
        header.set_margin_start(12);
        header.set_margin_end(12);
        header.append(&program_label);
        header.append(&remaining_label);
        header.append(&status_label);

        // Bottom row: playlist, timers, quick queue.
        let left = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let (playlist_section, playlist_content) =
            widgets::section(&t!("run.playlist").to_string());
        playlist_content.append(&playlist_list);
        left.append(&playlist_section);
        left.set_width_request(320);

        let middle = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let (timer_section, timer_content) = widgets::section(&t!("run.timers").to_string());
        let edit_timers = widgets::button(&t!("run.edit_timers").to_string());
        {
            let controller = controller.clone();
            let overlay = overlay.clone();
            edit_timers.connect_clicked(move |_| {
                crate::ui::dialogs::timers_dialog(&controller, &overlay, None);
            });
        }
        timer_content.append(&edit_timers);
        timer_content.append(&timer_box);
        middle.append(&timer_section);
        middle.set_hexpand(true);

        // Deck results arrive from the worker thread; drain them on the UI tick.
        {
            let page = page.clone();
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
                page.poll_deck();
                gtk::glib::ControlFlow::Continue
            });
        }

        let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        bottom.set_margin_top(8);
        bottom.set_margin_bottom(12);
        bottom.set_margin_start(12);
        bottom.set_margin_end(12);
        bottom.append(&left);
        bottom.append(&middle);

        let page_clone = page.clone();
        playlist_list.connect_row_activated(move |_, row| {
            page_clone
                .controller
                .set_preview_index(row.index() as usize);
        });

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&header);
        body.append(&preview_frame);
        body.append(&transport);
        body.append(&bottom);
        let scrolled = gtk::ScrolledWindow::builder()
            .child(&body)
            .vexpand(true)
            .hexpand(true)
            .build();
        page.root.append(&scrolled);
        page.refresh();
        page
    }

    pub fn preview_view(&self) -> SceneView {
        self.preview.clone()
    }

    /// Send a deck command to the worker thread.
    pub fn deck_call(&self, call: DeckCall) {
        let project = self.controller.project_clone();
        let (next_keysym, prev_keysym) = (project.deck.next_keysym(), project.deck.prev_keysym());
        let program = self.controller.program();
        let target = program.scene.as_ref().and_then(|scene| {
            scene.layers.iter().find_map(|layer| match &layer.kind {
                LayerKind::Capture(props) => match &props.source {
                    CaptureSource::X11Window { xid, .. } if *xid != 0 => Some(*xid),
                    _ => None,
                },
                _ => None,
            })
        });
        let request = match call {
            DeckCall::Next => DeckRequest::Next {
                keysym: next_keysym,
                target,
            },
            DeckCall::Prev => DeckRequest::Prev {
                keysym: prev_keysym,
                target,
            },
        };
        let worker = self.deck_worker.borrow();
        match worker.as_ref() {
            Some(worker) => {
                if worker.tx.send(request).is_err() {
                    self.deck_status
                        .set_text(&t!("deck.unavailable").to_string());
                } else {
                    self.deck_status.set_text(&t!("deck.next").to_string());
                }
            }
            None => {
                self.deck_status
                    .set_text(&t!("deck.unavailable").to_string());
            }
        }
    }

    fn poll_deck(&self) {
        let mut results = Vec::new();
        {
            let worker = self.deck_worker.borrow();
            if let Some(worker) = worker.as_ref() {
                while let Ok(result) = worker.rx.try_recv() {
                    results.push(result);
                }
            }
        }
        if results.is_empty() {
            return;
        }
        let live = self.controller.live();
        for result in results {
            match result {
                DeckResult::Ok => {
                    *live.deck_error.borrow_mut() = String::new();
                    *self.deck_error_at.borrow_mut() = None;
                    self.deck_status.set_text(&t!("deck.title").to_string());
                }
                DeckResult::Failed(message) => {
                    *live.deck_error.borrow_mut() = message.clone();
                    *self.deck_error_at.borrow_mut() = Some(std::time::Instant::now());
                    self.deck_status.set_text(&message);
                }
            }
        }
        live.deck_available.set(self.deck_worker.borrow().is_some());
        live.deck_backend
            .replace(self.controller.project_clone().deck.next_key.clone());
        self.controller.publish_state();
    }

    fn deck_visible(&self) -> bool {
        self.controller
            .program()
            .scene
            .as_ref()
            .is_some_and(|scene| scene.has_capture())
    }
}

fn build_patterns_menu(controller: &Rc<Controller>) -> gtk::MenuButton {
    let button = gtk::MenuButton::new();
    button.set_label(&t!("show.patterns").to_string());
    let popover = gtk::Popover::new();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    for kind in PatternKind::ALL {
        let item = widgets::button(&t!(kind.label_key()).to_string());
        let controller = controller.clone();
        item.connect_clicked(move |_| controller.show_pattern(kind));
        list.append(&item);
    }
    let resume = widgets::button(&t!("pattern.resume").to_string());
    {
        let controller = controller.clone();
        resume.connect_clicked(move |_| controller.resume_program());
    }
    list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    list.append(&resume);
    popover.set_child(Some(&list));
    button.set_popover(Some(&popover));
    button
}

impl Page for RunPage {
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    fn refresh(&self) {
        if *self.updating.borrow() {
            return;
        }
        *self.updating.borrow_mut() = true;
        let controller = &self.controller;

        let program = controller.program();
        self.program_label.set_text(&controller.program_name());
        let source = match &program.source {
            ProgramSource::Black => t!("show.source.black").to_string(),
            ProgramSource::Playlist { .. } => t!("show.source.playlist").to_string(),
            ProgramSource::Manual { .. } => t!("show.source.manual").to_string(),
            ProgramSource::Pattern { .. } => t!("show.source.pattern").to_string(),
        };
        self.status_label.set_text(&source);
        let remaining = controller
            .program_remaining()
            .map(|secs| crate::services::timers::format_eta(Some(secs.ceil() as i64)))
            .unwrap_or_else(|| crate::services::timers::MISSING.to_string());
        self.remaining_label.set_text(&remaining);

        self.black_button.set_active(controller.is_black());
        self.freeze_button.set_active(controller.is_frozen());
        self.mute_button.set_active(controller.is_muted());
        self.auto_switch.set_active(controller.auto_advance());
        if let Some(playlist) = controller.preview_playlist() {
            self.loop_switch.set_active(playlist.r#loop);
        }

        self.refresh_playlist();
        self.refresh_timers();

        self.deck_card.set_visible(self.deck_visible());
        let live = self.controller.live();
        if self
            .deck_error_at
            .borrow()
            .is_some_and(|at| at.elapsed().as_secs() >= 5)
        {
            *self.deck_error_at.borrow_mut() = None;
            live.deck_error.borrow_mut().clear();
        }
        live.deck_available.set(self.deck_worker.borrow().is_some());
        live.deck_backend
            .replace(if crate::services::x11::display_available() {
                "x11".to_string()
            } else {
                "portal".to_string()
            });
        live.deck_hint.replace(t!("deck.hint").to_string());
        let _ = &self.overlay;
        *self.updating.borrow_mut() = false;
    }
}

impl RunPage {
    fn refresh_playlist(&self) {
        let preview = self.controller.preview_state();
        let program_index = self.controller.program().source.playlist_index();
        let playlist = self.controller.preview_playlist();
        let signature = match &playlist {
            Some(playlist) => format!(
                "{}|{}|{}|{}|{}",
                playlist.id,
                playlist.items.len(),
                preview.index,
                program_index.map(|i| i as i64).unwrap_or(-1),
                playlist.r#loop
            ),
            None => "none".to_string(),
        };
        if *self.playlist_signature.borrow() == signature {
            return;
        }
        *self.playlist_signature.borrow_mut() = signature;

        while let Some(child) = self.playlist_list.first_child() {
            self.playlist_list.remove(&child);
        }
        let Some(playlist) = playlist else {
            return;
        };
        let project = self.controller.project_clone();
        for (index, item) in playlist.items.iter().enumerate() {
            let name = project
                .scene(&item.scene_id)
                .map(|scene| scene.name.clone())
                .unwrap_or_else(|| t!("scene.missing").to_string());
            let row = gtk::ListBoxRow::new();
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            line.set_margin_top(4);
            line.set_margin_bottom(4);
            line.set_margin_start(6);
            line.set_margin_end(6);
            let mut label = format!("{:02}  {}", index + 1, name);
            if item.duration_secs > 0 {
                label.push_str(&format!("  ({}s)", item.duration_secs));
            }
            let text = gtk::Label::new(Some(&label));
            text.set_xalign(0.0);
            text.set_hexpand(true);
            line.append(&text);
            if program_index == Some(index) {
                let badge = gtk::Label::new(Some(&t!("show.on_air").to_string()));
                badge.add_css_class("signifiage-onair");
                line.append(&badge);
            }
            if preview.index == index {
                let badge = gtk::Label::new(Some(&t!("run.next_marker").to_string()));
                badge.add_css_class("signifiage-badge");
                badge.add_css_class("accent");
                line.append(&badge);
            }
            row.set_child(Some(&line));
            self.playlist_list.append(&row);
        }
    }

    fn refresh_timers(&self) {
        let snapshots = self.controller.timer_snapshots();
        let signature: String = snapshots
            .iter()
            .map(|snapshot| snapshot.id.clone())
            .collect::<Vec<_>>()
            .join(",");
        if *self.timer_signature.borrow() != signature {
            *self.timer_signature.borrow_mut() = signature;
            while let Some(child) = self.timer_box.first_child() {
                self.timer_box.remove(&child);
            }
            for snapshot in &snapshots {
                self.timer_box
                    .append(&self.timer_card(&snapshot.id, &snapshot.name));
            }
        }
        // Update values in place.
        let mut child = self.timer_box.first_child();
        let mut index = 0;
        while let Some(widget) = child {
            if let Some(row) = widget.downcast_ref::<gtk::Box>()
                && let Some(time_label) = row
                    .last_child()
                    .and_then(|child| child.downcast::<gtk::Label>().ok())
                && let Some(snapshot) = snapshots.get(index)
            {
                let text = match snapshot.remaining_secs {
                    Some(remaining) => crate::services::timers::format_remaining(remaining),
                    None => crate::services::timers::format_hms(snapshot.elapsed_secs),
                };
                time_label.set_text(&text);
                if snapshot.running {
                    time_label.remove_css_class("dim-label");
                    time_label.add_css_class("accent");
                } else {
                    time_label.remove_css_class("accent");
                    time_label.add_css_class("dim-label");
                }
            }
            child = widget.next_sibling();
            index += 1;
        }
    }

    fn timer_card(&self, id: &str, name: &str) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let label = gtk::Label::new(Some(name));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        let toggle = widgets::button(&t!("timer.start").to_string());
        let reset = widgets::button(&t!("timer.reset").to_string());
        let time = gtk::Label::new(Some(crate::services::timers::MISSING));
        time.add_css_class("mono-time");
        time.add_css_class("dim-label");
        time.set_width_chars(9);
        time.set_xalign(1.0);

        {
            let controller = self.controller.clone();
            let toggle_clone = toggle.clone();
            let id = id.to_string();
            toggle.connect_clicked(move |_| {
                let running = controller.timer_toggle(&id);
                let _ = running;
                let now_running = controller
                    .timer_snapshots()
                    .iter()
                    .find(|snapshot| snapshot.id == id)
                    .map(|snapshot| snapshot.running)
                    .unwrap_or(false);
                toggle_clone.set_label(&if now_running {
                    t!("timer.pause").to_string()
                } else {
                    t!("timer.start").to_string()
                });
            });
        }
        {
            let controller = self.controller.clone();
            let id = id.to_string();
            reset.connect_clicked(move |_| {
                controller.timer_reset(&id);
            });
        }
        row.append(&label);
        row.append(&time);
        row.append(&toggle);
        row.append(&reset);
        row
    }
}
