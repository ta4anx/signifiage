//! Queue page: current call, live rows, import wizard and spreadsheet polling.
//!
//! The page reads only through [`Controller`] so the queue layer, run page and
//! web remote stay in sync. Spreadsheet reads happen on worker threads; results
//! arrive through `std::sync::mpsc` and are applied on the GTK main thread.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::Duration;

use gtk::gio;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use rust_i18n::t;

use crate::model::queue::fold_ascii;
use crate::model::{Playlist, PlaylistItem, QueueRow, QueueSource, QueueStatus};
use crate::services::controller::Controller;
use crate::services::queue::{ImportReport, ImportRow};
use crate::services::queue_source::{SourceEvent, SourceWatcher};
use crate::services::spreadsheet::{self, ColumnMapping, SheetPreview};
use crate::ui::{Page, widgets};

pub struct QueuePage {
    pub root: gtk::Box,
    controller: Rc<Controller>,
    overlay: adw::ToastOverlay,
    import_button: gtk::Button,
    add_button: gtk::Button,
    undo_button: gtk::Button,
    reload_button: gtk::Button,
    source_label: gtk::Label,
    sync_label: gtk::Label,
    stats_label: gtk::Label,
    last_called_toggle: gtk::ToggleButton,
    next_toggle: gtk::ToggleButton,
    ticket_toggle: gtk::ToggleButton,
    random_button: gtk::Button,
    current_card: gtk::Box,
    current_signature: RefCell<String>,
    search: gtk::SearchEntry,
    filter: gtk::DropDown,
    rows_list: gtk::ListBox,
    rows_signature: RefCell<String>,
    watcher: RefCell<SourceWatcher>,
    sender: Sender<SourceEvent>,
    receiver: Receiver<SourceEvent>,
    source_signature: RefCell<Option<String>>,
    last_sync: RefCell<Option<String>>,
    last_event_error: RefCell<Option<String>>,
    last_pending: Cell<bool>,
}

impl QueuePage {
    pub fn new(controller: Rc<Controller>, overlay: adw::ToastOverlay) -> Rc<Self> {
        let import_button = widgets::suggested(&t!("queue.import").to_string());
        let add_button = widgets::button(&t!("queue.add").to_string());
        let undo_button = widgets::button(&t!("queue.undo").to_string());
        let reload_button = widgets::button(&t!("queue.reload").to_string());

        let source_label = gtk::Label::new(Some(&t!("queue.no_source").to_string()));
        source_label.set_xalign(0.0);
        source_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        source_label.set_hexpand(true);
        source_label.add_css_class("dim-label");

        let sync_label = widgets::dim_label("");
        sync_label.set_xalign(1.0);

        let stats_label = widgets::dim_label("");

        let last_called_toggle =
            gtk::ToggleButton::with_label(&t!("queue.show_last_called").to_string());
        last_called_toggle.set_active(true);
        last_called_toggle.set_tooltip_text(Some(&t!("queue.show_last_called_hint").to_string()));
        let next_toggle = gtk::ToggleButton::with_label(&t!("queue.show_next").to_string());
        next_toggle.set_active(true);
        next_toggle.set_tooltip_text(Some(&t!("queue.show_next_hint").to_string()));
        let ticket_toggle = gtk::ToggleButton::with_label(&t!("queue.show_ticket").to_string());
        ticket_toggle.set_active(true);
        ticket_toggle.set_tooltip_text(Some(&t!("queue.show_ticket_hint").to_string()));
        let call_person_button = widgets::button(&t!("queue.call_person").to_string());
        let random_button = widgets::button(&t!("queue.random_pick").to_string());
        let restart_button = widgets::button(&t!("queue.restart").to_string());
        restart_button.add_css_class("destructive-action");

        let current_card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        current_card.add_css_class("card");
        current_card.set_margin_top(6);
        current_card.set_margin_bottom(6);

        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some(&t!("queue.search_placeholder").to_string()));
        search.set_hexpand(true);

        let filter_labels: Vec<String> = std::iter::once(t!("common.all").to_string())
            .chain(
                QueueStatus::ALL
                    .iter()
                    .map(|status| t!(status.label_key()).to_string()),
            )
            .collect();
        let filter = widgets::dropdown(&filter_labels, 0);

        let rows_list = gtk::ListBox::new();
        rows_list.set_selection_mode(gtk::SelectionMode::None);
        rows_list.add_css_class("boxed-list");

        let (sender, receiver) = mpsc::channel();

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let header = gtk::Box::new(gtk::Orientation::Vertical, 6);
        header.set_margin_top(12);
        header.set_margin_bottom(6);
        header.set_margin_start(12);
        header.set_margin_end(12);

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.append(&import_button);
        actions.append(&add_button);
        actions.append(&undo_button);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        actions.append(&call_person_button);
        actions.append(&random_button);
        actions.append(&restart_button);
        actions.append(&spacer);
        actions.append(&reload_button);
        header.append(&actions);

        let display_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        display_row.append(&last_called_toggle);
        display_row.append(&next_toggle);
        display_row.append(&ticket_toggle);
        header.append(&display_row);

        let source_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        source_row.append(&source_label);
        source_row.append(&sync_label);
        header.append(&source_row);
        header.append(&stats_label);
        root.append(&header);

        let card_holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card_holder.set_margin_start(12);
        card_holder.set_margin_end(12);
        card_holder.append(&current_card);
        root.append(&card_holder);

        let filter_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        filter_row.set_margin_top(6);
        filter_row.set_margin_bottom(6);
        filter_row.set_margin_start(12);
        filter_row.set_margin_end(12);
        filter_row.append(&search);
        filter_row.append(&filter);
        root.append(&filter_row);

        let scrolled = gtk::ScrolledWindow::builder()
            .child(&rows_list)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        scrolled.set_margin_start(12);
        scrolled.set_margin_end(12);
        scrolled.set_margin_bottom(12);
        root.append(&scrolled);

        let page = Rc::new(Self {
            root,
            controller: controller.clone(),
            overlay: overlay.clone(),
            import_button: import_button.clone(),
            add_button: add_button.clone(),
            undo_button: undo_button.clone(),
            reload_button: reload_button.clone(),
            source_label: source_label.clone(),
            sync_label: sync_label.clone(),
            stats_label: stats_label.clone(),
            last_called_toggle: last_called_toggle.clone(),
            next_toggle: next_toggle.clone(),
            ticket_toggle: ticket_toggle.clone(),
            random_button: random_button.clone(),
            current_card: current_card.clone(),
            current_signature: RefCell::new(String::new()),
            search: search.clone(),
            filter: filter.clone(),
            rows_list: rows_list.clone(),
            rows_signature: RefCell::new(String::from("<initial>")),
            watcher: RefCell::new(SourceWatcher::new()),
            sender,
            receiver,
            source_signature: RefCell::new(None),
            last_sync: RefCell::new(None),
            last_event_error: RefCell::new(None),
            last_pending: Cell::new(false),
        });

        {
            let controller = controller.clone();
            let overlay = overlay.clone();
            import_button.connect_clicked(move |_| {
                open_import(&controller, &overlay, window_of(&overlay));
            });
        }
        {
            let controller = controller.clone();
            let overlay = overlay.clone();
            undo_button.connect_clicked(move |_| {
                if !controller.queue_undo() {
                    widgets::toast(&overlay, &t!("queue.toast.nothing_to_undo").to_string());
                }
            });
        }
        {
            let controller = controller.clone();
            let overlay = overlay.clone();
            add_button.connect_clicked(move |_| {
                if !controller.is_open() {
                    return;
                }
                QueuePage::edit_row_dialog(
                    controller.clone(),
                    overlay.clone(),
                    window_of(&overlay),
                    None,
                );
            });
        }
        {
            let controller = controller.clone();
            last_called_toggle.connect_toggled(move |button| {
                controller.set_queue_display(Some(button.is_active()), None, None);
            });
        }
        {
            let controller = controller.clone();
            next_toggle.connect_toggled(move |button| {
                controller.set_queue_display(None, Some(button.is_active()), None);
            });
        }
        {
            let controller = controller.clone();
            ticket_toggle.connect_toggled(move |button| {
                controller.set_queue_display(None, None, Some(button.is_active()));
            });
        }
        {
            let controller = controller.clone();
            let overlay = overlay.clone();
            call_person_button.connect_clicked(move |_| {
                call_person_dialog(&controller, &overlay);
            });
        }
        {
            let controller = controller.clone();
            let overlay = overlay.clone();
            let random_button = random_button.clone();
            random_button.connect_clicked(move |_| match controller.queue_call_random() {
                Some(row) => widgets::toast(
                    &overlay,
                    &t!("queue.toast.random_picked", name = row.name).to_string(),
                ),
                None => widgets::toast(&overlay, &t!("queue.toast.nobody_waiting").to_string()),
            });
        }
        {
            let controller = controller.clone();
            let overlay = overlay.clone();
            restart_button.connect_clicked(move |_| {
                let controller = controller.clone();
                let overlay = overlay.clone();
                widgets::confirm(
                    None,
                    &t!("queue.restart").to_string(),
                    &t!("queue.restart_confirm").to_string(),
                    move || {
                        if controller.queue_restart() {
                            widgets::toast(&overlay, &t!("queue.toast.restarted").to_string());
                        }
                    },
                );
            });
        }
        {
            let page = Rc::downgrade(&page);
            let overlay = overlay.clone();
            reload_button.connect_clicked(move |_| {
                let Some(page) = page.upgrade() else {
                    return;
                };
                if !page.watcher.borrow_mut().force_reload(&page.sender) {
                    widgets::toast(&overlay, &t!("queue.no_source").to_string());
                }
            });
        }
        {
            let page = Rc::downgrade(&page);
            search.connect_search_changed(move |_| {
                if let Some(page) = page.upgrade() {
                    page.refresh_rows();
                }
            });
        }
        {
            let page = Rc::downgrade(&page);
            filter.connect_selected_notify(move |_| {
                if let Some(page) = page.upgrade() {
                    page.refresh_rows();
                }
            });
        }

        // Poll the source channel and the workbook stamp.
        {
            let weak = Rc::downgrade(&page);
            glib::timeout_add_local(Duration::from_millis(100), move || {
                if let Some(page) = weak.upgrade() {
                    page.on_tick();
                }
                glib::ControlFlow::Continue
            });
        }

        page.refresh();
        page
    }

    /// Drain finished background reads and apply them on the main thread.
    fn on_tick(&self) {
        self.drain_source();
        let pending = {
            let mut watcher = self.watcher.borrow_mut();
            let _enabled = watcher.tick(&self.sender);
            watcher.is_pending()
        };
        if pending != self.last_pending.get() {
            self.last_pending.set(pending);
            self.refresh_sync_label();
        }
    }

    fn drain_source(&self) {
        loop {
            match self.receiver.try_recv() {
                Ok(event) => {
                    self.watcher.borrow_mut().complete();
                    self.apply_event(event);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    fn apply_event(&self, event: SourceEvent) {
        match event {
            SourceEvent::Rows(rows) => {
                let _report = self.controller.queue_merge_import(&rows);
                *self.last_event_error.borrow_mut() = None;
                self.watcher.borrow_mut().clear_error();
                self.last_sync.replace(Some(clock_now()));
                self.refresh_header();
            }
            SourceEvent::Error(message) => {
                let duplicate = self
                    .last_event_error
                    .borrow()
                    .as_deref()
                    .is_some_and(|previous| previous == message);
                if !duplicate {
                    *self.last_event_error.borrow_mut() = Some(message.clone());
                    widgets::toast(
                        &self.overlay,
                        &t!("queue.source_error", message = message).to_string(),
                    );
                    self.refresh_sync_label();
                }
            }
        }
    }

    fn window(&self) -> Option<gtk::Window> {
        self.root
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok())
    }

    fn resolved_source(&self) -> Option<QueueSource> {
        let mut source = self.controller.queue_source()?;
        if source.path.trim().is_empty() {
            return None;
        }
        let stored = PathBuf::from(source.path.trim());
        let path = if stored.is_absolute() {
            stored
        } else if let Some(dir) = self.controller.project_dir() {
            dir.join(stored)
        } else {
            stored
        };
        source.path = path.display().to_string();
        Some(source)
    }

    fn sync_source(&self) {
        let source = self.resolved_source();
        let signature = source.as_ref().map(source_signature).unwrap_or_default();
        if self.source_signature.borrow().as_deref() == Some(signature.as_str()) {
            return;
        }
        self.watcher.borrow_mut().set_source(source);
        *self.source_signature.borrow_mut() = Some(signature);
        // Results still queued from the previous source must not be applied.
        while self.receiver.try_recv().is_ok() {}
        self.last_sync.replace(None);
        self.last_event_error.replace(None);
        self.last_pending.set(false);
    }

    fn refresh_header(&self) {
        let open = self.controller.is_open();
        self.import_button.set_sensitive(open);
        self.add_button.set_sensitive(open);
        self.undo_button
            .set_sensitive(open && self.controller.queue_can_undo());
        let source = self.resolved_source();
        self.reload_button.set_sensitive(source.is_some());
        match &source {
            Some(source) => {
                let file = Path::new(&source.path)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| source.path.clone());
                self.source_label.set_text(&format!(
                    "{}: {} — {}",
                    t!("queue.source"),
                    file,
                    source.sheet
                ));
                self.source_label.set_tooltip_text(Some(&source.path));
            }
            None => {
                self.source_label
                    .set_text(&t!("queue.no_source").to_string());
                self.source_label.set_tooltip_text(None);
            }
        }
        let (show_last, show_next, show_ticket) = self.controller.queue_display();
        self.last_called_toggle.set_active(show_last);
        self.next_toggle.set_active(show_next);
        self.ticket_toggle.set_active(show_ticket);
        self.random_button
            .set_visible(self.controller.config().experimental.random_pick);
        let snapshot = self.controller.queue();
        self.stats_label.set_text(
            &t!(
                "queue.stats",
                waiting = snapshot.waiting,
                called = snapshot.called,
                done = snapshot.done,
                skipped = snapshot.skipped
            )
            .to_string(),
        );
        self.refresh_sync_label();
    }

    fn refresh_sync_label(&self) {
        let has_source = self.resolved_source().is_some();
        let text = if !has_source {
            t!("queue.no_source").to_string()
        } else if self.watcher.borrow().is_pending() {
            t!("queue.reading").to_string()
        } else {
            match self.last_sync.borrow().as_deref() {
                Some(time) => t!("queue.last_sync", time = time).to_string(),
                None => t!("queue.sync_never").to_string(),
            }
        };
        self.sync_label.set_text(&text);
    }

    fn refresh_current(&self) {
        let snapshot = self.controller.queue();
        let signature = match &snapshot.current {
            Some(row) => format!("{}|{}|{}|{}", row.id, row.ticket, row.name, row.note),
            None => "none".to_string(),
        };
        if *self.current_signature.borrow() == signature {
            return;
        }
        *self.current_signature.borrow_mut() = signature;

        while let Some(child) = self.current_card.first_child() {
            self.current_card.remove(&child);
        }
        let heading = gtk::Label::new(Some(&t!("queue.display_title").to_string()));
        heading.set_xalign(0.0);
        heading.add_css_class("heading");
        self.current_card.append(&heading);

        match &snapshot.current {
            Some(row) => {
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                let ticket = gtk::Label::new(Some(&row.display_ticket()));
                ticket.set_xalign(1.0);
                ticket.set_width_chars(4);
                ticket.add_css_class("title-1");
                ticket.add_css_class("mono-time");
                line.append(&ticket);
                let name = gtk::Label::new(Some(&row.name));
                name.set_xalign(0.0);
                name.set_hexpand(true);
                name.set_wrap(true);
                name.add_css_class("title-2");
                line.append(&name);
                self.current_card.append(&line);
                if !row.note.trim().is_empty() {
                    let note = widgets::wrap_label(&row.note);
                    note.add_css_class("dim-label");
                    self.current_card.append(&note);
                }
            }
            None => {
                let empty = widgets::dim_label(&t!("queue.no_current").to_string());
                empty.add_css_class("title-3");
                self.current_card.append(&empty);
            }
        }

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let next = widgets::suggested(&t!("queue.next").to_string());
        let recall = widgets::button(&t!("queue.recall").to_string());
        let done = widgets::button(&t!("queue.done").to_string());
        let skip = widgets::button(&t!("queue.skip").to_string());
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            next.connect_clicked(move |_| {
                if !controller.queue_next() {
                    widgets::toast(&overlay, &t!("queue.toast.nobody_waiting").to_string());
                }
            });
        }
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            recall.connect_clicked(move |_| {
                if !controller.queue_recall() {
                    widgets::toast(&overlay, &t!("queue.toast.no_current").to_string());
                }
            });
        }
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            done.connect_clicked(move |_| {
                if !controller.queue_done() {
                    widgets::toast(&overlay, &t!("queue.toast.no_current").to_string());
                }
            });
        }
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            skip.connect_clicked(move |_| {
                if !controller.queue_skip() {
                    widgets::toast(&overlay, &t!("queue.toast.no_current").to_string());
                }
            });
        }
        buttons.append(&next);
        buttons.append(&recall);
        buttons.append(&done);
        buttons.append(&skip);
        self.current_card.append(&buttons);
    }

    fn refresh_rows(&self) {
        let query = fold_ascii(&self.search.text());
        let status_filter = match widgets::selected(&self.filter) {
            0 => None,
            index => QueueStatus::ALL.get(index - 1).copied(),
        };
        let rows: Vec<QueueRow> = self
            .controller
            .queue_rows()
            .into_iter()
            .filter(|row| {
                if let Some(status) = status_filter
                    && row.status != status
                {
                    return false;
                }
                if query.is_empty() {
                    return true;
                }
                fold_ascii(&row.name).contains(&query)
                    || fold_ascii(&row.ticket).contains(&query)
                    || fold_ascii(&row.note).contains(&query)
            })
            .collect();

        let signature = rows
            .iter()
            .map(|row| {
                format!(
                    "{}|{}|{}|{}|{}",
                    row.id,
                    row.ticket,
                    row.name,
                    row.note,
                    row.status.id()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if *self.rows_signature.borrow() == signature {
            return;
        }
        *self.rows_signature.borrow_mut() = signature;

        while let Some(child) = self.rows_list.first_child() {
            self.rows_list.remove(&child);
        }
        if rows.is_empty() {
            let row = gtk::ListBoxRow::new();
            row.set_activatable(false);
            row.set_selectable(false);
            row.set_child(Some(&widgets::dim_label(
                &t!("queue.rows_empty").to_string(),
            )));
            self.rows_list.append(&row);
            return;
        }
        for row in rows {
            self.rows_list.append(&self.build_row(row));
        }
    }

    fn build_row(&self, row: QueueRow) -> gtk::ListBoxRow {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        line.set_margin_top(6);
        line.set_margin_bottom(6);
        line.set_margin_start(10);
        line.set_margin_end(10);

        let ticket = gtk::Label::new(Some(&row.display_ticket()));
        ticket.set_xalign(1.0);
        ticket.set_width_chars(4);
        ticket.add_css_class("mono-time");
        ticket.add_css_class("title-4");
        line.append(&ticket);

        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        let name = gtk::Label::new(Some(&row.name));
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.add_css_class("heading");
        text.append(&name);
        if !row.note.trim().is_empty() {
            let note = gtk::Label::new(Some(&row.note));
            note.set_xalign(0.0);
            note.set_ellipsize(gtk::pango::EllipsizeMode::End);
            note.add_css_class("caption");
            note.add_css_class("dim-label");
            text.append(&note);
        }
        line.append(&text);

        let badge = gtk::Label::new(Some(&t!(row.status.label_key()).to_string()));
        badge.add_css_class("signifiage-badge");
        badge.add_css_class(match row.status {
            QueueStatus::Waiting => "dim-label",
            QueueStatus::Called => "accent",
            QueueStatus::Done => "success",
            QueueStatus::Skipped => "warning",
        });
        line.append(&badge);

        if row.status == QueueStatus::Waiting {
            let call = widgets::button(&t!("queue.call").to_string());
            {
                let controller = self.controller.clone();
                let overlay = self.overlay.clone();
                let id = row.id.clone();
                call.connect_clicked(move |_| {
                    if !controller.queue_call(&id) {
                        widgets::toast(&overlay, &t!("queue.toast.not_waiting").to_string());
                    }
                });
            }
            line.append(&call);
        }

        let edit = widgets::button(&t!("queue.edit").to_string());
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            let existing = row.clone();
            edit.connect_clicked(move |_| {
                QueuePage::edit_row_dialog(
                    controller.clone(),
                    overlay.clone(),
                    window_of(&overlay),
                    Some(existing.clone()),
                );
            });
        }
        line.append(&edit);

        let remove = widgets::icon_button("user-trash-symbolic", &t!("queue.remove").to_string());
        {
            let controller = self.controller.clone();
            let overlay = self.overlay.clone();
            let parent = self.window();
            let id = row.id.clone();
            let name = row.name.clone();
            remove.connect_clicked(move |_| {
                let controller = controller.clone();
                let overlay = overlay.clone();
                let id = id.clone();
                widgets::confirm(
                    parent.as_ref(),
                    &t!("queue.remove").to_string(),
                    &t!("queue.remove_confirm", name = name.clone()).to_string(),
                    move || {
                        if controller.queue_remove(&id) {
                            widgets::toast(&overlay, &t!("queue.toast.removed").to_string());
                        }
                    },
                );
            });
        }
        line.append(&remove);

        let list_row = gtk::ListBoxRow::new();
        list_row.set_activatable(false);
        list_row.set_selectable(false);
        list_row.set_child(Some(&line));
        list_row
    }

    fn edit_row_dialog(
        controller: Rc<Controller>,
        overlay: adw::ToastOverlay,
        parent: Option<gtk::Window>,
        row: Option<QueueRow>,
    ) {
        if !controller.is_open() {
            return;
        }
        let title = if row.is_some() {
            t!("queue.edit_title").to_string()
        } else {
            t!("queue.add_title").to_string()
        };
        let window = adw::Window::builder()
            .modal(true)
            .default_width(460)
            .title(title)
            .build();
        if let Some(parent) = parent.as_ref() {
            window.set_transient_for(Some(parent));
        }

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());

        let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
        body.set_margin_top(12);
        body.set_margin_bottom(18);
        body.set_margin_start(18);
        body.set_margin_end(18);

        let name = widgets::entry(row.as_ref().map(|row| row.name.as_str()).unwrap_or(""));
        name.set_placeholder_text(Some(&t!("queue.name").to_string()));
        let note = widgets::entry(row.as_ref().map(|row| row.note.as_str()).unwrap_or(""));
        note.set_placeholder_text(Some(&t!("queue.note").to_string()));
        let ticket = widgets::entry(row.as_ref().map(|row| row.ticket.as_str()).unwrap_or(""));
        ticket.set_placeholder_text(Some(&t!("queue.ticket_placeholder").to_string()));
        body.append(&widgets::field(&t!("queue.name").to_string(), &name));
        body.append(&widgets::field(&t!("queue.note").to_string(), &note));
        body.append(&widgets::field(&t!("queue.ticket").to_string(), &ticket));
        body.append(&widgets::dim_label(
            &t!("queue.ticket_placeholder").to_string(),
        ));

        let error = gtk::Label::new(None);
        error.set_xalign(0.0);
        error.set_wrap(true);
        error.add_css_class("error");
        body.append(&error);

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        let cancel = widgets::button(&t!("common.cancel").to_string());
        let save = widgets::suggested(&t!("common.save").to_string());
        buttons.append(&cancel);
        buttons.append(&save);
        body.append(&buttons);

        toolbar.set_content(Some(&body));
        window.set_content(Some(&toolbar));

        {
            let window = window.clone();
            cancel.connect_clicked(move |_| window.close());
        }
        {
            let controller = controller.clone();
            let overlay = overlay.clone();
            let window = window.clone();
            let name = name.clone();
            let note = note.clone();
            let ticket = ticket.clone();
            let error = error.clone();
            let editing = row.clone();
            save.connect_clicked(move |_| {
                let name_text = name.text().to_string();
                let note_text = note.text().to_string();
                let ticket_text = ticket.text().to_string();
                if name_text.trim().is_empty() {
                    error.set_text(&t!("queue.toast.name_required").to_string());
                    return;
                }
                let ticket_trimmed = ticket_text.trim();
                if !ticket_trimmed.is_empty()
                    && controller.queue_rows().iter().any(|other| {
                        other.ticket.trim() == ticket_trimmed
                            && editing.as_ref().is_none_or(|row| row.id != other.id)
                    })
                {
                    error.set_text(&t!("queue.toast.duplicate_ticket").to_string());
                    return;
                }
                let saved = match &editing {
                    Some(row) => {
                        controller.queue_edit(&row.id, &name_text, &note_text, &ticket_text)
                    }
                    None => {
                        let before: Vec<String> = controller
                            .queue_rows()
                            .into_iter()
                            .map(|row| row.id)
                            .collect();
                        if !controller.queue_add(&name_text, &note_text) {
                            false
                        } else if ticket_trimmed.is_empty() {
                            true
                        } else {
                            // `add` allocates a ticket; honour a typed one.
                            match controller
                                .queue_rows()
                                .into_iter()
                                .find(|row| !before.contains(&row.id))
                            {
                                Some(row) => controller.queue_edit(
                                    &row.id,
                                    &name_text,
                                    &note_text,
                                    ticket_trimmed,
                                ),
                                None => true,
                            }
                        }
                    }
                };
                if !saved {
                    error.set_text(&t!("queue.toast.duplicate_ticket").to_string());
                    return;
                }
                if editing.is_none() {
                    widgets::toast(&overlay, &t!("queue.toast.added").to_string());
                }
                window.close();
            });
        }
        window.present();
    }
}

/// Pick a waiting person by name and call them (works with long queues).
fn call_person_dialog(controller: &Rc<Controller>, overlay: &adw::ToastOverlay) {
    let waiting: Vec<_> = controller
        .queue_rows()
        .into_iter()
        .filter(|row| row.status == QueueStatus::Waiting)
        .collect();
    let window = adw::Window::builder()
        .title(t!("queue.call_person").to_string())
        .modal(true)
        .default_width(460)
        .default_height(420)
        .build();
    if let Some(parent) = window_of(overlay) {
        window.set_transient_for(Some(&parent));
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.set_margin_top(8);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);

    if waiting.is_empty() {
        body.append(&widgets::dim_label(&t!("queue.no_waiting").to_string()));
    } else {
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some(&t!("queue.search_placeholder").to_string()));
        body.append(&search);
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("boxed-list");
        for row in &waiting {
            let item = gtk::ListBoxRow::new();
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            line.set_margin_top(6);
            line.set_margin_bottom(6);
            line.set_margin_start(8);
            line.set_margin_end(8);
            let text = if row.ticket.trim().is_empty() {
                row.name.clone()
            } else {
                format!(
                    "{}  {}",
                    crate::model::queue::pad_ticket(&row.ticket),
                    row.name
                )
            };
            let label = gtk::Label::new(Some(&text));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            line.append(&label);
            let call = widgets::suggested(&t!("queue.call").to_string());
            line.append(&call);
            item.set_child(Some(&line));
            item.set_widget_name(&row.name);
            {
                let controller = controller.clone();
                let window = window.clone();
                let id = row.id.clone();
                call.connect_clicked(move |_| {
                    controller.queue_call(&id);
                    window.close();
                });
            }
            list.append(&item);
        }
        let scrolled = gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .build();
        body.append(&scrolled);
        {
            let list = list.clone();
            search.connect_search_changed(move |entry| {
                let needle = entry.text().to_lowercase();
                let mut child = list.first_child();
                while let Some(row) = child {
                    child = row.next_sibling();
                    if let Some(row) = row.downcast_ref::<gtk::ListBoxRow>() {
                        let hit =
                            needle.is_empty() || row.widget_name().to_lowercase().contains(&needle);
                        row.set_visible(hit);
                    }
                }
            });
        }
    }

    let close = widgets::button(&t!("common.close").to_string());
    {
        let window = window.clone();
        close.connect_clicked(move |_| window.close());
    }
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    footer.set_halign(gtk::Align::End);
    footer.append(&close);
    body.append(&footer);
    toolbar.set_content(Some(&body));
    window.set_content(Some(&toolbar));
    window.present();
}

impl Page for QueuePage {
    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    fn refresh(&self) {
        self.sync_source();
        self.refresh_header();
        self.refresh_current();
        self.refresh_rows();
    }
}

// ---------------------------------------------------------------- import wizard

/// Pick a workbook and open the import wizard.
fn open_import(
    controller: &Rc<Controller>,
    overlay: &adw::ToastOverlay,
    parent: Option<gtk::Window>,
) {
    if !controller.is_open() {
        return;
    }
    let dialog = gtk::FileDialog::builder()
        .title(t!("queue.wizard.title").to_string())
        .modal(true)
        .build();
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(&t!("queue.wizard.title").to_string()));
    filter.add_suffix("xlsx");
    filter.add_suffix("xls");
    filter.add_suffix("ods");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    dialog.set_filters(Some(&filters));
    dialog.set_default_filter(Some(&filter));

    let controller = controller.clone();
    let overlay = overlay.clone();
    let parent_for_wizard = parent.clone();
    dialog.open(parent.as_ref(), gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else {
            return;
        };
        let Some(path) = file.path() else {
            return;
        };
        build_wizard(
            controller.clone(),
            overlay.clone(),
            parent_for_wizard.clone(),
            path,
        );
    });
}

#[derive(Clone)]
struct WizardState {
    path: PathBuf,
    preview: Option<SheetPreview>,
    sheet_index: usize,
    header_row: u32,
    mapping: ColumnMapping,
    error: Option<String>,
    request: u64,
}

impl WizardState {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            preview: None,
            sheet_index: 0,
            header_row: 1,
            mapping: ColumnMapping::default(),
            error: None,
            request: 0,
        }
    }

    fn apply_preview(&mut self, path: PathBuf, preview: SheetPreview) {
        self.path = path;
        self.sheet_index = preview
            .sheets
            .iter()
            .position(|sheet| *sheet == preview.sheet)
            .unwrap_or(0);
        self.header_row = preview.header_row.max(1);
        self.mapping = spreadsheet::guess_mapping(&preview.headers);
        self.error = None;
        self.preview = Some(preview);
    }
}

fn build_wizard(
    controller: Rc<Controller>,
    overlay: adw::ToastOverlay,
    parent: Option<gtk::Window>,
    path: PathBuf,
) {
    let window = adw::Window::builder()
        .modal(true)
        .default_width(760)
        .default_height(680)
        .title(t!("queue.wizard.title").to_string())
        .build();
    if let Some(parent) = parent.as_ref() {
        window.set_transient_for(Some(parent));
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(12);
    body.set_margin_bottom(18);
    body.set_margin_start(18);
    body.set_margin_end(18);

    let holder = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.append(&holder);

    let scrolled = gtk::ScrolledWindow::builder()
        .child(&body)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    toolbar.set_content(Some(&scrolled));
    window.set_content(Some(&toolbar));
    window.present();

    let state = Rc::new(RefCell::new(WizardState::new(path.clone())));
    request_preview(&window, &holder, &state, &controller, &overlay, path, None);
}

/// Read the workbook preview on a worker thread, then fill the wizard form.
fn request_preview(
    window: &adw::Window,
    holder: &gtk::Box,
    state: &Rc<RefCell<WizardState>>,
    controller: &Rc<Controller>,
    overlay: &adw::ToastOverlay,
    path: PathBuf,
    sheet: Option<String>,
) {
    while let Some(child) = holder.first_child() {
        holder.remove(&child);
    }
    let spinner = gtk::Spinner::new();
    spinner.set_margin_top(24);
    spinner.set_margin_bottom(24);
    spinner.start();
    holder.append(&spinner);
    holder.append(&widgets::dim_label(&t!("queue.wizard.reading").to_string()));

    let request = {
        let mut state = state.borrow_mut();
        state.request = state.request.wrapping_add(1);
        state.request
    };

    let (tx, rx) = mpsc::channel::<Result<SheetPreview, String>>();
    let thread_path = path.clone();
    let thread_sheet = sheet.clone();
    let spawned = std::thread::Builder::new()
        .name("signifiage-queue-preview".to_string())
        .spawn(move || {
            let result = spreadsheet::preview(&thread_path, thread_sheet.as_deref())
                .map_err(|err| err.to_string());
            let _ = tx.send(result);
        });
    if let Err(err) = spawned {
        widget_error(
            holder,
            &t!("queue.wizard.read_failed", message = err.to_string()).to_string(),
        );
        return;
    }

    let window = window.clone();
    let holder = holder.clone();
    let state = state.clone();
    let controller = controller.clone();
    let overlay = overlay.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        if state.borrow().request != request {
            return glib::ControlFlow::Break;
        }
        match rx.try_recv() {
            Ok(Ok(preview)) => {
                state.borrow_mut().apply_preview(path.clone(), preview);
                populate_wizard(&window, &holder, &state, &controller, &overlay);
                glib::ControlFlow::Break
            }
            Ok(Err(err)) => {
                {
                    let mut state = state.borrow_mut();
                    state.error = Some(t!("queue.wizard.read_failed", message = err).to_string());
                    if let Some(preview) = state.preview.clone() {
                        state.sheet_index = preview
                            .sheets
                            .iter()
                            .position(|sheet| *sheet == preview.sheet)
                            .unwrap_or(0);
                    }
                }
                populate_wizard(&window, &holder, &state, &controller, &overlay);
                glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(TryRecvError::Disconnected) => {
                widget_error(
                    &holder,
                    &t!("queue.wizard.read_failed", message = "…").to_string(),
                );
                glib::ControlFlow::Break
            }
        }
    });
}

fn populate_wizard(
    window: &adw::Window,
    holder: &gtk::Box,
    state: &Rc<RefCell<WizardState>>,
    controller: &Rc<Controller>,
    overlay: &adw::ToastOverlay,
) {
    while let Some(child) = holder.first_child() {
        holder.remove(&child);
    }
    let snapshot = state.borrow().clone();

    // File row: the chosen workbook, replaceable.
    let file_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let choose = widgets::button(&t!("queue.wizard.choose_file").to_string());
    {
        let window = window.clone();
        let holder = holder.clone();
        let state = state.clone();
        let controller = controller.clone();
        let overlay = overlay.clone();
        choose.connect_clicked(move |_| {
            choose_spreadsheet_file(&window, {
                let window = window.clone();
                let holder = holder.clone();
                let state = state.clone();
                let controller = controller.clone();
                let overlay = overlay.clone();
                move |path: PathBuf| {
                    request_preview(&window, &holder, &state, &controller, &overlay, path, None);
                }
            });
        });
    }
    let file_label = gtk::Label::new(Some(&snapshot.path.display().to_string()));
    file_label.set_xalign(0.0);
    file_label.set_hexpand(true);
    file_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    file_label.add_css_class("dim-label");
    file_row.append(&choose);
    file_row.append(&file_label);
    holder.append(&widgets::field(
        &t!("queue.wizard.file").to_string(),
        &file_row,
    ));

    let Some(preview) = snapshot.preview.clone() else {
        holder.append(&widgets::dim_label(&t!("queue.wizard.no_file").to_string()));
        if let Some(error) = &snapshot.error {
            widget_error(holder, error);
        }
        return;
    };

    // Sheet selector.
    let sheet_index = snapshot
        .sheet_index
        .min(preview.sheets.len().saturating_sub(1));
    let sheet_dropdown = widgets::dropdown(&preview.sheets, sheet_index);
    {
        let window = window.clone();
        let holder = holder.clone();
        let state = state.clone();
        let controller = controller.clone();
        let overlay = overlay.clone();
        sheet_dropdown.connect_selected_notify(move |dropdown| {
            let index = dropdown.selected() as usize;
            let sheet = {
                let mut state = state.borrow_mut();
                let Some(preview) = state.preview.clone() else {
                    return;
                };
                if state.sheet_index == index || index >= preview.sheets.len() {
                    return;
                }
                state.sheet_index = index;
                preview.sheets[index].clone()
            };
            let path = state.borrow().path.clone();
            request_preview(
                &window,
                &holder,
                &state,
                &controller,
                &overlay,
                path,
                Some(sheet),
            );
        });
    }
    holder.append(&widgets::field(
        &t!("queue.wizard.sheet").to_string(),
        &sheet_dropdown,
    ));

    // Header row.
    let header_spin = widgets::int_spin(1, 100_000, snapshot.header_row.max(1) as i32);
    {
        let state = state.clone();
        header_spin.connect_value_changed(move |spin| {
            state.borrow_mut().header_row = spin.value().max(1.0) as u32;
        });
    }
    holder.append(&widgets::field(
        &t!("queue.wizard.header_row").to_string(),
        &header_spin,
    ));

    // Column mapping.
    let (mapping_section, mapping_content) =
        widgets::section(&t!("queue.wizard.mapping").to_string());
    let column_labels: Vec<String> = preview
        .headers
        .iter()
        .enumerate()
        .map(|(index, header)| column_label(index, header))
        .collect();
    let name_dropdown = widgets::dropdown(
        &column_labels,
        (snapshot.mapping.name_col as usize).min(preview.headers.len().saturating_sub(1)),
    );
    {
        let state = state.clone();
        name_dropdown.connect_selected_notify(move |dropdown| {
            state.borrow_mut().mapping.name_col = dropdown.selected();
        });
    }
    mapping_content.append(&widgets::field(
        &t!("queue.wizard.name_column").to_string(),
        &name_dropdown,
    ));

    let mut optional_labels = vec![t!("queue.wizard.not_mapped").to_string()];
    optional_labels.extend(column_labels.iter().cloned());
    let (ticket_dropdown, note_dropdown, status_dropdown) = (
        widgets::dropdown(
            &optional_labels,
            snapshot
                .mapping
                .ticket_col
                .map_or(0, |col| col as usize + 1),
        ),
        widgets::dropdown(
            &optional_labels,
            snapshot.mapping.note_col.map_or(0, |col| col as usize + 1),
        ),
        widgets::dropdown(
            &optional_labels,
            snapshot
                .mapping
                .status_col
                .map_or(0, |col| col as usize + 1),
        ),
    );
    {
        let state = state.clone();
        ticket_dropdown.connect_selected_notify(move |dropdown| {
            state.borrow_mut().mapping.ticket_col = optional_column(dropdown.selected());
        });
    }
    {
        let state = state.clone();
        note_dropdown.connect_selected_notify(move |dropdown| {
            state.borrow_mut().mapping.note_col = optional_column(dropdown.selected());
        });
    }
    {
        let state = state.clone();
        status_dropdown.connect_selected_notify(move |dropdown| {
            state.borrow_mut().mapping.status_col = optional_column(dropdown.selected());
        });
    }
    mapping_content.append(&widgets::field(
        &t!("queue.wizard.ticket_column").to_string(),
        &ticket_dropdown,
    ));
    mapping_content.append(&widgets::field(
        &t!("queue.wizard.note_column").to_string(),
        &note_dropdown,
    ));
    mapping_content.append(&widgets::field(
        &t!("queue.wizard.status_column").to_string(),
        &status_dropdown,
    ));
    holder.append(&mapping_section);

    // Preview grid.
    let (preview_section, preview_content) =
        widgets::section(&t!("queue.wizard.preview").to_string());
    let grid = gtk::Grid::new();
    grid.set_row_spacing(4);
    grid.set_column_spacing(12);
    for (column, header) in preview.headers.iter().enumerate() {
        let text = if header.trim().is_empty() {
            column_letter(column)
        } else {
            header.clone()
        };
        let label = gtk::Label::new(Some(&text));
        label.set_xalign(0.0);
        label.add_css_class("heading");
        grid.attach(&label, column as i32, 0, 1, 1);
    }
    for (row_index, row) in preview.rows.iter().take(8).enumerate() {
        for column in 0..preview.headers.len() {
            let text = row.get(column).cloned().unwrap_or_default();
            let label = gtk::Label::new(Some(&text));
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            grid.attach(&label, column as i32, row_index as i32 + 1, 1, 1);
        }
    }
    let grid_scroll = gtk::ScrolledWindow::builder()
        .child(&grid)
        .min_content_height(160)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    preview_content.append(&grid_scroll);
    preview_content.append(&widgets::dim_label(
        &t!("queue.wizard.rename_note").to_string(),
    ));
    holder.append(&preview_section);

    let error = gtk::Label::new(None);
    error.set_xalign(0.0);
    error.set_wrap(true);
    error.add_css_class("error");
    if let Some(message) = &snapshot.error {
        error.set_text(message);
    }
    holder.append(&error);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = widgets::button(&t!("common.cancel").to_string());
    let import = widgets::suggested(&t!("queue.wizard.import").to_string());
    buttons.append(&cancel);
    buttons.append(&import);
    holder.append(&buttons);
    {
        let window = window.clone();
        cancel.connect_clicked(move |_| window.close());
    }

    // Import: read every row on a worker thread, then merge on the main thread.
    {
        let window = window.clone();
        let state = state.clone();
        let controller = controller.clone();
        let overlay = overlay.clone();
        let import_button = import.clone();
        let error_label = error.clone();
        import.connect_clicked(move |_| {
            let snapshot = state.borrow().clone();
            let Some(preview) = snapshot.preview.clone() else {
                error_label.set_text(&t!("queue.wizard.no_file").to_string());
                return;
            };
            if snapshot.mapping.name_col as usize >= preview.headers.len() {
                error_label.set_text(&t!("queue.wizard.no_name_column").to_string());
                return;
            }
            let source = QueueSource {
                path: snapshot.path.display().to_string(),
                sheet: preview.sheet.clone(),
                header_row: snapshot.header_row.max(1),
                name_col: snapshot.mapping.name_col,
                ticket_col: snapshot.mapping.ticket_col,
                note_col: snapshot.mapping.note_col,
                status_col: snapshot.mapping.status_col,
            };
            let path = snapshot.path.clone();
            let sheet = preview.sheet.clone();
            let header_row = snapshot.header_row.max(1);
            let mapping = snapshot.mapping;
            import_button.set_sensitive(false);
            error_label.set_text("");

            let (tx, rx) = mpsc::channel::<Result<Vec<ImportRow>, String>>();
            let spawned = std::thread::Builder::new()
                .name("signifiage-queue-import".to_string())
                .spawn(move || {
                    let result = spreadsheet::read_import_rows(&path, &sheet, header_row, &mapping)
                        .map_err(|err| err.to_string());
                    let _ = tx.send(result);
                });
            if let Err(err) = spawned {
                import_button.set_sensitive(true);
                error_label.set_text(
                    &t!("queue.wizard.read_failed", message = err.to_string()).to_string(),
                );
                return;
            }

            let window = window.clone();
            let controller = controller.clone();
            let overlay = overlay.clone();
            let import_button = import_button.clone();
            let error_label = error_label.clone();
            glib::timeout_add_local(Duration::from_millis(100), move || match rx.try_recv() {
                Ok(Ok(rows)) => {
                    apply_import(&controller, &overlay, source.clone(), &rows);
                    window.close();
                    glib::ControlFlow::Break
                }
                Ok(Err(err)) => {
                    error_label
                        .set_text(&t!("queue.wizard.read_failed", message = err).to_string());
                    import_button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
                Err(TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(TryRecvError::Disconnected) => {
                    error_label
                        .set_text(&t!("queue.wizard.read_failed", message = "…").to_string());
                    import_button.set_sensitive(true);
                    glib::ControlFlow::Break
                }
            });
        });
    }
}

/// Persist the source, merge rows and create the queue scene on first import.
fn apply_import(
    controller: &Rc<Controller>,
    overlay: &adw::ToastOverlay,
    source: QueueSource,
    rows: &[ImportRow],
) -> ImportReport {
    let had_queue_scene = controller.project_clone().has_queue_scene();
    controller.set_queue_source(Some(source));
    let report = controller.queue_merge_import(rows);
    let created = if had_queue_scene {
        false
    } else {
        ensure_queue_scene(controller)
    };
    if created {
        widgets::toast(overlay, &t!("queue.toast.scene_created").to_string());
    } else if report.changed {
        widgets::toast(
            overlay,
            &t!(
                "queue.toast.imported",
                added = report.added,
                updated = report.updated
            )
            .to_string(),
        );
    } else {
        widgets::toast(overlay, &t!("queue.toast.nothing_new").to_string());
    }
    report
}

/// Add a full-bleed queue scene to the default playlist when the project has
/// none yet. Returns true when a scene was created.
fn ensure_queue_scene(controller: &Controller) -> bool {
    controller.mutate(Some("queue-scene"), |project| {
        if project.has_queue_scene() {
            return false;
        }
        let scene = project.queue_scene();
        let scene_id = scene.id.clone();
        project.scenes.push(scene);
        let playlist_id = if project.playlist(&project.default_playlist_id).is_some() {
            project.default_playlist_id.clone()
        } else {
            project
                .playlists
                .first()
                .map(|playlist| playlist.id.clone())
                .unwrap_or_default()
        };
        if playlist_id.is_empty() {
            let mut playlist = Playlist::new(t!("template.playlist.queue").to_string());
            playlist.r#loop = true;
            playlist.items.push(PlaylistItem::new(scene_id, 0));
            let id = playlist.id.clone();
            project.playlists.push(playlist);
            if project.default_playlist_id.is_empty() {
                project.default_playlist_id = id;
            }
        } else if let Some(playlist) = project.playlist_mut(&playlist_id) {
            playlist.items.push(PlaylistItem::new(scene_id, 0));
        }
        true
    })
}

fn choose_spreadsheet_file(window: &adw::Window, on_file: impl Fn(PathBuf) + 'static) {
    let dialog = gtk::FileDialog::builder()
        .title(t!("queue.wizard.title").to_string())
        .modal(true)
        .build();
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(&t!("queue.wizard.title").to_string()));
    filter.add_suffix("xlsx");
    filter.add_suffix("xls");
    filter.add_suffix("ods");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    dialog.set_filters(Some(&filters));
    dialog.set_default_filter(Some(&filter));
    dialog.open(
        Some(window.upcast_ref::<gtk::Window>()),
        gio::Cancellable::NONE,
        move |result| {
            let Ok(file) = result else {
                return;
            };
            if let Some(path) = file.path() {
                on_file(path);
            }
        },
    );
}

fn widget_error(holder: &gtk::Box, message: &str) {
    let label = gtk::Label::new(Some(message));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.add_css_class("error");
    holder.append(&label);
}

fn optional_column(selected: u32) -> Option<u32> {
    if selected == 0 {
        None
    } else {
        Some(selected - 1)
    }
}

fn column_letter(mut index: usize) -> String {
    let mut out = String::new();
    loop {
        out.insert(0, (b'A' + (index % 26) as u8) as char);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    out
}

fn column_label(index: usize, header: &str) -> String {
    let header = header.trim();
    let letter = column_letter(index);
    if header.is_empty() {
        format!("{} — {}", letter, t!("common.none"))
    } else {
        format!("{letter} — {header}")
    }
}

fn source_signature(source: &QueueSource) -> String {
    format!(
        "{}|{}|{}|{}|{:?}|{:?}|{:?}",
        source.path,
        source.sheet,
        source.header_row,
        source.name_col,
        source.ticket_col,
        source.note_col,
        source.status_col
    )
}

fn clock_now() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// Top-level window owning a widget, used as a dialog parent.
fn window_of(widget: &impl IsA<gtk::Widget>) -> Option<gtk::Window> {
    widget
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok())
}
