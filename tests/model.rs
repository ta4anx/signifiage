//! Model integration tests: schema defaults, persistence, validation.

use std::fs;
use std::path::PathBuf;

use signifiage::model::layer::{LayerKind, Rect};
use signifiage::model::playlist::PlaylistItem;
use signifiage::model::project::{self, Project, Template};
use signifiage::model::schedule::Schedule;
use signifiage::model::{QueueStatus, new_id};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("signifiage-model-{tag}-{}", new_id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn minimal_project_json_uses_defaults() {
    let project: Project = serde_json::from_str("{}").unwrap();
    assert_eq!(project.format, 1);
    assert_eq!(project.design.width, 1920);
    assert_eq!(project.design.height, 1080);
    assert_eq!(
        project.presentation.scale,
        signifiage::model::OutputScale::Fit
    );
    assert_eq!(project.presentation.rotation, 0);
    assert!(project.queue.rows.is_empty());
    assert_eq!(project.queue.next_ticket, 1);
    assert_eq!(project.soundboard.pages.len(), 3);
    assert_eq!(project.prompter.font_size, 64.0);
    assert_eq!(project.deck.next_key, "right");
}

#[test]
fn stages_template_is_playable_and_round_trips() {
    let project = Project::from_template(Template::StageShow, "Stage 2026");
    assert!(project.scenes.len() >= 2);
    assert!(project.playlists[0].items.len() >= 2);
    assert_eq!(project.playlists[0].id, project.default_playlist_id);
    assert!(
        project.scenes[0]
            .layers
            .iter()
            .any(|l| matches!(l.kind, LayerKind::Text(_)))
    );
    assert!(
        project.scenes[0]
            .layers
            .iter()
            .any(|l| matches!(l.kind, LayerKind::Clock(_)))
    );
    assert!(project.scenes[1].has_capture());

    let json = serde_json::to_string(&project).unwrap();
    let back: Project = serde_json::from_str(&json).unwrap();
    assert_eq!(back, project);
}

#[test]
fn save_load_preserves_queue_and_timers() {
    let dir = temp_dir("roundtrip");
    let mut project = Project::from_template(Template::QueueBoard, "Board");
    project.queue.rows.push(Default::default());
    project.queue.rows[0].name = "Ayşe Yılmaz".into();
    project.queue.rows[0].ticket = "001".into();
    project.queue.rows[0].status = QueueStatus::Waiting;
    project.timers[0].duration_secs = Some(300);

    project::create(&project, &dir).unwrap();
    let loaded = project::load(&dir).unwrap();
    assert_eq!(loaded.project.queue.rows[0].name, "Ayşe Yılmaz");
    assert_eq!(loaded.project.timers[0].duration_secs, Some(300));
    assert!(!loaded.used_backup);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn load_warns_and_drops_dangling_references() {
    let dir = temp_dir("dangling");
    let mut project = Project::from_template(Template::Blank, "Dangling");
    let playlist_id = project.playlists[0].id.clone();
    project.playlists[0]
        .items
        .push(PlaylistItem::new("ghost", 3));
    project.schedules.push(Schedule {
        playlist_id,
        days: vec![0, 1],
        start: "08:00".into(),
        end: "12:00".into(),
    });
    project.scenes[0].layers.push(signifiage::model::Layer::new(
        LayerKind::Image(Default::default()),
        "img",
        Rect::new(0.0, 0.0, 100.0, 100.0),
    ));
    if let LayerKind::Image(props) = &mut project.scenes[0].layers[0].kind {
        props.asset = Some("../../etc/passwd".into());
    }
    project::save(&project, &dir).unwrap();

    let loaded = project::load(&dir).unwrap();
    assert_eq!(loaded.project.playlists[0].items.len(), 1);
    assert_eq!(loaded.project.schedules.len(), 1);
    match &loaded.project.scenes[0].layers[0].kind {
        LayerKind::Image(props) => assert!(props.asset.is_none()),
        other => panic!("unexpected {other:?}"),
    }
    assert!(
        loaded.warnings.len() >= 2,
        "warnings: {:?}",
        loaded.warnings
    );
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn asset_references_must_be_relative_and_contained() {
    assert!(project::asset_reference_ok(None, "assets/logo.png"));
    for bad in [
        "/usr/share/logo.png",
        "../logo.png",
        "assets/../../logo.png",
        "assets/..",
        "",
        "  ",
    ] {
        assert!(!project::asset_reference_ok(None, bad), "{bad}");
    }
}

#[test]
fn symlinked_asset_outside_project_is_rejected() {
    let dir = temp_dir("symlink");
    let outside = temp_dir("outside");
    fs::create_dir_all(dir.join("assets")).unwrap();
    let target = outside.join("secret.png");
    fs::write(&target, b"png").unwrap();
    let link = dir.join("assets/secret.png");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(!project::asset_reference_ok(
        Some(&dir),
        "assets/secret.png"
    ));

    // A regular file inside assets/ is fine.
    fs::write(dir.join("assets/ok.png"), b"png").unwrap();
    assert!(project::asset_reference_ok(Some(&dir), "assets/ok.png"));
    fs::remove_dir_all(&dir).ok();
    fs::remove_dir_all(&outside).ok();
}

#[test]
fn backup_is_used_when_primary_json_is_broken() {
    let dir = temp_dir("backup");
    let mut project = Project::from_template(Template::Blank, "First");
    project::create(&project, &dir).unwrap();
    project.name = "Second".into();
    project::save(&project, &dir).unwrap();

    fs::write(dir.join("project.json"), b"{ broken").unwrap();
    let loaded = project::load(&dir).unwrap();
    assert!(loaded.used_backup);
    assert_eq!(loaded.project.name, "First");
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn unknown_top_level_fields_are_ignored() {
    let raw = r##"{
        "format": 1,
        "name": "Future",
        "future_field": {"nested": true},
        "design": {"width": 1280, "height": 720, "background": "#000000", "grid": 5, "safe_area": false},
        "scenes": [], "playlists": [], "default_playlist_id": ""
    }"##;
    let mut project: Project = serde_json::from_str(raw).unwrap();
    project.normalize(None);
    assert_eq!(project.design.width, 1280);
    assert_eq!(project.playlists.len(), 1);
    assert_eq!(project.playlists[0].id, project.default_playlist_id);
}

#[test]
fn delete_scene_semantics_keep_project_consistent() {
    let mut project = Project::from_template(Template::StageShow, "Del");
    let victim = project.scenes[0].id.clone();
    project.scenes.retain(|s| s.id != victim);
    for playlist in &mut project.playlists {
        playlist.remove_scene(&victim);
    }
    project.normalize(None);
    assert!(project.scenes.iter().all(|s| s.id != victim));
    assert!(
        project.playlists[0]
            .items
            .iter()
            .all(|i| i.scene_id != victim)
    );
}

#[test]
fn demo_fixture_loads_with_queue_rows() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo-project");
    let loaded = project::load(&fixture).expect("demo fixture must load");
    let project = loaded.project;
    assert!(project.has_queue_scene(), "demo needs a queue scene");
    assert_eq!(project.queue.rows.len(), 3);
    assert!(
        project
            .queue
            .rows
            .iter()
            .all(|row| row.status == QueueStatus::Waiting),
        "fixture rows start waiting"
    );
    assert!(project.playlists.iter().any(|p| p.items.len() >= 2));
    assert_eq!(project.name, "Demo Show");
    for scene in &project.scenes {
        for layer in &scene.layers {
            if let Some(asset) = layer.kind.asset() {
                assert!(
                    fixture.join(asset).is_file(),
                    "missing fixture asset {asset}"
                );
            }
        }
    }
}
