//! Offscreen render proof: builds a scene view, renders it to a PNG and exits.
//!
//! ```text
//! cargo run --release --example render_scene -- --out /tmp/signifiage-render.png
//! ```

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use signifiage::model::LayerKind;
use signifiage::model::project::{self, Project};
use signifiage::ui::render::{RenderData, SceneView, ViewMode};

fn main() -> glib::ExitCode {
    let mut out = PathBuf::from("/tmp/signifiage-render.png");
    let mut project_dir: Option<PathBuf> = None;
    let mut scene_name: Option<String> = None;
    let mut size = (1280, 720);

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => {
                if let Some(value) = args.next() {
                    out = PathBuf::from(value);
                }
            }
            "--project" => {
                if let Some(value) = args.next() {
                    project_dir = Some(PathBuf::from(value));
                }
            }
            "--scene" => scene_name = args.next(),
            "--width" => {
                if let Some(value) = args.next().and_then(|v| v.parse().ok()) {
                    size.0 = value;
                }
            }
            "--height" => {
                if let Some(value) = args.next().and_then(|v| v.parse().ok()) {
                    size.1 = value;
                }
            }
            other => {
                eprintln!("unknown argument: {other}");
                return glib::ExitCode::FAILURE;
            }
        }
    }

    let project_dir = project_dir.unwrap_or_else(default_project);
    let loaded = match project::load(&project_dir) {
        Ok(loaded) => loaded,
        Err(err) => {
            eprintln!("cannot load {}: {err}", project_dir.display());
            return glib::ExitCode::FAILURE;
        }
    };
    let project = loaded.project;

    let scene = pick_scene(&project, scene_name.as_deref());
    let Some(scene) = scene else {
        eprintln!("project has no scene to render");
        return glib::ExitCode::FAILURE;
    };

    let app = gtk::Application::builder()
        .application_id("io.github.ta4anx.Signifiage.RenderExample")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    let success = Rc::new(Cell::new(false));

    {
        let success = success.clone();
        let project = project.clone();
        let out = out.clone();
        let project_dir = project_dir.clone();
        app.connect_activate(move |app| {
            let view = SceneView::new(ViewMode::Program);
            view.set_design(
                project.design.width,
                project.design.height,
                &project.design.background,
                project.presentation.scale,
                project.presentation.rotation,
            );
            view.set_asset_base(Some(project_dir.clone()));
            view.set_scene(Some(scene.clone()));
            view.set_render_data(RenderData::default());

            let window = gtk::ApplicationWindow::builder()
                .application(app)
                .default_width(size.0)
                .default_height(size.1)
                .child(&view)
                .build();
            window.present();

            let success = success.clone();
            let out = out.clone();
            let window_clone = window.clone();
            glib::timeout_add_local_once(Duration::from_millis(400), move || {
                match view.render_to_texture(size.0, size.1) {
                    Some(texture) => match texture.save_to_png(&out) {
                        Ok(()) => {
                            println!("wrote {}", out.display());
                            success.set(true);
                        }
                        Err(err) => eprintln!("cannot write {}: {err}", out.display()),
                    },
                    None => eprintln!("renderer could not capture the scene"),
                }
                window_clone.close();
            });
        });
    }

    {
        let success = success.clone();
        app.connect_shutdown(move |_| {
            if !success.get() {
                eprintln!("render failed");
            }
        });
    }

    let code = app.run_with_args(&["render_scene"]);
    if success.get() {
        glib::ExitCode::SUCCESS
    } else {
        let _ = code;
        glib::ExitCode::FAILURE
    }
}

fn default_project() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/fixtures/demo-project");
    path
}

fn pick_scene(project: &Project, name: Option<&str>) -> Option<signifiage::model::Scene> {
    if let Some(name) = name {
        return project
            .scenes
            .iter()
            .find(|scene| scene.name.eq_ignore_ascii_case(name))
            .cloned();
    }
    project
        .scenes
        .iter()
        .find(|scene| {
            scene
                .layers
                .iter()
                .any(|layer| matches!(layer.kind, LayerKind::Text(_)))
                && scene
                    .layers
                    .iter()
                    .any(|layer| matches!(layer.kind, LayerKind::Clock(_)))
        })
        .or_else(|| project.scenes.first())
        .cloned()
}
