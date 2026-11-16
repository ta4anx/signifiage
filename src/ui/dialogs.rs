//! Modal dialogs: new project, timers, schedules, publish and about.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use rust_i18n::t;

use crate::model::Schedule;
use crate::model::project::Template;
use crate::model::schedule::{format_hhmm, parse_hhmm};
use crate::services::controller::Controller;
use crate::ui::widgets;

/// New-project dialog with template cards and a parent folder chooser.
pub fn new_project(parent: &adw::ApplicationWindow, controller: &Rc<Controller>) {
    let window = adw::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(t!("new_project.title").to_string())
        .default_width(520)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(12);
    body.set_margin_bottom(18);
    body.set_margin_start(18);
    body.set_margin_end(18);

    let name_entry = widgets::entry(&t!("new_project.default_name").to_string());
    body.append(&widgets::field(
        &t!("new_project.name").to_string(),
        &name_entry,
    ));

    let template_labels: Vec<String> = Template::ALL
        .iter()
        .map(|template| {
            format!(
                "{} — {}",
                t!(template.label_key()),
                t!(template.description_key())
            )
        })
        .collect();
    let template_dropdown = widgets::dropdown(&template_labels, 0);
    body.append(&widgets::field(
        &t!("new_project.template").to_string(),
        &template_dropdown,
    ));

    let folder_label = widgets::dim_label(&t!("new_project.no_folder").to_string());
    let folder: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let choose = widgets::button(&t!("new_project.choose_folder").to_string());
    {
        let folder = folder.clone();
        let folder_label = folder_label.clone();
        let window = window.clone();
        choose.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::builder()
                .title(t!("new_project.choose_folder").to_string())
                .modal(true)
                .build();
            let folder = folder.clone();
            let folder_label = folder_label.clone();
            dialog.select_folder(Some(&window), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result
                    && let Some(path) = file.path()
                {
                    folder_label.set_text(&path.display().to_string());
                    *folder.borrow_mut() = Some(path);
                }
            });
        });
    }
    let folder_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    folder_row.append(&choose);
    folder_row.append(&folder_label);
    body.append(&widgets::field(
        &t!("new_project.parent_folder").to_string(),
        &folder_row,
    ));

    let error = widgets::dim_label("");
    body.append(&error);

    let create = widgets::suggested(&t!("new_project.create").to_string());
    let cancel = widgets::button(&t!("common.cancel").to_string());
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&cancel);
    buttons.append(&create);
    body.append(&buttons);

    toolbar.set_content(Some(&body));
    window.set_content(Some(&toolbar));

    {
        let window = window.clone();
        cancel.connect_clicked(move |_| window.close());
    }
    {
        let window = window.clone();
        let controller = controller.clone();
        let folder = folder.clone();
        let error = error.clone();
        let name_entry = name_entry.clone();
        let template_dropdown = template_dropdown.clone();
        create.connect_clicked(move |_| {
            let Some(parent) = folder.borrow().clone() else {
                error.set_text(&t!("new_project.folder_required").to_string());
                return;
            };
            let name = name_entry.text().to_string();
            let name = if name.trim().is_empty() {
                t!("new_project.default_name").to_string()
            } else {
                name
            };
            let template = Template::ALL
                .get(template_dropdown.selected() as usize)
                .copied()
                .unwrap_or(Template::Blank);
            let folder_name = sanitize_folder_name(&name);
            let target = parent.join(folder_name);
            match controller.create_project(template, &name, &target) {
                Ok(()) => window.close(),
                Err(err) => error.set_text(&err.to_string()),
            }
        });
    }

    window.present();
}

fn sanitize_folder_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "signifiage-project".to_string()
    } else {
        trimmed
    }
}

/// Timer definitions editor.
pub fn timers_dialog(
    controller: &Rc<Controller>,
    overlay: &adw::ToastOverlay,
    parent: Option<&gtk::Window>,
) {
    let window = adw::Window::builder()
        .title(t!("timers.title").to_string())
        .modal(true)
        .default_width(480)
        .build();
    if let Some(parent) = parent {
        window.set_transient_for(Some(parent));
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());

    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_top(12);
    body.set_margin_bottom(18);
    body.set_margin_start(18);
    body.set_margin_end(18);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 6);
    rebuild_timers(&list, controller, overlay);
    body.append(&list);

    let add = widgets::button(&t!("timers.add").to_string());
    {
        let controller = controller.clone();
        let overlay = overlay.clone();
        let list = list.clone();
        add.connect_clicked(move |_| {
            controller.add_timer(&t!("timers.new_name").to_string(), None);
            rebuild_timers(&list, &controller, &overlay);
        });
    }
    body.append(&add);
    toolbar.set_content(Some(&body));
    window.set_content(Some(&toolbar));
    window.present();
}

fn rebuild_timers(list: &gtk::Box, controller: &Rc<Controller>, overlay: &adw::ToastOverlay) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let project = controller.project_clone();
    for timer in &project.timers {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let name = widgets::entry(&timer.name);
        name.set_hexpand(true);
        let duration = widgets::entry(
            &timer
                .duration_secs
                .map(|secs| secs.to_string())
                .unwrap_or_default(),
        );
        duration.set_placeholder_text(Some(&t!("timers.duration_placeholder").to_string()));
        duration.set_width_chars(8);
        let remove = widgets::icon_button("user-trash-symbolic", &t!("timers.remove").to_string());

        {
            let controller = controller.clone();
            let id = timer.id.clone();
            let duration = duration.clone();
            name.connect_changed(move |entry| {
                let duration_value = duration.text().parse::<u32>().ok();
                controller.update_timer(&id, &entry.text(), duration_value);
            });
        }
        {
            let controller = controller.clone();
            let id = timer.id.clone();
            let name_field = name.clone();
            duration.connect_changed(move |entry| {
                let duration_value = entry.text().trim().parse::<u32>().ok();
                controller.update_timer(&id, &name_field.text(), duration_value);
            });
        }
        {
            let controller = controller.clone();
            let overlay = overlay.clone();
            let id = timer.id.clone();
            let list = list.clone();
            remove.connect_clicked(move |_| {
                controller.remove_timer(&id);
                widgets::toast(&overlay, &t!("timers.removed").to_string());
                rebuild_timers(&list, &controller, &overlay);
            });
        }

        row.append(&name);
        row.append(&duration);
        row.append(&remove);
        list.append(&row);
    }
    let hint = widgets::dim_label(&t!("timers.duration_hint").to_string());
    list.append(&hint);
}

/// Single-line text prompt.
pub fn prompt_text(
    parent: Option<&gtk::Window>,
    title: &str,
    initial: &str,
    on_ok: impl Fn(String) + 'static,
) {
    let window = adw::Window::builder()
        .title(title.to_string())
        .modal(true)
        .default_width(420)
        .build();
    if let Some(parent) = parent {
        window.set_transient_for(Some(parent));
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(12);
    body.set_margin_bottom(18);
    body.set_margin_start(18);
    body.set_margin_end(18);
    let entry = widgets::entry(initial);
    body.append(&entry);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = widgets::button(&t!("common.cancel").to_string());
    let ok = widgets::suggested(&t!("common.save").to_string());
    buttons.append(&cancel);
    buttons.append(&ok);
    body.append(&buttons);
    toolbar.set_content(Some(&body));
    window.set_content(Some(&toolbar));

    {
        let window = window.clone();
        cancel.connect_clicked(move |_| window.close());
    }
    let on_ok = std::rc::Rc::new(on_ok);
    {
        let window = window.clone();
        let entry = entry.clone();
        let on_ok = on_ok.clone();
        ok.connect_clicked(move |_| {
            on_ok(entry.text().to_string());
            window.close();
        });
    }
    {
        let window = window.clone();
        let entry_weak = entry.downgrade();
        let on_ok = on_ok.clone();
        entry.connect_activate(move |_| {
            if let Some(entry) = entry_weak.upgrade() {
                on_ok(entry.text().to_string());
                window.close();
            }
        });
    }
    window.present();
}

#[derive(Clone)]
struct ScheduleDraft {
    playlist_id: RefCell<String>,
    days: RefCell<Vec<u8>>,
    start: RefCell<String>,
    end: RefCell<String>,
    error: gtk::Label,
}

/// Weekday/time schedule windows, saved into the project.
pub fn schedules_dialog(
    controller: &Rc<Controller>,
    overlay: &adw::ToastOverlay,
    parent: Option<&gtk::Window>,
) {
    let window = adw::Window::builder()
        .title(t!("schedule.title").to_string())
        .modal(true)
        .default_width(640)
        .build();
    if let Some(parent) = parent {
        window.set_transient_for(Some(parent));
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());

    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_top(12);
    body.set_margin_bottom(18);
    body.set_margin_start(18);
    body.set_margin_end(18);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let drafts: Rc<RefCell<Vec<Rc<ScheduleDraft>>>> = Rc::new(RefCell::new(Vec::new()));

    let playlists = controller.playlists();
    let playlist_names: Vec<String> = playlists.iter().map(|(_, name)| name.clone()).collect();

    let project = controller.project_clone();
    for schedule in &project.schedules {
        let selected = playlists
            .iter()
            .position(|(id, _)| id == &schedule.playlist_id)
            .unwrap_or(0);
        let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let dropdown = widgets::dropdown(&playlist_names, selected);
        let start = widgets::entry(&schedule.start);
        start.set_width_chars(6);
        let end = widgets::entry(&schedule.end);
        end.set_width_chars(6);
        let remove =
            widgets::icon_button("user-trash-symbolic", &t!("schedule.remove").to_string());
        top.append(&dropdown);
        top.append(&gtk::Label::new(Some(&t!("schedule.from").to_string())));
        top.append(&start);
        top.append(&gtk::Label::new(Some(&t!("schedule.to").to_string())));
        top.append(&end);
        top.append(&remove);

        let days_row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        let mut day_buttons = Vec::new();
        for (index, key) in Schedule::weekday_names().iter().enumerate() {
            let toggle = gtk::ToggleButton::with_label(&t!(*key).to_string());
            toggle.add_css_class("flat");
            toggle.set_active(schedule.days.contains(&(index as u8)));
            days_row.append(&toggle);
            day_buttons.push(toggle);
        }

        let error = widgets::dim_label("");
        error.add_css_class("error");

        row.append(&top);
        row.append(&days_row);
        row.append(&error);

        let draft = Rc::new(ScheduleDraft {
            playlist_id: RefCell::new(schedule.playlist_id.clone()),
            days: RefCell::new(schedule.days.clone()),
            start: RefCell::new(schedule.start.clone()),
            end: RefCell::new(schedule.end.clone()),
            error: error.clone(),
        });
        drafts.borrow_mut().push(draft.clone());

        {
            let draft = draft.clone();
            let playlists = playlists.clone();
            dropdown.connect_selected_notify(move |widget| {
                if let Some((id, _)) = playlists.get(widget.selected() as usize) {
                    *draft.playlist_id.borrow_mut() = id.clone();
                }
            });
        }
        for (index, toggle) in day_buttons.iter().enumerate() {
            let draft = draft.clone();
            toggle.connect_toggled(move |button| {
                let mut days = draft.days.borrow_mut();
                days.retain(|day| *day != index as u8);
                if button.is_active() {
                    days.push(index as u8);
                    days.sort_unstable();
                }
            });
        }
        {
            let draft = draft.clone();
            start.connect_changed(move |entry| {
                *draft.start.borrow_mut() = entry.text().to_string();
            });
        }
        {
            let draft = draft.clone();
            end.connect_changed(move |entry| {
                *draft.end.borrow_mut() = entry.text().to_string();
            });
        }
        {
            let drafts = drafts.clone();
            let draft = draft.clone();
            let row = row.clone();
            let list = list.clone();
            remove.connect_clicked(move |_| {
                drafts.borrow_mut().retain(|item| !Rc::ptr_eq(item, &draft));
                list.remove(&row);
            });
        }
        list.append(&row);
    }

    let add = widgets::button(&t!("schedule.add").to_string());
    {
        let drafts = drafts.clone();
        let list = list.clone();
        let playlists = playlists.clone();
        let playlist_names = playlist_names.clone();
        add.connect_clicked(move |_| {
            let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
            let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let dropdown = widgets::dropdown(&playlist_names, 0);
            let start = widgets::entry("09:00");
            start.set_width_chars(6);
            let end = widgets::entry("17:00");
            end.set_width_chars(6);
            let remove =
                widgets::icon_button("user-trash-symbolic", &t!("schedule.remove").to_string());
            top.append(&dropdown);
            top.append(&gtk::Label::new(Some(&t!("schedule.from").to_string())));
            top.append(&start);
            top.append(&gtk::Label::new(Some(&t!("schedule.to").to_string())));
            top.append(&end);
            top.append(&remove);

            let days_row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
            let mut day_buttons = Vec::new();
            for (index, key) in Schedule::weekday_names().iter().enumerate() {
                let toggle = gtk::ToggleButton::with_label(&t!(*key).to_string());
                toggle.add_css_class("flat");
                toggle.set_active(index < 5);
                days_row.append(&toggle);
                day_buttons.push(toggle);
            }
            let error = widgets::dim_label("");
            error.add_css_class("error");
            row.append(&top);
            row.append(&days_row);
            row.append(&error);

            let draft = Rc::new(ScheduleDraft {
                playlist_id: RefCell::new(
                    playlists
                        .first()
                        .map(|(id, _)| id.clone())
                        .unwrap_or_default(),
                ),
                days: RefCell::new(vec![0, 1, 2, 3, 4]),
                start: RefCell::new("09:00".to_string()),
                end: RefCell::new("17:00".to_string()),
                error: error.clone(),
            });
            drafts.borrow_mut().push(draft.clone());

            {
                let draft = draft.clone();
                let playlists = playlists.clone();
                dropdown.connect_selected_notify(move |widget| {
                    if let Some((id, _)) = playlists.get(widget.selected() as usize) {
                        *draft.playlist_id.borrow_mut() = id.clone();
                    }
                });
            }
            for (index, toggle) in day_buttons.iter().enumerate() {
                let draft = draft.clone();
                toggle.connect_toggled(move |button| {
                    let mut days = draft.days.borrow_mut();
                    days.retain(|day| *day != index as u8);
                    if button.is_active() {
                        days.push(index as u8);
                        days.sort_unstable();
                    }
                });
            }
            {
                let draft = draft.clone();
                start.connect_changed(move |entry| {
                    *draft.start.borrow_mut() = entry.text().to_string();
                });
            }
            {
                let draft = draft.clone();
                end.connect_changed(move |entry| {
                    *draft.end.borrow_mut() = entry.text().to_string();
                });
            }
            {
                let drafts = drafts.clone();
                let draft = draft.clone();
                let row = row.clone();
                let list = list.clone();
                remove.connect_clicked(move |_| {
                    drafts.borrow_mut().retain(|item| !Rc::ptr_eq(item, &draft));
                    list.remove(&row);
                });
            }
            list.append(&row);
        });
    }
    body.append(&list);
    body.append(&add);

    let save = widgets::suggested(&t!("common.save").to_string());
    let cancel = widgets::button(&t!("common.cancel").to_string());
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&cancel);
    buttons.append(&save);
    body.append(&buttons);

    {
        let window = window.clone();
        cancel.connect_clicked(move |_| window.close());
    }
    {
        let window = window.clone();
        let controller = controller.clone();
        let drafts = drafts.clone();
        let overlay = overlay.clone();
        save.connect_clicked(move |_| {
            let mut schedules = Vec::new();
            let mut valid = true;
            for draft in drafts.borrow().iter() {
                let start = draft.start.borrow().clone();
                let end = draft.end.borrow().clone();
                let days = draft.days.borrow().clone();
                let playlist_id = draft.playlist_id.borrow().clone();
                let start_minutes = parse_hhmm(&start);
                let end_minutes = parse_hhmm(&end);
                let schedule = Schedule {
                    playlist_id,
                    days,
                    start: start.clone(),
                    end: end.clone(),
                };
                match (start_minutes, end_minutes) {
                    (Some(start_m), Some(end_m)) if start_m < end_m => {
                        draft.error.set_text("");
                    }
                    _ => {
                        draft.error.set_text(&t!("schedule.invalid").to_string());
                        valid = false;
                    }
                }
                if schedule.days.is_empty() {
                    draft.error.set_text(&t!("schedule.no_days").to_string());
                    valid = false;
                }
                schedules.push(schedule);
            }
            if !valid {
                widgets::toast(&overlay, &t!("schedule.fix_errors").to_string());
                return;
            }
            controller.mutate(Some("schedules"), |project| {
                project.schedules = schedules;
            });
            window.close();
        });
    }

    let scrolled = gtk::ScrolledWindow::builder()
        .child(&body)
        .min_content_height(420)
        .build();
    toolbar.set_content(Some(&scrolled));
    window.set_content(Some(&toolbar));
    window.present();
}

/// Publish the project to a `.signifiage` package.
pub fn publish_dialog(
    parent: &adw::ApplicationWindow,
    controller: &Rc<Controller>,
    overlay: &adw::ToastOverlay,
) {
    let Some(dir) = controller.project_dir() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title(t!("publish.title").to_string())
        .modal(true)
        .initial_name(format!("{}.signifiage", controller.project_name()))
        .build();
    let controller = controller.clone();
    let overlay = overlay.clone();
    dialog.save(Some(parent), gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else {
            return;
        };
        let Some(path) = file.path() else {
            return;
        };
        if let Err(err) = controller.save_now() {
            widgets::toast(&overlay, &err.to_string());
            return;
        }
        match crate::model::project::export_package(&dir, &path) {
            Ok(path) => widgets::toast(
                &overlay,
                &t!("publish.done", path = path.display().to_string()).to_string(),
            ),
            Err(err) => widgets::toast(&overlay, &err.to_string()),
        }
    });
}

/// About window with versions.
pub fn about(parent: &adw::ApplicationWindow) {
    let about = adw::AboutDialog::builder()
        .application_name(t!("app.name").to_string())
        .application_icon(crate::APP_ID)
        .developer_name("Signifiage contributors")
        .version(crate::VERSION)
        .website("https://github.com/ta4anx/signifiage")
        .license_type(gtk::License::Gpl30)
        .comments(glib::markup_escape_text(&t!("app.subtitle").to_string()))
        .build();
    let gtk_version = format!(
        "GTK {}.{}.{}",
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version()
    );
    let adw_version = format!(
        "libadwaita {}.{}.{}",
        adw::major_version(),
        adw::minor_version(),
        adw::micro_version()
    );
    let gst_version = gstreamer::version_string();
    about.set_debug_info(&format!(
        "{gtk_version}\n{adw_version}\nGStreamer {gst_version}"
    ));
    about.present(Some(parent));
}

/// Display name for a stored HH:MM window (used by tests and the schedule list).
pub fn schedule_summary(schedule: &Schedule) -> String {
    let days: Vec<&str> = schedule
        .days
        .iter()
        .filter_map(|day| Schedule::weekday_names().get(*day as usize).copied())
        .collect();
    let start = schedule
        .start_minutes()
        .map(format_hhmm)
        .unwrap_or_else(|| schedule.start.clone());
    let end = schedule
        .end_minutes()
        .map(format_hhmm)
        .unwrap_or_else(|| schedule.end.clone());
    format!("{} {}–{}", days.join(","), start, end)
}

#[allow(dead_code)]
fn placeholder_for_schedules(_: &ScheduleDraft) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_summary_lists_days_and_times() {
        let schedule = Schedule {
            playlist_id: "p".into(),
            days: vec![0, 2],
            start: "09:00".into(),
            end: "17:30".into(),
        };
        let summary = schedule_summary(&schedule);
        assert!(summary.contains("09:00"));
        assert!(summary.contains("17:30"));
        assert_eq!(summary.matches(',').count(), 1);
    }

    #[test]
    fn folder_names_are_filesystem_safe() {
        assert_eq!(sanitize_folder_name("Stage 2026"), "Stage-2026");
        assert_eq!(sanitize_folder_name("???"), "signifiage-project");
        assert_eq!(sanitize_folder_name("a/b"), "a-b");
    }
}
