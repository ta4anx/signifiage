//! Output windows: program, preview and prompter on chosen monitors.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::gdk;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use crate::config::{Config, MonitorRef};
use crate::ui::render::{SceneView, ViewMode};

/// Which output a window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputRole {
    Program,
    Preview,
    Prompter,
}

impl OutputRole {
    pub fn view_mode(self) -> ViewMode {
        match self {
            OutputRole::Program => ViewMode::Program,
            OutputRole::Preview => ViewMode::Preview,
            OutputRole::Prompter => ViewMode::Prompter,
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            OutputRole::Program => "outputs.program",
            OutputRole::Preview => "outputs.preview",
            OutputRole::Prompter => "outputs.prompter",
        }
    }
}

struct OutputWindow {
    window: gtk::Window,
    view: SceneView,
    fullscreen: bool,
    monitor_key: String,
}

pub struct OutputManager {
    app: adw::Application,
    windows: RefCell<HashMap<OutputRole, OutputWindow>>,
    windowed: Cell<bool>,
    inhibit: Cell<u32>,
}

impl OutputManager {
    pub fn new(app: &adw::Application) -> Rc<Self> {
        Rc::new(Self {
            app: app.clone(),
            windows: RefCell::new(HashMap::new()),
            windowed: Cell::new(false),
            inhibit: Cell::new(0),
        })
    }

    /// Force every role into a 960x540 window (`--windowed`, config switch).
    pub fn set_windowed(&self, windowed: bool) {
        self.windowed.set(windowed);
    }

    pub fn view(&self, role: OutputRole) -> Option<SceneView> {
        self.windows
            .borrow()
            .get(&role)
            .map(|entry| entry.view.clone())
    }

    /// The window that shows this role, if it is open.
    pub fn window_for(&self, role: OutputRole) -> Option<gtk::Window> {
        self.windows
            .borrow()
            .get(&role)
            .map(|entry| entry.window.clone())
    }

    pub fn has(&self, role: OutputRole) -> bool {
        self.windows.borrow().contains_key(&role)
    }

    pub fn program_view(&self) -> Option<SceneView> {
        self.view(OutputRole::Program)
    }

    pub fn preview_view(&self) -> Option<SceneView> {
        self.view(OutputRole::Preview)
    }

    /// Open, move or close windows so they match `config` and the current monitors.
    pub fn apply(&self, config: &Config) {
        self.set_windowed(config.windowed_outputs);
        let monitors = monitor_list();

        // Program always exists; preview/prompter only when a monitor is assigned.
        self.ensure_role(OutputRole::Program, config, &monitors, true);
        self.ensure_role(
            OutputRole::Preview,
            config,
            &monitors,
            config.outputs.preview.monitor.is_some(),
        );
        self.ensure_role(
            OutputRole::Prompter,
            config,
            &monitors,
            config.outputs.prompter.monitor.is_some(),
        );
        self.update_inhibit(config);
    }

    fn ensure_role(
        &self,
        role: OutputRole,
        config: &Config,
        monitors: &[gdk::Monitor],
        wanted: bool,
    ) {
        if !wanted {
            self.close(role);
            return;
        }
        let role_config = match role {
            OutputRole::Program => &config.outputs.program,
            OutputRole::Preview => &config.outputs.preview,
            OutputRole::Prompter => &config.outputs.prompter,
        };

        // First run with a single monitor: program stays a windowed 960x540.
        let single_monitor_window =
            role == OutputRole::Program && role_config.monitor.is_none() && monitors.len() <= 1;
        let fullscreen = !self.windowed.get()
            && !single_monitor_window
            && (role_config.fullscreen || role_config.monitor.is_some());

        let monitor_index = resolve_monitor(role_config.monitor.as_ref(), monitors);
        let monitor_key = monitor_index
            .map(|index| monitor_key(&monitors[index], index))
            .unwrap_or_else(|| "none".to_string());

        if let Some(entry) = self.windows.borrow().get(&role) {
            let same = entry.fullscreen == fullscreen && entry.monitor_key == monitor_key;
            if same {
                entry.window.present();
                return;
            }
        }

        self.close(role);
        let view = SceneView::new(role.view_mode());
        view.set_can_focus(false);
        view.set_focusable(false);
        let window = gtk::Window::builder()
            .application(&self.app)
            .decorated(false)
            .deletable(false)
            .resizable(true)
            .title(&format!("Signifiage {}", rust_i18n::t!(role.label_key())))
            .build();
        window.set_can_focus(false);
        window.set_child(Some(&view));
        window.set_cursor_from_name(Some("none"));
        window.set_default_size(960, 540);

        if fullscreen {
            if let Some(index) = monitor_index {
                window.fullscreen_on_monitor(&monitors[index]);
            } else {
                window.fullscreen();
            }
        }
        window.present();
        tracing::info!(
            role = ?role,
            fullscreen,
            monitor = %monitor_key,
            "output window opened"
        );
        self.windows.borrow_mut().insert(
            role,
            OutputWindow {
                window,
                view,
                fullscreen,
                monitor_key,
            },
        );
    }

    fn close(&self, role: OutputRole) {
        if let Some(entry) = self.windows.borrow_mut().remove(&role) {
            entry.window.destroy();
            tracing::info!(?role, "output window closed");
        }
    }

    /// Open a windowed output even when unassigned (prompter fallback).
    pub fn open_windowed(&self, role: OutputRole) {
        if self.windows.borrow().contains_key(&role) {
            if let Some(entry) = self.windows.borrow().get(&role) {
                entry.window.present();
            }
            return;
        }
        let view = SceneView::new(role.view_mode());
        let window = gtk::Window::builder()
            .application(&self.app)
            .title(&format!("Signifiage {}", rust_i18n::t!(role.label_key())))
            .build();
        window.set_child(Some(&view));
        window.set_default_size(960, 540);
        window.present();
        self.windows.borrow_mut().insert(
            role,
            OutputWindow {
                window,
                view,
                fullscreen: false,
                monitor_key: "none".to_string(),
            },
        );
    }

    pub fn close_all(&self) {
        // Release the idle inhibitor before the windows go away: the cookie dies
        // with its window, and uninhibiting afterwards warns.
        let cookie = self.inhibit.take();
        if cookie != 0 {
            self.app.uninhibit(cookie);
        }
        for (_, entry) in self.windows.borrow_mut().drain() {
            entry.window.destroy();
        }
    }

    fn update_inhibit(&self, config: &Config) {
        let any_visible = !self.windows.borrow().is_empty();
        let cookie = self.inhibit.get();
        match (any_visible, cookie) {
            (true, 0) => {
                let window = self
                    .windows
                    .borrow()
                    .values()
                    .next()
                    .map(|entry| entry.window.clone());
                if let Some(window) = window {
                    let flags = self.app.inhibit(
                        Some(&window),
                        gtk::ApplicationInhibitFlags::IDLE,
                        Some(&rust_i18n::t!("outputs.inhibit_reason").to_string()),
                    );
                    self.inhibit.set(flags);
                }
            }
            (false, flags) if flags != 0 => {
                self.app.uninhibit(flags);
                self.inhibit.set(0);
            }
            _ => {}
        }
        let _ = config;
    }
}

/// Snapshot of the connected monitors.
pub fn monitor_list() -> Vec<gdk::Monitor> {
    let Some(display) = gdk::Display::default() else {
        return Vec::new();
    };
    let model = display.monitors();
    let mut monitors = Vec::new();
    for index in 0..model.n_items() {
        if let Some(monitor) = model
            .item(index)
            .and_then(|item| item.downcast::<gdk::Monitor>().ok())
        {
            monitors.push(monitor);
        }
    }
    monitors
}

/// Stable identity of a monitor for config comparisons.
pub fn monitor_key(monitor: &gdk::Monitor, index: usize) -> String {
    format!(
        "{}|{}|{}",
        monitor
            .connector()
            .map(|c| c.to_string())
            .unwrap_or_default(),
        monitor.model().map(|m| m.to_string()).unwrap_or_default(),
        index
    )
}

/// Connector first, then a unique model, then the index, else `None`.
pub fn resolve_monitor(reference: Option<&MonitorRef>, monitors: &[gdk::Monitor]) -> Option<usize> {
    let reference = reference?;
    if let Some(connector) = reference
        .connector
        .as_deref()
        .filter(|value| !value.is_empty())
        && let Some(index) = monitors
            .iter()
            .position(|monitor| monitor.connector().as_deref() == Some(connector))
    {
        return Some(index);
    }
    if !reference.model.is_empty() {
        let matching: Vec<usize> = monitors
            .iter()
            .enumerate()
            .filter(|(_, monitor)| monitor.model().as_deref() == Some(reference.model.as_str()))
            .map(|(index, _)| index)
            .collect();
        if matching.len() == 1 {
            return Some(matching[0]);
        }
    }
    let index = reference.index as usize;
    if index < monitors.len() {
        return Some(index);
    }
    None
}

/// Describe monitors for the settings dropdowns: `model — WxH`.
pub fn describe_monitors(monitors: &[gdk::Monitor]) -> Vec<(usize, String, MonitorRef)> {
    monitors
        .iter()
        .enumerate()
        .map(|(index, monitor)| {
            let geometry = monitor.geometry();
            let model = monitor.model().map(|m| m.to_string()).unwrap_or_default();
            let label = format!(
                "{} — {}×{}",
                if model.is_empty() {
                    "Monitor"
                } else {
                    model.as_str()
                },
                geometry.width(),
                geometry.height()
            );
            let reference = MonitorRef {
                connector: monitor.connector().map(|c| c.to_string()),
                model,
                index: index as u32,
            };
            (index, label, reference)
        })
        .collect()
}

/// Default output config for a fresh install.
pub fn default_outputs(
    monitor_count: usize,
    last_monitor: Option<MonitorRef>,
) -> crate::config::Outputs {
    let mut outputs = crate::config::Outputs::default();
    outputs.program.fullscreen = monitor_count >= 2 && !outputs.program.fullscreen;
    outputs.program.monitor = if monitor_count >= 2 {
        last_monitor
    } else {
        None
    };
    outputs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_view_modes() {
        assert_eq!(OutputRole::Program.view_mode(), ViewMode::Program);
        assert_eq!(OutputRole::Preview.view_mode(), ViewMode::Preview);
        assert_eq!(OutputRole::Prompter.view_mode(), ViewMode::Prompter);
    }

    #[test]
    fn resolve_requires_a_reference() {
        assert_eq!(resolve_monitor(None, &[]), None);
    }

    #[test]
    fn single_monitor_defaults_are_windowed() {
        let outputs = default_outputs(1, None);
        assert!(outputs.program.monitor.is_none());
        assert!(!outputs.program.fullscreen);
        let outputs = default_outputs(2, None);
        assert!(outputs.program.fullscreen);
    }
}
