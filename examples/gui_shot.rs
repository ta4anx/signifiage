//! Boot the operator window offscreen-ish and save a PNG of the requested page.
//!
//! ```text
//! cargo run --release --example gui_shot -- --project /tmp/show --page run --out /tmp/run.png
//! ```

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use signifiage::config::Config;
use signifiage::ui::window::MainWindow;

fn main() -> glib::ExitCode {
    let mut project = PathBuf::from("tests/fixtures/demo-project");
    let mut page = "design".to_string();
    let mut out = PathBuf::from("/tmp/signifiage-gui.png");
    let mut width = 1440;
    let mut height = 900;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--project" => {
                if let Some(value) = args.next() {
                    project = PathBuf::from(value);
                }
            }
            "--page" => {
                if let Some(value) = args.next() {
                    page = value;
                }
            }
            "--out" => {
                if let Some(value) = args.next() {
                    out = PathBuf::from(value);
                }
            }
            "--width" => {
                if let Some(value) = args.next().and_then(|v| v.parse().ok()) {
                    width = value;
                }
            }
            "--height" => {
                if let Some(value) = args.next().and_then(|v| v.parse().ok()) {
                    height = value;
                }
            }
            other => {
                eprintln!("unknown argument: {other}");
                return glib::ExitCode::FAILURE;
            }
        }
    }

    let app = adw::Application::builder()
        .application_id("io.github.ta4anx.Signifiage.GuiShot")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    let success = Rc::new(std::cell::Cell::new(false));
    let window: Rc<RefCell<Option<MainWindow>>> = Rc::new(RefCell::new(None));

    {
        let success = success.clone();
        let window = window.clone();
        app.connect_activate(move |app| {
            let config = Config {
                remote: Default::default(),
                ..Config::default()
            };
            let main = MainWindow::new(app, Some(project.clone()), config);
            main.show_page(&page);
            window.replace(Some(main));

            let success = success.clone();
            let window = window.clone();
            let out = out.clone();
            let app = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(900), move || {
                let texture = window
                    .borrow()
                    .as_ref()
                    .and_then(|main| main.capture(width, height));
                match texture {
                    Some(texture) => match texture.save_to_png(&out) {
                        Ok(()) => {
                            println!("wrote {}", out.display());
                            success.set(true);
                        }
                        Err(err) => eprintln!("cannot write {}: {err}", out.display()),
                    },
                    None => eprintln!("capture failed"),
                }
                if let Some(main) = window.borrow().as_ref() {
                    if let Some(outputs) = main.controller().outputs() {
                        outputs.close_all();
                    }
                }
                app.quit();
            });
        });
    }

    app.run_with_args(&["gui_shot"]);
    if success.get() {
        glib::ExitCode::SUCCESS
    } else {
        glib::ExitCode::FAILURE
    }
}
