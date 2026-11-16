//! Operator GUI application bootstrap.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;

use crate::config::Config;
use crate::services::controller::Controller;

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
    let controller: Rc<RefCell<Option<Rc<Controller>>>> = Rc::new(RefCell::new(None));

    {
        let shared = shared.clone();
        let pending = pending.clone();
        let controller = controller.clone();
        app.connect_activate(move |app| {
            let requested = pending.borrow_mut().take();
            if let Some(existing) = app.active_window() {
                existing.present();
                // A second launch forwards its project to the running window.
                if let (Some(path), Some(running)) = (requested, controller.borrow().clone()) {
                    if let Err(err) = running.open_project(&path) {
                        tracing::warn!("cannot open {}: {err}", path.display());
                    }
                }
                return;
            }
            let startup = shared.borrow().clone();
            let project = requested.or(startup.project);
            let window = crate::ui::window::MainWindow::new(app, project, startup.config);
            *controller.borrow_mut() = Some(window.controller());
            window.present();
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

    // Args are already parsed by clap; keep GTK from rejecting the project path.
    app.run_with_args(&["signifiage"])
}
