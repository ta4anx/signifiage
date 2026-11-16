//! Player schedule evaluation: weekday/time windows, first match, transitions.

use chrono::{TimeZone, Timelike, Weekday};

use signifiage::model::Schedule;
use signifiage::model::playlist::PlaylistItem;
use signifiage::model::project::{Project, Template};
use signifiage::services::schedule::{POLL_INTERVAL, ScheduleRuntime, is_playable, playlist_for};

fn at(weekday: Weekday, hour: u32, minute: u32) -> chrono::DateTime<chrono::Local> {
    // 2026-03-02 is a Monday.
    let base = chrono::Local
        .with_ymd_and_hms(2026, 3, 2, 0, 0, 0)
        .single()
        .expect("valid local time");
    let offset = weekday.num_days_from_monday() as i64;
    (base + chrono::Duration::days(offset))
        .with_hour(hour)
        .and_then(|value| value.with_minute(minute))
        .and_then(|value| value.with_second(0))
        .and_then(|value| value.with_nanosecond(0))
        .expect("valid time")
}

fn project_with_schedules() -> Project {
    let mut project = Project::from_template(Template::Blank, "Schedules");
    let scene_id = project.scenes[0].id.clone();
    let mut evening = project.playlists[0].clone();
    evening.id = "pl-evening".to_string();
    evening.name = "Evening".to_string();
    evening.items = vec![PlaylistItem::new(scene_id, 0)];
    project.playlists.push(evening);
    project.schedules = vec![
        Schedule {
            playlist_id: project.playlists[0].id.clone(),
            days: vec![0, 1, 2, 3, 4],
            start: "09:00".into(),
            end: "17:00".into(),
        },
        Schedule {
            playlist_id: "pl-evening".into(),
            days: vec![0, 1, 2, 3, 4, 5, 6],
            start: "17:00".into(),
            end: "22:00".into(),
        },
    ];
    project
}

#[test]
fn daytime_and_evening_windows_resolve_in_order() {
    let project = project_with_schedules();
    let daytime = at(Weekday::Mon, 10, 0);
    assert_eq!(
        ScheduleRuntime::current_playlist(&project, daytime),
        Some(project.playlists[0].id.clone())
    );

    let evening = at(Weekday::Sat, 18, 30);
    assert_eq!(
        ScheduleRuntime::current_playlist(&project, evening),
        Some("pl-evening".to_string())
    );

    // Outside every window the default playlist is used.
    let late = at(Weekday::Sun, 23, 0);
    assert_eq!(
        ScheduleRuntime::current_playlist(&project, late),
        Some(project.default_playlist_id.clone())
    );
}

#[test]
fn boundaries_are_start_inclusive_and_end_exclusive() {
    let project = project_with_schedules();
    let start = at(Weekday::Tue, 9, 0);
    assert_eq!(
        ScheduleRuntime::current_playlist(&project, start),
        Some(project.playlists[0].id.clone())
    );
    // 17:00 is the first excluded minute of the morning window and the first
    // included minute of the evening window.
    let boundary = at(Weekday::Tue, 17, 0);
    assert_eq!(
        ScheduleRuntime::current_playlist(&project, boundary),
        Some("pl-evening".to_string())
    );
}

#[test]
fn runtime_only_reports_transitions() {
    let project = project_with_schedules();
    let mut runtime = ScheduleRuntime::new();
    assert_eq!(
        runtime.poll(&project, at(Weekday::Mon, 10, 0)),
        Some(project.playlists[0].id.clone())
    );
    assert_eq!(runtime.poll(&project, at(Weekday::Mon, 10, 5)), None);
    assert_eq!(
        runtime.poll(&project, at(Weekday::Mon, 18, 0)),
        Some("pl-evening".to_string())
    );
    assert_eq!(
        runtime.poll(&project, at(Weekday::Mon, 23, 30)),
        Some(project.default_playlist_id.clone())
    );
}

#[test]
fn leaving_a_window_switches_to_the_default_playlist() {
    let project = project_with_schedules();
    let mut runtime = ScheduleRuntime::new();
    runtime.poll(&project, at(Weekday::Sun, 18, 0));
    assert_eq!(
        runtime.poll(&project, at(Weekday::Sun, 22, 30)),
        Some(project.default_playlist_id.clone())
    );
}

#[test]
fn invalid_targets_fall_back_to_the_default_playlist() {
    let mut project = project_with_schedules();
    project.schedules[0].playlist_id = "missing".to_string();
    assert_eq!(
        ScheduleRuntime::current_playlist(&project, at(Weekday::Mon, 10, 0)),
        Some(project.default_playlist_id.clone())
    );
    assert_eq!(
        playlist_for(&project, None),
        Some(project.default_playlist_id.clone())
    );

    project.schedules.clear();
    assert_eq!(
        ScheduleRuntime::current_playlist(&project, at(Weekday::Mon, 10, 0)),
        Some(project.default_playlist_id.clone())
    );
}

#[test]
fn empty_playlists_are_not_playable() {
    let mut project = project_with_schedules();
    let first = project.playlists[0].id.clone();
    assert!(is_playable(&project, &first));
    project.playlists[0].items.clear();
    assert!(!is_playable(&project, &first));
    assert!(!is_playable(&project, "missing"));
}

#[test]
fn poll_interval_gates_evaluation() {
    let mut runtime = ScheduleRuntime::new();
    let now = std::time::Instant::now();
    assert!(runtime.due(now), "a fresh runtime is due immediately");
    runtime.reset();
    assert!(runtime.due(std::time::Instant::now()));
    assert!(POLL_INTERVAL.as_secs() >= 30);
}
