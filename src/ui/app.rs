//! Operator GUI application bootstrap.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;

use crate::config::Config;

/// Where a fresh GUI process starts: project to open and loaded config.
#[derive(Debug, Clone)]
pub struct Startup {
    pub project: Option<PathBuf>,
    pub config: Config,
}

/// Run the operator GUI. Language/theme are applied by `main`.
pub fn run(startup: Startup) -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(crate::APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    let shared = Rc::new(RefCell::new(startup));
    let pending: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    // The window owns the live state (`Rc<Inner>`); every button, menu action and
    // shortcut upgrades a weak reference to it. GTK keeps the toplevel on screen
    // by itself, so this slot is what keeps the handlers connected for the whole
    // application lifetime.
    let window_slot: Rc<RefCell<Option<crate::ui::window::MainWindow>>> =
        Rc::new(RefCell::new(None));

    {
        let shared = shared.clone();
        let pending = pending.clone();
        let window_slot = window_slot.clone();
        app.connect_activate(move |app| {
            let requested = pending.borrow_mut().take();
            if let Some(existing) = app.active_window() {
                existing.present();
                // A second launch forwards its project to the running window.
                let running = window_slot
                    .borrow()
                    .as_ref()
                    .map(|window| window.controller());
                if let (Some(path), Some(running)) = (requested, running) {
                    if let Err(err) = running.open_project(&path) {
                        tracing::warn!("cannot open {}: {err}", path.display());
                    }
                }
                return;
            }
            let startup = shared.borrow().clone();
            let project = requested.or(startup.project);
            let window = crate::ui::window::MainWindow::new(app, project, startup.config);
            window.present();
            *window_slot.borrow_mut() = Some(window);
        });
    }

    {
        let pending = pending.clone();
        app.connect_open(move |app, files, _hint| {
            if let Some(path) = files.first().and_then(|file| file.path()) {
                *pending.borrow_mut() = Some(path);
            }
            app.activate();
        });
    }

    {
        let window_slot = window_slot.clone();
        app.connect_shutdown(move |_| {
            window_slot.borrow_mut().take();
        });
    }

    // Args are already parsed by clap; keep GTK from rejecting the project path.
    app.run_with_args(&["signifiage"])
}
