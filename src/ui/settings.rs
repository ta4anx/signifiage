//! Preferences dialog: displays, audio, project design, remote, capture,
//! appearance and about. Every control applies and persists immediately.

use std::cell::{Cell, RefCell};
use std::net::{IpAddr, UdpSocket};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use rust_i18n::t;

use crate::config::{self, CaptureBackend, Config, Language, MonitorRef, Theme};
use crate::model::project::{MAX_DESIGN, MIN_DESIGN, OutputScale};
use crate::services::controller::Controller;
use crate::services::media::MediaHub;
use crate::ui::outputs::{describe_monitors, monitor_list, resolve_monitor};
use crate::ui::{style, widgets};

pub struct SettingsWindow;

impl SettingsWindow {
    pub fn present(controller: Rc<Controller>, parent: &adw::ApplicationWindow) {
        let window = adw::PreferencesDialog::builder()
            .title(t!("page.settings").to_string())
            .build();
        window.add(&build_displays(&controller));
        window.add(&build_audio(&controller));
        window.add(&build_project(&controller));
        window.add(&build_remote(&controller));
        window.add(&build_queue(&controller));
        window.add(&build_experimental(&controller));
        window.add(&build_capture(&controller));
        window.add(&build_appearance(&controller));
        window.add(&build_about(&controller));
        window.present(Some(parent));
    }
}

// ------------------------------------------------------------------ helpers

fn page(title: &str, icon_name: &str) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title(title);
    // Without an icon the dialog's page switcher shows the "image missing" glyph.
    page.set_icon_name(Some(icon_name));
    page
}

fn group(title: &str) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title(title);
    group
}

/// An `ActionRow` with a trailing control.
fn row(title: &str, subtitle: Option<&str>, control: &impl IsA<gtk::Widget>) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    control.set_valign(gtk::Align::Center);
    row.add_suffix(control);
    row
}

/// Explanatory paragraph inside a preferences group.
fn note_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.add_css_class("dim-label");
    label.set_margin_top(6);
    label.set_margin_bottom(6);
    label.set_margin_start(12);
    label.set_margin_end(12);
    label
}

fn update_config(controller: &Rc<Controller>, f: impl FnOnce(&mut Config)) {
    let mut config = controller.config();
    f(&mut config);
    controller.set_config(config.clone());
    if let Err(err) = config.save() {
        controller.toast(&t!("settings.error.save", message = err.to_string()).to_string());
    }
}

// ---------------------------------------------------------------- displays

fn set_program_monitor(config: &mut Config, monitor: Option<MonitorRef>) {
    config.outputs.program.monitor = monitor;
}

fn set_preview_monitor(config: &mut Config, monitor: Option<MonitorRef>) {
    config.outputs.preview.monitor = monitor;
}

fn set_prompter_monitor(config: &mut Config, monitor: Option<MonitorRef>) {
    config.outputs.prompter.monitor = monitor;
}

fn set_operator_monitor(config: &mut Config, monitor: Option<MonitorRef>) {
    config.operator_monitor = monitor;
}

fn monitor_row(
    group: &adw::PreferencesGroup,
    controller: &Rc<Controller>,
    title: &str,
    current: Option<MonitorRef>,
    setter: fn(&mut Config, Option<MonitorRef>),
) {
    let monitors = monitor_list();
    let mut labels = vec![t!("settings.displays.unassigned").to_string()];
    let mut references: Vec<Option<MonitorRef>> = vec![None];
    for (_, label, reference) in describe_monitors(&monitors) {
        labels.push(label);
        references.push(Some(reference));
    }
    let selected = current
        .as_ref()
        .and_then(|reference| resolve_monitor(Some(reference), &monitors))
        .map(|index| index as u32 + 1)
        .unwrap_or(0);
    let dropdown = widgets::dropdown(&labels, selected as usize);
    let references = Rc::new(references);
    let controller = controller.clone();
    dropdown.connect_selected_notify(move |dropdown| {
        let value = references
            .get(dropdown.selected() as usize)
            .cloned()
            .flatten();
        update_config(&controller, |config| setter(config, value));
        controller.apply_output_config();
    });
    group.add(&row(title, None, &dropdown));
}

fn build_displays(controller: &Rc<Controller>) -> adw::PreferencesPage {
    let config = controller.config();
    let page = page(
        &t!("settings.section.displays").to_string(),
        "video-display-symbolic",
    );
    let group = group(&t!("settings.section.displays").to_string());

    monitor_row(
        &group,
        controller,
        &t!("settings.displays.program").to_string(),
        config.outputs.program.monitor.clone(),
        set_program_monitor,
    );
    monitor_row(
        &group,
        controller,
        &t!("settings.displays.preview").to_string(),
        config.outputs.preview.monitor.clone(),
        set_preview_monitor,
    );
    monitor_row(
        &group,
        controller,
        &t!("settings.displays.prompter").to_string(),
        config.outputs.prompter.monitor.clone(),
        set_prompter_monitor,
    );

    let windowed = gtk::Switch::new();
    windowed.set_active(config.windowed_outputs);
    {
        let controller = controller.clone();
        windowed.connect_active_notify(move |switch| {
            update_config(&controller, |config| {
                config.windowed_outputs = switch.is_active();
            });
            controller.apply_output_config();
        });
    }
    group.add(&row(
        &t!("settings.displays.windowed").to_string(),
        Some(&t!("settings.displays.windowed_hint").to_string()),
        &windowed,
    ));

    let mut operator_labels = vec![t!("settings.displays.operator_desktop").to_string()];
    let mut operator_references: Vec<Option<MonitorRef>> = vec![None];
    let monitors = monitor_list();
    for (_, label, reference) in describe_monitors(&monitors) {
        operator_labels.push(label);
        operator_references.push(Some(reference));
    }
    let operator_selected = config
        .operator_monitor
        .as_ref()
        .and_then(|reference| resolve_monitor(Some(reference), &monitors))
        .map(|index| index as u32 + 1)
        .unwrap_or(0);
    let operator = widgets::dropdown(&operator_labels, operator_selected as usize);
    let operator_references = Rc::new(operator_references);
    {
        let controller = controller.clone();
        operator.connect_selected_notify(move |dropdown| {
            let value = operator_references
                .get(dropdown.selected() as usize)
                .cloned()
                .flatten();
            update_config(&controller, |config| set_operator_monitor(config, value));
            controller.apply_output_config();
        });
    }
    group.add(&row(
        &t!("settings.displays.operator").to_string(),
        None,
        &operator,
    ));

    page.add(&group);
    page
}

// ------------------------------------------------------------------- audio

fn audio_items(devices: &[(String, String)]) -> Vec<String> {
    let mut items = vec![t!("settings.audio.default").to_string()];
    items.extend(devices.iter().map(|(_, display)| display.clone()));
    items
}

fn audio_selection(devices: &[(String, String)], sink: Option<&str>) -> u32 {
    sink.and_then(|sink| devices.iter().position(|(node, _)| node == sink))
        .map(|index| index as u32 + 1)
        .unwrap_or(0)
}

fn build_audio(controller: &Rc<Controller>) -> adw::PreferencesPage {
    let config = controller.config();
    let page = page(
        &t!("settings.section.audio").to_string(),
        "audio-volume-high-symbolic",
    );
    let group = group(&t!("settings.section.audio").to_string());

    let devices = Rc::new(RefCell::new(MediaHub::audio_devices()));
    let items = audio_items(&devices.borrow());
    let selected = audio_selection(&devices.borrow(), config.audio_sink.as_deref());
    let dropdown = widgets::dropdown(&items, selected as usize);
    let guard = Rc::new(Cell::new(false));
    {
        let devices = devices.clone();
        let guard = guard.clone();
        let controller = controller.clone();
        dropdown.connect_selected_notify(move |dropdown| {
            if guard.get() {
                return;
            }
            let index = dropdown.selected() as usize;
            let node = if index == 0 {
                None
            } else {
                devices
                    .borrow()
                    .get(index - 1)
                    .map(|(node, _)| node.clone())
            };
            update_config(&controller, |config| config.audio_sink = node);
        });
    }
    group.add(&row(
        &t!("settings.audio.sink").to_string(),
        None,
        &dropdown,
    ));

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let refresh = widgets::button(&t!("settings.audio.refresh").to_string());
    {
        let devices = devices.clone();
        let guard = guard.clone();
        let dropdown = dropdown.clone();
        let controller = controller.clone();
        refresh.connect_clicked(move |_| {
            let fresh = MediaHub::audio_devices();
            let empty = fresh.is_empty();
            *devices.borrow_mut() = fresh;
            let disappeared = controller
                .config()
                .audio_sink
                .as_deref()
                .is_some_and(|sink| audio_selection(&devices.borrow(), Some(sink)) == 0);
            if disappeared {
                update_config(&controller, |config| config.audio_sink = None);
            }
            let items = audio_items(&devices.borrow());
            let refs: Vec<&str> = items.iter().map(String::as_str).collect();
            guard.set(true);
            dropdown.set_model(Some(&gtk::StringList::new(&refs)));
            let sink = controller.config().audio_sink;
            dropdown.set_selected(audio_selection(&devices.borrow(), sink.as_deref()));
            guard.set(false);
            if disappeared {
                controller.toast(&t!("settings.audio.fallback").to_string());
            } else if empty {
                controller.toast(&t!("settings.audio.no_devices").to_string());
            }
        });
    }
    buttons.append(&refresh);

    let test = widgets::button(&t!("settings.audio.test_tone").to_string());
    {
        let controller = controller.clone();
        test.connect_clicked(move |_| {
            let device = controller.config().audio_sink;
            let (sender, receiver) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = sender.send(MediaHub::play_test_tone(device.as_deref()));
            });
            let controller = controller.clone();
            gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
                match receiver.try_recv() {
                    Ok(Ok(())) => {
                        controller.toast(&t!("settings.audio.test_ok").to_string());
                        gtk::glib::ControlFlow::Break
                    }
                    Ok(Err(message)) => {
                        controller.toast(
                            &t!("settings.audio.test_failed", message = message).to_string(),
                        );
                        gtk::glib::ControlFlow::Break
                    }
                    Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => gtk::glib::ControlFlow::Break,
                }
            });
        });
    }
    buttons.append(&test);
    group.add(&row(
        &t!("settings.audio.actions").to_string(),
        None,
        &buttons,
    ));

    group.add(&note_label(&t!("settings.audio.sink_hint").to_string()));

    page.add(&group);
    page
}

// ----------------------------------------------------------------- project

fn preset_index(width: u32, height: u32) -> usize {
    match (width, height) {
        (1920, 1080) => 0,
        (3840, 2160) => 1,
        (1080, 1920) => 2,
        _ => 3,
    }
}

fn preset_dims(index: usize) -> Option<(u32, u32)> {
    match index {
        0 => Some((1920, 1080)),
        1 => Some((3840, 2160)),
        2 => Some((1080, 1920)),
        _ => None,
    }
}

fn sync_preset(dropdown: &gtk::DropDown, guard: &Cell<bool>, width: u32, height: u32) {
    let index = preset_index(width, height) as u32;
    if dropdown.selected() != index {
        guard.set(true);
        dropdown.set_selected(index);
        guard.set(false);
    }
}

fn project_available(controller: &Rc<Controller>) -> bool {
    if controller.is_open() {
        true
    } else {
        controller.toast(&t!("settings.project.no_project").to_string());
        false
    }
}

fn build_project(controller: &Rc<Controller>) -> adw::PreferencesPage {
    let page = page(
        &t!("settings.section.project").to_string(),
        "document-edit-symbolic",
    );
    let group = group(&t!("settings.section.project").to_string());
    let open = controller.is_open();
    let project = controller.project_clone();
    let design = project.design.clone();
    let presentation = project.presentation.clone();

    let preset_labels = vec![
        format!("{} — 1920×1080", t!("settings.project.preset.fhd")),
        format!("{} — 3840×2160", t!("settings.project.preset.uhd")),
        format!("{} — 1080×1920", t!("settings.project.preset.portrait")),
        t!("settings.project.preset.custom").to_string(),
    ];
    let preset = widgets::dropdown(&preset_labels, preset_index(design.width, design.height));
    let width_spin = widgets::int_spin(
        MIN_DESIGN as i32,
        MAX_DESIGN as i32,
        design.width.min(MAX_DESIGN) as i32,
    );
    let height_spin = widgets::int_spin(
        MIN_DESIGN as i32,
        MAX_DESIGN as i32,
        design.height.min(MAX_DESIGN) as i32,
    );
    let guard = Rc::new(Cell::new(false));

    {
        let controller = controller.clone();
        let guard = guard.clone();
        let width_spin = width_spin.clone();
        let height_spin = height_spin.clone();
        preset.connect_selected_notify(move |dropdown| {
            if guard.get() {
                return;
            }
            let Some((width, height)) = preset_dims(dropdown.selected() as usize) else {
                return;
            };
            if !project_available(&controller) {
                return;
            }
            guard.set(true);
            width_spin.set_value(f64::from(width));
            height_spin.set_value(f64::from(height));
            guard.set(false);
            controller.mutate(Some("settings.design.size"), |project| {
                project.design.width = width;
                project.design.height = height;
                project.design.normalize();
            });
        });
    }

    {
        let controller = controller.clone();
        let guard = guard.clone();
        let height_spin = height_spin.clone();
        let preset = preset.clone();
        width_spin.connect_value_changed(move |spin| {
            if guard.get() {
                return;
            }
            if !project_available(&controller) {
                return;
            }
            let width = (spin.value().round() as u32).clamp(MIN_DESIGN, MAX_DESIGN);
            controller.mutate(Some("settings.design.size"), move |project| {
                project.design.width = width;
                project.design.normalize();
            });
            let height = (height_spin.value().round() as u32).clamp(MIN_DESIGN, MAX_DESIGN);
            sync_preset(&preset, &guard, width, height);
        });
    }

    {
        let controller = controller.clone();
        let guard = guard.clone();
        let width_spin = width_spin.clone();
        let preset = preset.clone();
        height_spin.connect_value_changed(move |spin| {
            if guard.get() {
                return;
            }
            if !project_available(&controller) {
                return;
            }
            let height = (spin.value().round() as u32).clamp(MIN_DESIGN, MAX_DESIGN);
            controller.mutate(Some("settings.design.size"), move |project| {
                project.design.height = height;
                project.design.normalize();
            });
            let width = (width_spin.value().round() as u32).clamp(MIN_DESIGN, MAX_DESIGN);
            sync_preset(&preset, &guard, width, height);
        });
    }

    group.add(&row(
        &t!("settings.project.preset_label").to_string(),
        None,
        &preset,
    ));
    group.add(&row(
        &t!("settings.project.width").to_string(),
        None,
        &width_spin,
    ));
    group.add(&row(
        &t!("settings.project.height").to_string(),
        None,
        &height_spin,
    ));

    let scales = [OutputScale::Fit, OutputScale::Fill, OutputScale::Stretch];
    let scale_labels = vec![
        t!("settings.project.scale.fit").to_string(),
        t!("settings.project.scale.fill").to_string(),
        t!("settings.project.scale.stretch").to_string(),
    ];
    let scale_selected = scales
        .iter()
        .position(|scale| *scale == presentation.scale)
        .unwrap_or(0);
    let scale = widgets::dropdown(&scale_labels, scale_selected);
    {
        let controller = controller.clone();
        scale.connect_selected_notify(move |dropdown| {
            if !project_available(&controller) {
                return;
            }
            let value = scales
                .get(dropdown.selected() as usize)
                .copied()
                .unwrap_or_default();
            controller.mutate(Some("settings.design.scale"), |project| {
                project.presentation.scale = value;
            });
        });
    }
    group.add(&row(
        &t!("settings.project.scale_label").to_string(),
        None,
        &scale,
    ));

    let rotations: [u16; 4] = [0, 90, 180, 270];
    let rotation_labels = vec![
        "0°".to_string(),
        "90°".to_string(),
        "180°".to_string(),
        "270°".to_string(),
    ];
    let rotation_selected = rotations
        .iter()
        .position(|rotation| *rotation == presentation.rotation)
        .unwrap_or(0);
    let rotation = widgets::dropdown(&rotation_labels, rotation_selected);
    {
        let controller = controller.clone();
        rotation.connect_selected_notify(move |dropdown| {
            if !project_available(&controller) {
                return;
            }
            let value = rotations
                .get(dropdown.selected() as usize)
                .copied()
                .unwrap_or(0);
            controller.mutate(Some("settings.design.rotation"), |project| {
                project.presentation.rotation = value;
            });
        });
    }
    group.add(&row(
        &t!("settings.project.rotation").to_string(),
        None,
        &rotation,
    ));

    let safe_area = gtk::Switch::new();
    safe_area.set_active(design.safe_area);
    {
        let controller = controller.clone();
        safe_area.connect_active_notify(move |switch| {
            if !project_available(&controller) {
                return;
            }
            let value = switch.is_active();
            controller.mutate(Some("settings.design.safe_area"), |project| {
                project.design.safe_area = value;
            });
        });
    }
    group.add(&row(
        &t!("settings.project.safe_area").to_string(),
        None,
        &safe_area,
    ));

    let grid = widgets::int_spin(0, 400, design.grid.min(400) as i32);
    {
        let controller = controller.clone();
        grid.connect_value_changed(move |spin| {
            if !project_available(&controller) {
                return;
            }
            let value = (spin.value().round() as u32).min(400);
            controller.mutate(Some("settings.design.grid"), |project| {
                project.design.grid = value;
            });
        });
    }
    group.add(&row(&t!("settings.project.grid").to_string(), None, &grid));

    for control in [
        preset.upcast_ref::<gtk::Widget>(),
        width_spin.upcast_ref::<gtk::Widget>(),
        height_spin.upcast_ref::<gtk::Widget>(),
        scale.upcast_ref::<gtk::Widget>(),
        rotation.upcast_ref::<gtk::Widget>(),
        safe_area.upcast_ref::<gtk::Widget>(),
        grid.upcast_ref::<gtk::Widget>(),
    ] {
        control.set_sensitive(open);
    }
    if !open {
        group.add(&note_label(&t!("settings.project.no_project").to_string()));
    }

    page.add(&group);
    page
}

// ------------------------------------------------------------------ remote

/// The machine's LAN IPv4 address, obtained without sending any packet.
pub fn lan_ipv4() -> Option<IpAddr> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("8.8.8.8", 80)).ok()?;
    socket.local_addr().ok().map(|address| address.ip())
}

fn remote_status(config: &Config) -> String {
    let port = config.remote.port.clamp(1024, 65535);
    let state = if config.remote.enabled {
        t!("settings.remote.state_on").to_string()
    } else {
        t!("settings.remote.state_off").to_string()
    };
    let local = format!("http://localhost:{port}");
    let lan = match lan_ipv4() {
        Some(address) => format!("http://{address}:{port}"),
        None => t!("settings.remote.no_lan").to_string(),
    };
    format!(
        "{}: {state}\n{}: {local}\n{}: {lan}",
        t!("settings.remote.state"),
        t!("settings.remote.local"),
        t!("settings.remote.lan"),
    )
}

fn build_remote(controller: &Rc<Controller>) -> adw::PreferencesPage {
    let config = controller.config();
    let page = page(
        &t!("settings.section.remote").to_string(),
        "network-wireless-symbolic",
    );
    let group = group(&t!("settings.section.remote").to_string());

    let enabled = gtk::Switch::new();
    enabled.set_active(config.remote.enabled);
    let token_entry = gtk::Entry::builder()
        .text(&config.remote.token)
        .editable(false)
        .width_chars(26)
        .build();
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    status.set_margin_top(6);
    status.set_margin_bottom(6);
    status.set_margin_start(12);
    status.set_margin_end(12);
    let refresh_status: Rc<dyn Fn()> = {
        let controller = controller.clone();
        let status = status.clone();
        Rc::new(move || {
            status.set_text(&remote_status(&controller.config()));
        })
    };
    refresh_status();

    {
        let controller = controller.clone();
        let token_entry = token_entry.clone();
        let refresh_status = refresh_status.clone();
        enabled.connect_active_notify(move |switch| {
            let active = switch.is_active();
            update_config(&controller, |config| {
                config.remote.enabled = active;
                if active && config.remote.token.trim().is_empty() {
                    config.remote.token = config::generate_token();
                }
            });
            token_entry.set_text(&controller.config().remote.token);
            refresh_status();
        });
    }
    group.add(&row(
        &t!("settings.remote.enabled").to_string(),
        None,
        &enabled,
    ));

    let port = widgets::int_spin(
        1024,
        65535,
        i32::from(config.remote.port.clamp(1024, 65535)),
    );
    {
        let controller = controller.clone();
        let refresh_status = refresh_status.clone();
        port.connect_value_changed(move |spin| {
            let value = (spin.value().round() as u16).clamp(1024, 65535);
            update_config(&controller, |config| config.remote.port = value);
            refresh_status();
        });
    }
    group.add(&row(&t!("settings.remote.port").to_string(), None, &port));

    let token_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    token_row.append(&token_entry);
    let copy = widgets::button(&t!("settings.remote.copy").to_string());
    {
        let controller = controller.clone();
        copy.connect_clicked(move |_| {
            let token = controller.config().remote.token;
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&token);
                controller.toast(&t!("settings.remote.copied").to_string());
            }
        });
    }
    token_row.append(&copy);
    group.add(&row(
        &t!("settings.remote.token").to_string(),
        None,
        &token_row,
    ));

    let regenerate = widgets::button(&t!("settings.remote.regenerate").to_string());
    {
        let controller = controller.clone();
        let token_entry = token_entry.clone();
        let refresh_status = refresh_status.clone();
        regenerate.connect_clicked(move |_| {
            update_config(&controller, |config| {
                config.remote.token = config::generate_token();
            });
            token_entry.set_text(&controller.config().remote.token);
            refresh_status();
        });
    }
    group.add(&row(
        &t!("settings.remote.token_actions").to_string(),
        None,
        &regenerate,
    ));

    group.add(&status);
    group.add(&note_label(&t!("settings.remote.warning").to_string()));

    page.add(&group);
    page
}

// ----------------------------------------------------------------- capture

// ------------------------------------------------------------------- queue

fn build_queue(controller: &Rc<Controller>) -> adw::PreferencesPage {
    let config = controller.config();
    let page = page(
        &t!("settings.section.queue").to_string(),
        "view-list-symbolic",
    );
    let group = group(&t!("settings.queue.call_sound").to_string());

    let enabled = widgets::switch(config.call_sound.enabled);
    {
        let controller = controller.clone();
        enabled.connect_state_set(move |_, state| {
            update_config(&controller, |config| config.call_sound.enabled = state);
            gtk::glib::Propagation::Proceed
        });
    }
    group.add(&row(
        &t!("settings.queue.sound_enabled").to_string(),
        Some(&t!("settings.queue.sound_hint").to_string()),
        &enabled,
    ));

    let sound_row = adw::ActionRow::builder()
        .title(t!("settings.queue.sound_file").to_string())
        .subtitle(match &config.call_sound.path {
            Some(path) => path.display().to_string(),
            None => t!("settings.queue.sound_default").to_string(),
        })
        .build();
    let choose = widgets::button(&t!("settings.queue.choose_file").to_string());
    let reset = widgets::button(&t!("settings.queue.use_default").to_string());
    let test = widgets::button(&t!("settings.queue.test").to_string());
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.append(&choose);
    buttons.append(&reset);
    buttons.append(&test);
    sound_row.add_suffix(&buttons);
    group.add(&sound_row);

    {
        let controller = controller.clone();
        let sound_row = sound_row.clone();
        let parent = None::<gtk::Window>;
        choose.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::builder()
                .title(t!("settings.queue.choose_file").to_string())
                .modal(true)
                .build();
            let filter = gtk::FileFilter::new();
            filter.set_name(Some(&t!("soundboard.audio_filter").to_string()));
            for suffix in crate::services::assets::AUDIO_EXTENSIONS {
                filter.add_suffix(suffix);
            }
            let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));
            let sound_row = sound_row.clone();
            let controller = controller.clone();
            dialog.open(
                parent.as_ref(),
                gtk::gio::Cancellable::NONE,
                move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        return;
                    };
                    sound_row.set_subtitle(&path.display().to_string());
                    update_config(&controller, |config| {
                        config.call_sound.path = Some(path.clone());
                        config.call_sound.enabled = true;
                    });
                },
            );
        });
    }
    {
        let controller = controller.clone();
        let sound_row = sound_row.clone();
        reset.connect_clicked(move |_| {
            sound_row.set_subtitle(&t!("settings.queue.sound_default").to_string());
            update_config(&controller, |config| config.call_sound.path = None);
        });
    }
    {
        let controller = controller.clone();
        test.connect_clicked(move |_| {
            let config = controller.config();
            match crate::services::chime::resolve(true, config.call_sound.path.as_deref()) {
                Some(path) => crate::services::chime::play_once(&path, config.audio_sink.clone()),
                None => controller.toast(&t!("settings.queue.sound_missing").to_string()),
            }
        });
    }

    let glow = widgets::spin(0.0, 15.0, 0.5, config.call_glow_secs);
    {
        let controller = controller.clone();
        glow.connect_value_changed(move |spin| {
            let value = spin.value();
            update_config(&controller, |config| config.call_glow_secs = value);
        });
    }
    group.add(&row(
        &t!("settings.queue.glow").to_string(),
        Some(&t!("settings.queue.glow_hint").to_string()),
        &glow,
    ));

    page.add(&group);
    page
}

// -------------------------------------------------------------- experimental

fn build_experimental(controller: &Rc<Controller>) -> adw::PreferencesPage {
    let config = controller.config();
    let page = page(
        &t!("settings.section.experimental").to_string(),
        "applications-science-symbolic",
    );
    let group = group(&t!("settings.experimental.title").to_string());
    group.add(&note_label(&t!("settings.experimental.hint").to_string()));

    let random = widgets::switch(config.experimental.random_pick);
    {
        let controller = controller.clone();
        random.connect_state_set(move |_, state| {
            update_config(&controller, |config| {
                config.experimental.random_pick = state
            });
            gtk::glib::Propagation::Proceed
        });
    }
    group.add(&row(
        &t!("settings.experimental.random_pick").to_string(),
        Some(&t!("settings.experimental.random_pick_hint").to_string()),
        &random,
    ));

    let write_back = widgets::switch(config.experimental.write_back);
    write_back.set_sensitive(config.experimental.random_pick);
    {
        let write_back = write_back.clone();
        random.connect_state_set(move |_, state| {
            write_back.set_sensitive(state);
            gtk::glib::Propagation::Proceed
        });
    }
    {
        let controller = controller.clone();
        write_back.connect_state_set(move |_, state| {
            update_config(&controller, |config| config.experimental.write_back = state);
            gtk::glib::Propagation::Proceed
        });
    }
    group.add(&row(
        &t!("settings.experimental.write_back").to_string(),
        Some(&t!("settings.experimental.write_back_hint").to_string()),
        &write_back,
    ));

    page.add(&group);
    page
}

fn build_capture(controller: &Rc<Controller>) -> adw::PreferencesPage {
    let config = controller.config();
    let page = page(
        &t!("settings.section.capture").to_string(),
        "camera-web-symbolic",
    );
    let group = group(&t!("settings.section.capture").to_string());

    let backends = [
        CaptureBackend::Auto,
        CaptureBackend::X11,
        CaptureBackend::Portal,
    ];
    let labels = vec![
        t!("settings.capture.auto").to_string(),
        t!("settings.capture.x11").to_string(),
        t!("settings.capture.portal").to_string(),
    ];
    let selected = backends
        .iter()
        .position(|backend| *backend == config.capture_backend)
        .unwrap_or(0);
    let dropdown = widgets::dropdown(&labels, selected);
    {
        let controller = controller.clone();
        dropdown.connect_selected_notify(move |dropdown| {
            let backend = backends
                .get(dropdown.selected() as usize)
                .copied()
                .unwrap_or_default();
            update_config(&controller, |config| config.capture_backend = backend);
        });
    }
    group.add(&row(
        &t!("settings.capture.backend").to_string(),
        Some(&t!("settings.capture.hint").to_string()),
        &dropdown,
    ));

    page.add(&group);
    page
}

// -------------------------------------------------------------- appearance

fn theme_label(theme: Theme) -> String {
    match theme {
        Theme::System => t!("settings.appearance.theme_system").to_string(),
        Theme::Dark => t!("settings.appearance.theme_dark").to_string(),
        Theme::Light => t!("settings.appearance.theme_light").to_string(),
    }
}

fn build_appearance(controller: &Rc<Controller>) -> adw::PreferencesPage {
    let config = controller.config();
    let page = page(
        &t!("settings.section.appearance").to_string(),
        "preferences-desktop-appearance-symbolic",
    );
    let group = group(&t!("settings.section.appearance").to_string());

    let themes = [Theme::System, Theme::Dark, Theme::Light];
    let theme_labels: Vec<String> = themes.iter().map(|theme| theme_label(*theme)).collect();
    let theme_selected = themes
        .iter()
        .position(|theme| *theme == config.theme)
        .unwrap_or(0);
    let theme_dropdown = widgets::dropdown(&theme_labels, theme_selected);
    {
        let controller = controller.clone();
        theme_dropdown.connect_selected_notify(move |dropdown| {
            let theme = themes
                .get(dropdown.selected() as usize)
                .copied()
                .unwrap_or_default();
            style::apply_theme(theme);
            update_config(&controller, |config| config.theme = theme);
        });
    }
    group.add(&row(
        &t!("settings.appearance.theme").to_string(),
        None,
        &theme_dropdown,
    ));

    let language_labels: Vec<String> = Language::ALL
        .iter()
        .map(|language| t!(language.label_key()).to_string())
        .collect();
    let language_selected = Language::ALL
        .iter()
        .position(|language| *language == config.language)
        .unwrap_or(0);
    let language_dropdown = widgets::dropdown(&language_labels, language_selected);
    {
        let controller = controller.clone();
        language_dropdown.connect_selected_notify(move |dropdown| {
            let language = Language::ALL
                .get(dropdown.selected() as usize)
                .copied()
                .unwrap_or_default();
            update_config(&controller, |config| config.language = language);
            controller.toast(&t!("language.restart_required").to_string());
        });
    }
    group.add(&row(
        &t!("settings.appearance.language").to_string(),
        None,
        &language_dropdown,
    ));
    group.add(&note_label(&t!("language.restart_required").to_string()));

    page.add(&group);
    page
}

// -------------------------------------------------------------------- about

fn build_about(controller: &Rc<Controller>) -> adw::PreferencesPage {
    let _ = controller;
    let page = page(
        &t!("settings.section.about").to_string(),
        "help-about-symbolic",
    );
    let group = group(&t!("settings.section.about").to_string());

    let version = gtk::Label::new(Some(crate::VERSION));
    version.add_css_class("dim-label");
    group.add(&row(
        &t!("settings.about.version").to_string(),
        None,
        &version,
    ));

    let gtk_version = format!(
        "{}.{}.{}",
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version()
    );
    group.add(&row(
        &t!("settings.about.gtk").to_string(),
        None,
        &gtk::Label::new(Some(&gtk_version)),
    ));

    let adw_version = format!(
        "{}.{}.{}",
        adw::major_version(),
        adw::minor_version(),
        adw::micro_version()
    );
    group.add(&row(
        &t!("settings.about.adwaita").to_string(),
        None,
        &gtk::Label::new(Some(&adw_version)),
    ));

    group.add(&row(
        &t!("settings.about.gstreamer").to_string(),
        None,
        &gtk::Label::new(Some(&gstreamer::version_string())),
    ));

    let license = gtk::LinkButton::builder()
        .label("GPL-3.0-or-later")
        .uri("https://www.gnu.org/licenses/gpl-3.0.html")
        .build();
    group.add(&row(
        &t!("settings.about.license").to_string(),
        None,
        &license,
    ));

    page.add(&group);
    page
}
