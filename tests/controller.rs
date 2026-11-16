//! Controller semantics: program/preview, transport, patterns, undo, live edit.
//!
//! These use the real controller without any output windows, so they run without
//! a compositor.

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Once;

use signifiage::config::Config;
use signifiage::model::layer::{LayerKind, PatternKind, Rect};
use signifiage::model::project::{self};
use signifiage::services::controller::{Controller, Mode, ProgramSource, ViewRole};
use signifiage::ui::render::SceneView;

static INIT: Once = Once::new();

/// Point config/state at a scratch directory once per test process.
fn init_env() {
    INIT.call_once(|| {
        let root = std::env::temp_dir().join("signifiage-tests-config");
        fs::create_dir_all(&root).unwrap();
        // SAFETY: test process, single-threaded initialisation.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
            std::env::set_var("XDG_STATE_HOME", root.join("state"));
        }
    });
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn demo_project() -> PathBuf {
    init_env();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo-project");
    let dir = std::env::temp_dir().join(format!("signifiage-ctl-{}", signifiage::model::new_id()));
    copy_dir(&source, &dir);
    dir
}

fn open(dir: &Path) -> Rc<Controller> {
    let controller = Controller::new(Config::default(), Mode::Gui);
    controller.open_project(dir).expect("project opens");
    controller
}

#[test]
fn opening_a_project_starts_black_with_the_default_preview() {
    let dir = demo_project();
    let controller = open(&dir);
    assert!(controller.is_open());
    assert!(controller.program().is_black());
    let preview = controller.preview_state();
    assert_eq!(preview.index, 0);
    assert_eq!(
        controller.preview_scene().map(|scene| scene.name),
        Some("Welcome".to_string())
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn go_commits_the_preview_and_advances_it() {
    let dir = demo_project();
    let controller = open(&dir);
    assert!(controller.go());
    let program = controller.program();
    assert_eq!(program.name, "Welcome");
    assert!(matches!(
        program.source,
        ProgramSource::Playlist { index: 0, .. }
    ));
    assert_eq!(program.item_duration, 8);
    assert_eq!(controller.preview_state().index, 1);

    assert!(controller.go());
    assert_eq!(controller.program().name, "Deck");
    assert_eq!(controller.preview_state().index, 2);

    assert!(controller.go());
    assert_eq!(controller.program().name, "Queue");
    // The playlist loops back to the first item.
    assert_eq!(controller.preview_state().index, 0);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn prev_wraps_with_loop_and_reports_the_beginning_without_loop() {
    let dir = demo_project();
    let controller = open(&dir);
    assert!(controller.go());
    assert!(controller.prev());
    // Looping playlist: from index 0 back to the last item.
    assert_eq!(controller.program().name, "Queue");
    assert_eq!(controller.preview_state().index, 0);

    // Turning loop off makes the first item a no-op boundary.
    let playlist_id = controller.preview_state().playlist_id;
    controller.mutate(Some("test-loop"), |project| {
        if let Some(playlist) = project.playlist_mut(&playlist_id) {
            playlist.r#loop = false;
        }
    });
    controller.set_preview_index(0);
    // With nothing on air, Previous works from the preview position, which is
    // the first item of a non-looping playlist.
    controller.blackout();
    assert!(!controller.prev());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn take_scene_is_manual_and_suspends_auto_advance() {
    let dir = demo_project();
    let controller = open(&dir);
    let queue_id = controller
        .project_clone()
        .scenes
        .iter()
        .find(|scene| scene.name == "Queue")
        .map(|scene| scene.id.clone())
        .unwrap();
    assert!(controller.take_scene(&queue_id));
    let program = controller.program();
    assert_eq!(program.name, "Queue");
    assert!(matches!(program.source, ProgramSource::Manual { .. }));
    assert_eq!(controller.program_remaining(), None);
    // Preview is untouched by Take.
    assert_eq!(controller.preview_state().index, 0);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn patterns_save_and_restore_the_program() {
    let dir = demo_project();
    let controller = open(&dir);
    controller.go();
    controller.show_pattern(PatternKind::Colorbars);
    let program = controller.program();
    assert!(matches!(program.source, ProgramSource::Pattern { .. }));
    assert!(program.saved.is_some());
    assert_eq!(controller.program_remaining(), None);

    controller.resume_program();
    let program = controller.program();
    assert_eq!(program.name, "Welcome");
    assert!(matches!(
        program.source,
        ProgramSource::Playlist { index: 0, .. }
    ));
    assert!(program.saved.is_none());

    // Resuming with no snapshot clears to black.
    controller.show_pattern(PatternKind::Focus);
    assert!(controller.program().saved.is_some());
    controller.blackout();
    controller.resume_program();
    assert!(controller.program().is_black());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn black_overlay_hides_the_program_without_clearing_it() {
    let dir = demo_project();
    let controller = open(&dir);
    controller.go();
    controller.set_black(true);
    assert!(controller.is_black());
    assert_eq!(controller.program().name, "Welcome");
    controller.set_black(false);
    assert!(!controller.is_black());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn deleting_the_live_scene_clears_the_program_and_playlists() {
    let dir = demo_project();
    let controller = open(&dir);
    let welcome = controller.preview_scene().unwrap();
    controller.take_scene(&welcome.id);
    assert_eq!(controller.program().name, "Welcome");

    assert!(controller.delete_scene(&welcome.id));
    assert!(controller.program().is_black());
    let project = controller.project_clone();
    assert!(project.scenes.iter().all(|scene| scene.id != welcome.id));
    for playlist in &project.playlists {
        assert!(
            playlist
                .items
                .iter()
                .all(|item| item.scene_id != welcome.id)
        );
    }
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn playlist_editing_updates_the_preview_and_undo_restores_it() {
    let dir = demo_project();
    let controller = open(&dir);
    let scene_id = controller.preview_scene().unwrap().id;
    assert!(controller.playlist_add_scene(&scene_id));
    let playlist_id = controller.preview_state().playlist_id;
    assert_eq!(
        controller
            .project_clone()
            .playlist(&playlist_id)
            .unwrap()
            .items
            .len(),
        4
    );
    assert!(controller.undo());
    assert_eq!(
        controller
            .project_clone()
            .playlist(&playlist_id)
            .unwrap()
            .items
            .len(),
        3
    );
    assert!(controller.redo());
    assert_eq!(
        controller
            .project_clone()
            .playlist(&playlist_id)
            .unwrap()
            .items
            .len(),
        4
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn last_playlist_cannot_be_deleted_and_preview_falls_back() {
    let dir = demo_project();
    let controller = open(&dir);
    let original = controller.preview_state().playlist_id;
    let second = controller.add_playlist("Second");
    assert_eq!(controller.preview_state().playlist_id, second);
    assert!(controller.delete_playlist(&second));
    assert_eq!(controller.preview_state().playlist_id, original);
    assert!(!controller.delete_playlist(&original));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn design_undo_never_rolls_back_the_live_queue() {
    let dir = demo_project();
    let controller = open(&dir);
    controller.queue_next();
    let called = controller.queue().current.expect("a row is called");
    assert!(!called.name.is_empty());

    // A design edit, then undo: the queue must stay as it was.
    let scene_id = controller.preview_scene().unwrap().id;
    controller.select_scene(&scene_id);
    controller.add_layer(LayerKind::Text(Default::default()));
    assert!(controller.can_undo());
    assert!(controller.undo());
    let after = controller.queue().current.expect("still called");
    assert_eq!(after.name, called.name);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn live_edit_pushes_changes_to_the_on_air_scene() {
    let dir = demo_project();
    let controller = open(&dir);
    let scene_id = controller.preview_scene().unwrap().id;
    controller.take_scene(&scene_id);
    controller.select_scene(&scene_id);
    controller.set_live_edit(true);

    let layer_id = controller.project_clone().scenes[0].layers[0].id.clone();
    controller.update_layer(&layer_id, Some("test-move"), |layer| {
        layer.rect = Rect::new(10.0, 10.0, 200.0, 200.0);
    });
    let program = controller.program();
    let updated = program
        .scene
        .as_ref()
        .unwrap()
        .layer(&layer_id)
        .unwrap()
        .rect;
    assert_eq!(updated.x, 10.0);

    // Live edit off keeps the program untouched.
    controller.set_live_edit(false);
    controller.update_layer(&layer_id, Some("test-move-2"), |layer| {
        layer.rect = Rect::new(300.0, 300.0, 200.0, 200.0);
    });
    let program = controller.program();
    let kept = program
        .scene
        .as_ref()
        .unwrap()
        .layer(&layer_id)
        .unwrap()
        .rect;
    assert_eq!(kept.x, 10.0);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn queue_and_timers_are_wired_through_the_controller() {
    let dir = demo_project();
    let controller = open(&dir);
    assert!(controller.queue_next());
    assert!(controller.queue().current.is_some());
    assert!(controller.queue_done());
    assert!(controller.queue().current.is_none());

    assert!(controller.timer_start("show"));
    let snapshot = controller
        .timer_snapshots()
        .into_iter()
        .find(|snapshot| snapshot.id == "show")
        .unwrap();
    assert!(snapshot.running);
    assert!(controller.timer_pause("show"));
    assert!(controller.timer_reset("show"));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn saving_mirrors_live_queue_state_into_the_document() {
    let dir = demo_project();
    let controller = open(&dir);
    controller.queue_next();
    let called = controller.queue().current.unwrap();
    controller.save_now().unwrap();
    let reloaded = project::load(&dir).unwrap().project;
    let row = reloaded
        .queue
        .rows
        .iter()
        .find(|row| row.id == called.id)
        .expect("row saved");
    assert_eq!(row.status, signifiage::model::QueueStatus::Called);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn attach_view_receives_scene_and_design() {
    let dir = demo_project();
    // SceneView is a GTK widget; initialise GTK once for this process.
    let _ = gtk4::init();
    let controller = open(&dir);
    let view = SceneView::new(signifiage::ui::ViewMode::Preview);
    controller.attach_view(ViewRole::Preview, &view);
    let scene = view.scene().expect("preview scene attached");
    assert_eq!(scene.name, "Welcome");
    let _ = gtk4::init();
    controller.set_black(true);
    controller.refresh_views();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn save_is_a_no_op_in_player_mode() {
    init_env();
    let dir = demo_project();
    let before = fs::read_to_string(dir.join("project.json")).unwrap();
    let controller = Controller::new(Config::default(), Mode::Player);
    controller.open_project(&dir).unwrap();
    controller.queue_next();
    controller.save_now().unwrap();
    let after = fs::read_to_string(dir.join("project.json")).unwrap();
    assert_eq!(before, after, "the player must not modify the project");
    fs::remove_dir_all(&dir).ok();
}
