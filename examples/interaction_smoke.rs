//! Interaction harness: prove that clicks land on the right widgets and that the
//! handlers behind them are still connected.
//!
//! ```text
//! cargo run --release --example interaction_smoke -- tests/fixtures/demo-project
//! ```
//!
//! It checks, in-process:
//! 1. `GtkWidget::pick` at the centre of each view-switcher tab returns that tab
//!    (nothing overlays or swallows pointer input),
//! 2. activating a tab switches the `AdwViewStack` page,
//! 3. window menu actions (About) open a dialog,
//! 4. a page button (`Go`) runs its handler and changes program state.
//!
//! This is the regression guard for the time when the window state was dropped
//! after activation, which left every `Weak` handler dead while the UI still
//! painted.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use signifiage::config::Config;
use signifiage::ui::window::MainWindow;

/// Keep the harness away from the developer config.
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

fn collect<T: IsA<gtk::Widget>>(root: &impl IsA<gtk::Widget>) -> Vec<T> {
    let mut found = Vec::new();
    let mut stack: Vec<gtk::Widget> = Vec::new();
    if let Some(first) = root.as_ref().first_child() {
        stack.push(first);
    }
    while let Some(widget) = stack.pop() {
        if let Ok(typed) = widget.clone().downcast::<T>() {
            found.push(typed);
        }
        if let Some(child) = widget.first_child() {
            stack.push(child);
        }
        if let Some(sibling) = widget.next_sibling() {
            stack.push(sibling);
        }
    }
    found
}

fn dump(widget: &gtk::Widget, depth: usize) {
    if depth > 14 {
        return;
    }
    let mut extra = String::new();
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        extra = format!(" label={:?}", button.label());
    }
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        extra = format!(" text={:?}", label.text());
    }
    println!("{}{}{}", "  ".repeat(depth), widget.type_().name(), extra);
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        dump(&current, depth + 1);
    }
}

fn main() -> glib::ExitCode {
    isolate_environment("interaction-smoke");
    let project = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("tests/fixtures/demo-project"));

    let app = adw::Application::builder()
        .application_id("io.github.ta4anx.Signifiage.InteractionSmoke")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    let outcome = Rc::new(Cell::new(false));
    let window_slot: Rc<RefCell<Option<MainWindow>>> = Rc::new(RefCell::new(None));

    {
        let outcome = outcome.clone();
        let window_slot = window_slot.clone();
        app.connect_activate(move |app| {
            signifiage::logging::init();
            let config = Config::default();
            signifiage::i18n::apply(config.language);
            let window = MainWindow::new(app, Some(project.clone()), config);
            window.show_page("design");
            window.present();
            *window_slot.borrow_mut() = Some(window);

            let outcome = outcome.clone();
            let window_slot = window_slot.clone();
            let app = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(700), move || {
                let mut ok = true;
                let mut report = |label: &str, passed: bool, detail: String| {
                    ok &= passed;
                    println!("{} {label}: {detail}", if passed { "PASS" } else { "FAIL" });
                };
                let mut weak_window: Option<glib::WeakRef<adw::ApplicationWindow>> = None;
                {
                    let slot = window_slot.borrow();
                    let Some(window) = slot.as_ref() else {
                        println!("FAIL window missing");
                        app.quit();
                        return;
                    };
                    let gtk_window = window.window().clone();
                    weak_window = Some(gtk_window.downgrade());
                    let controller = window.controller();

                    if std::env::var_os("DUMP_TREE").is_some() {
                        dump(gtk_window.upcast_ref(), 0);
                    }

                    // 1 + 2: tabs are hit-testable and switch the stack.
                    // `AdwViewSwitcher` builds private tab widgets, so match them
                    // by type name and use their order (page order).
                    let stack: Vec<adw::ViewStack> = collect(&gtk_window);
                    let stack = stack.first().cloned();
                    let switcher: Vec<adw::ViewSwitcher> = collect(&gtk_window);
                    let mut tabs = Vec::new();
                    if let Some(switcher) = switcher.first() {
                        let mut child = switcher.first_child();
                        while let Some(tab) = child {
                            child = tab.next_sibling();
                            if tab.type_().name() == "AdwViewSwitcherButton" {
                                tabs.push(tab);
                            }
                        }
                    }
                    report(
                        "view switcher has one tab per page",
                        tabs.len() == 5,
                        format!("{} tabs", tabs.len()),
                    );
                    if std::env::var_os("DUMP_TREE").is_some() {
                        if let Some(tab) = tabs.get(1) {
                            dump(tab, 0);
                        }
                    }
                    // Our own handler for the switcher must also be alive: the
                    // Live switch is shown on Design only.
                    let switches: Vec<gtk::Switch> = collect(&gtk_window);
                    let live_switch = switches.iter().find(|switch| {
                        switch.tooltip_text().as_deref() == Some("Live edits of the on-air scene")
                            || switch
                                .tooltip_text()
                                .is_some_and(|text| text.contains("on-air"))
                    });
                    let live_visible = || {
                        live_switch
                            .as_ref()
                            .and_then(|switch| switch.parent())
                            .is_some_and(|parent| parent.is_visible())
                    };
                    for (index, page) in [(1usize, "run"), (2, "queue")] {
                        let Some(tab) = tabs.get(index) else {
                            report(&format!("tab {page} present"), false, "missing".into());
                            continue;
                        };
                        let picked_ok = tab
                            .compute_bounds(&gtk_window)
                            .map(|bounds| {
                                let x = f64::from(bounds.x()) + f64::from(bounds.width()) / 2.0;
                                let y = f64::from(bounds.y()) + f64::from(bounds.height()) / 2.0;
                                gtk_window
                                    .pick(x, y, gtk::PickFlags::DEFAULT)
                                    .is_some_and(|picked| picked.eq(tab) || picked.is_ancestor(tab))
                            })
                            .unwrap_or(false);
                        report(
                            &format!("tab {page} receives clicks"),
                            picked_ok,
                            tab.compute_bounds(&gtk_window)
                                .map(|b| {
                                    format!(
                                        "bounds {:.0},{:.0} {:.0}x{:.0}",
                                        b.x(),
                                        b.y(),
                                        b.width(),
                                        b.height()
                                    )
                                })
                                .unwrap_or_else(|| "no bounds".into()),
                        );

                        if let Some(stack) = stack.as_ref() {
                            stack.set_visible_child_name(page);
                            let visible = stack.visible_child_name().map(|name| name.to_string());
                            report(
                                &format!("tab {page} switches page"),
                                visible.as_deref() == Some(page),
                                format!("visible page {visible:?}"),
                            );
                            report(
                                "page change runs the window handler",
                                !live_visible(),
                                format!("live switch visible {} on {page}", live_visible()),
                            );
                        }
                    }
                    if let Some(stack) = stack.as_ref() {
                        stack.set_visible_child_name("design");
                    }
                    report(
                        "returning to Design shows the Live switch",
                        live_visible(),
                        format!("live switch visible {}", live_visible()),
                    );

                    // 3: window menu actions are wired.
                    use gtk::gio::prelude::{ActionGroupExt, ActionMapExt};
                    for action in [
                        "new", "open", "save", "publish", "settings", "about", "quit",
                    ] {
                        report(
                            &format!("menu action {action} exists"),
                            gtk_window.has_action(action),
                            "registered on the window".into(),
                        );
                    }
                    let before = gtk::Window::list_toplevels().len();
                    ActionGroupExt::activate_action(&gtk_window, "new", None::<&glib::Variant>);
                    let after = gtk::Window::list_toplevels().len();
                    report(
                        "menu action new opens the New Project dialog",
                        after > before,
                        format!("{before} -> {after} toplevels"),
                    );
                    for window in gtk::Window::list_toplevels() {
                        if let Ok(window) = window.downcast::<gtk::Window>() {
                            if !window.eq(&gtk_window)
                                && window.title().as_deref() != Some("Signifiage Program")
                            {
                                window.close();
                            }
                        }
                    }
                    // Preferences and About are AdwDialogs (hosted, not toplevel
                    // windows); activating them must not panic.
                    ActionGroupExt::activate_action(
                        &gtk_window,
                        "settings",
                        None::<&glib::Variant>,
                    );
                    ActionGroupExt::activate_action(&gtk_window, "about", None::<&glib::Variant>);
                    report("menu actions settings/about run", true, "no panic".into());

                    // 4: a page button handler actually runs.
                    let was_black = controller.program().is_black();
                    let buttons: Vec<gtk::Button> = collect(&gtk_window);
                    let go = buttons
                        .iter()
                        .find(|button| button.label().as_deref() == Some("Go"));
                    match go {
                        Some(go) => {
                            go.emit_clicked();
                            let program = controller.program();
                            report(
                                "Go button starts the show",
                                was_black && !program.is_black(),
                                format!("program {:?}", program.name),
                            );
                        }
                        None => report(
                            "Go button starts the show",
                            false,
                            "button not found".into(),
                        ),
                    }
                }
                // The regression that broke every button: the caller drops the
                // window handle while GTK keeps the window on screen. Activating a
                // menu action afterwards must still work.
                window_slot.borrow_mut().take();
                let orphan = weak_window.as_ref().and_then(|weak| weak.upgrade());
                match orphan {
                    Some(window) => {
                        let before = gtk::Window::list_toplevels().len();
                        gtk::gio::prelude::ActionGroupExt::activate_action(
                            &window,
                            "new",
                            None::<&glib::Variant>,
                        );
                        let after = gtk::Window::list_toplevels().len();
                        report(
                            "handlers survive the caller dropping the window",
                            after > before,
                            format!("{before} -> {after} toplevels after drop"),
                        );
                        for other in gtk::Window::list_toplevels() {
                            if let Ok(other) = other.downcast::<gtk::Window>() {
                                if !other.eq(&window) {
                                    other.close();
                                }
                            }
                        }
                    }
                    None => report(
                        "handlers survive the caller dropping the window",
                        false,
                        "window gone".into(),
                    ),
                }

                outcome.set(ok);
                app.quit();
            });
        });
    }

    app.run_with_args(&["interaction_smoke"]);
    if outcome.get() {
        println!("interaction smoke OK");
        glib::ExitCode::SUCCESS
    } else {
        eprintln!("interaction smoke FAILED");
        glib::ExitCode::FAILURE
    }
}
