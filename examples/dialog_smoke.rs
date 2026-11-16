//! Smoke harness: construct every dialog against a real project and window.
//!
//! ```text
//! cargo run --release --example dialog_smoke -- /tmp/signifiage-smoke
//! ```
//!
//! Construction-time GTK errors (bad widget parents, missing models, panics in
//! signal wiring) show up here without a human clicking through the UI.
//!
//! Like `gui_shot`, the harness uses its own XDG config/state directory.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use signifiage::config::Config;
use signifiage::ui::dialogs;
use signifiage::ui::window::MainWindow;

/// Point config/state at a scratch directory unless the caller chose one.
fn isolate_environment(tag: &str) {
    for variable in ["XDG_CONFIG_HOME", "XDG_STATE_HOME"] {
        if std::env::var_os(variable).is_some() {
            continue;
        }
        let dir = std::env::temp_dir().join(format!(
            "signifiage-{tag}-{}-{variable}",
            std::process::id()
        ));
        if std::fs::create_dir_all(&dir).is_ok() {
            // SAFETY: called before GTK or any worker thread starts.
            unsafe {
                std::env::set_var(variable, dir);
            }
        }
    }
}

fn main() -> glib::ExitCode {
    isolate_environment("dialog-smoke");
    let project = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("tests/fixtures/demo-project"));

    let app = adw::Application::builder()
        .application_id("io.github.ta4anx.Signifiage.DialogSmoke")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    let success = Rc::new(Cell::new(false));
    let main_window: Rc<RefCell<Option<MainWindow>>> = Rc::new(RefCell::new(None));

    {
        let success = success.clone();
        let main_window = main_window.clone();
        app.connect_activate(move |app| {
            let window = MainWindow::new(app, Some(project.clone()), Config::default());
            window.show_page("design");
            let controller = window.controller();
            let overlay = window.overlay();
            let parent = window.window().clone();
            main_window.replace(Some(window));

            // Dialogs open sequentially; each one is closed by the harness.
            let steps: Vec<Box<dyn Fn()>> = vec![
                Box::new({
                    let controller = controller.clone();
                    let overlay = overlay.clone();
                    let parent = parent.clone();
                    move || {
                        dialogs::timers_dialog(&controller, &overlay, Some(parent.upcast_ref()));
                    }
                }),
                Box::new({
                    let controller = controller.clone();
                    let overlay = overlay.clone();
                    let parent = parent.clone();
                    move || {
                        dialogs::schedules_dialog(&controller, &overlay, Some(parent.upcast_ref()));
                    }
                }),
                Box::new({
                    let parent = parent.clone();
                    let controller = controller.clone();
                    move || dialogs::new_project(&parent, &controller)
                }),
                Box::new({
                    let parent = parent.clone();
                    move || dialogs::about(&parent)
                }),
            ];

            for (index, step) in steps.into_iter().enumerate() {
                glib::timeout_add_local_once(
                    Duration::from_millis(400 * (index as u64 + 1)),
                    move || step(),
                );
            }

            let success = success.clone();
            let main_window = main_window.clone();
            let app = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(2400), move || {
                // Count the windows that were opened, then close everything.
                let toplevels = gtk::Window::list_toplevels();
                let count = toplevels.len();
                for window in toplevels {
                    if let Ok(window) = window.downcast::<gtk::Window>() {
                        window.close();
                    }
                }
                if let Some(main) = main_window.borrow().as_ref() {
                    if let Some(outputs) = main.controller().outputs() {
                        outputs.close_all();
                    }
                }
                if count >= 4 {
                    success.set(true);
                    println!("dialog smoke OK: {count} toplevel windows");
                } else {
                    eprintln!("dialog smoke: only {count} toplevel windows");
                }
                app.quit();
            });
        });
    }

    app.run_with_args(&["dialog_smoke"]);
    if success.get() {
        glib::ExitCode::SUCCESS
    } else {
        glib::ExitCode::FAILURE
    }
}
