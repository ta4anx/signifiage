//! Timer runtime and formatting tests.

use std::time::Duration;

use chrono::{Local, TimeZone};
use signifiage::model::{ClockPreset, TimerDef, TimerMode};
use signifiage::services::timers::{
    MISSING, TimerService, format_clock, format_date, format_end_at, format_eta, format_hms,
    format_remaining,
};

#[test]
fn formats_elapsed_and_remaining() {
    assert_eq!(format_hms(0), "00:00");
    assert_eq!(format_hms(65), "01:05");
    assert_eq!(format_hms(3725), "01:02:05");
    assert_eq!(format_hms(3599), "59:59");
    assert_eq!(format_hms(3600), "01:00:00");

    assert_eq!(format_remaining(0), "00:00");
    assert_eq!(format_remaining(65), "01:05");
    assert_eq!(format_remaining(3725), "01:02:05");
    assert_eq!(format_remaining(-5), "+00:05");
    assert_eq!(format_remaining(-65), "+01:05");
    assert_eq!(format_remaining(-3725), "+01:02:05");
}

#[test]
fn unknown_id_renders_missing() {
    let mut service = TimerService::new(&[]);
    assert_eq!(service.snapshot("nope"), None);
    assert_eq!(service.elapsed("nope"), None);
    assert!(!service.start("nope"));
    assert!(!service.pause("nope"));
    assert!(!service.toggle("nope"));
    assert!(!service.reset("nope"));
    assert!(!service.is_running("nope"));
    assert!(service.all().is_empty());
    for mode in [TimerMode::Elapsed, TimerMode::Remaining, TimerMode::EndAt] {
        assert_eq!(service.text_for_layer("nope", mode), MISSING);
    }
}

#[test]
fn start_pause_toggle_reset_transitions() {
    let defs = [TimerDef::new("speaker", "Speaker", Some(600))];
    let mut service = TimerService::new(&defs);
    let id = "speaker";

    assert!(!service.is_running(id));
    assert!(service.start(id));
    assert!(service.is_running(id));
    assert!(!service.start(id), "starting a running timer is a no-op");

    assert!(service.toggle(id));
    assert!(!service.is_running(id));
    assert!(!service.pause(id), "pausing a stopped timer is a no-op");

    assert!(service.toggle(id));
    assert!(service.is_running(id));

    assert!(service.pause(id));
    assert!(!service.is_running(id));
    assert!(service.reset(id));
    assert!(!service.is_running(id));

    let snapshot = service.snapshot(id).expect("known timer");
    assert_eq!(snapshot.elapsed_secs, 0);
    assert_eq!(snapshot.remaining_secs, Some(600));
    assert_eq!(snapshot.name, "Speaker");
    assert_eq!(snapshot.id, id);
}

#[test]
fn count_up_timer_has_no_remaining_or_end_at() {
    let defs = [TimerDef::new("show", "Show", None)];
    let mut service = TimerService::new(&defs);
    assert!(service.start("show"));
    std::thread::sleep(Duration::from_millis(3));

    let bundle = service.elapsed("show").expect("known timer");
    let snapshot = service.snapshot("show").expect("known timer");
    assert_eq!(snapshot.elapsed_secs, bundle.as_secs() as i64);
    assert_eq!(snapshot.remaining_secs, None);
    assert_eq!(snapshot.end_at, None);
    assert!(snapshot.running);

    assert_eq!(
        service.text_for_layer("show", TimerMode::Elapsed),
        format_hms(bundle.as_secs() as i64)
    );
    assert_eq!(
        service.text_for_layer("show", TimerMode::Remaining),
        MISSING
    );
    assert_eq!(service.text_for_layer("show", TimerMode::EndAt), MISSING);
}

#[test]
fn countdown_reports_remaining_and_end_at() {
    let defs = [TimerDef::new("speaker", "Speaker", Some(600))];
    let mut service = TimerService::new(&defs);
    assert!(service.start("speaker"));
    std::thread::sleep(Duration::from_millis(3));

    let bundle = service.elapsed("speaker").expect("known timer");
    let snapshot = service.snapshot("speaker").expect("known timer");
    assert_eq!(snapshot.elapsed_secs, bundle.as_secs() as i64);
    assert_eq!(snapshot.remaining_secs, Some(600 - bundle.as_secs() as i64));
    assert_eq!(
        snapshot.end_at.map(format_end_at),
        Some(format_end_at(
            Local::now() + chrono::Duration::seconds(snapshot.remaining_secs.unwrap())
        ))
    );

    assert_eq!(
        service.text_for_layer("speaker", TimerMode::Elapsed),
        format_hms(snapshot.elapsed_secs)
    );
    assert_eq!(
        service.text_for_layer("speaker", TimerMode::Remaining),
        format_hms(snapshot.remaining_secs.unwrap())
    );
    assert_eq!(
        service.text_for_layer("speaker", TimerMode::EndAt),
        format_end_at(snapshot.end_at.unwrap())
    );

    assert!(service.pause("speaker"));
    let paused = service.snapshot("speaker").expect("known timer");
    assert!(!paused.running);
    assert_eq!(paused.remaining_secs, snapshot.remaining_secs);
}

#[test]
fn sync_definitions_preserves_runtime_and_updates_duration() {
    let defs = [TimerDef::new("show", "Show", None)];
    let mut service = TimerService::new(&defs);
    assert!(service.start("show"));
    std::thread::sleep(Duration::from_millis(3));
    let elapsed_before = service.elapsed("show").expect("known timer");
    assert!(elapsed_before > Duration::ZERO);

    // Becoming a countdown keeps the elapsed runtime and stays running.
    service.sync_definitions(&[TimerDef::new("show", "Main show", Some(600))]);
    let snapshot = service.snapshot("show").expect("survivor");
    assert!(snapshot.running);
    assert_eq!(snapshot.name, "Main show");
    assert!(snapshot.elapsed_secs >= elapsed_before.as_secs() as i64);
    assert!(snapshot.remaining_secs.is_some());
    assert!(service.elapsed("show").unwrap() >= elapsed_before);

    // A shorter duration clamps the elapsed time.
    service.sync_definitions(&[TimerDef::new("show", "Main show", Some(0))]);
    let clamped = service.snapshot("show").expect("survivor");
    assert_eq!(clamped.elapsed_secs, 0);
    assert_eq!(clamped.remaining_secs, Some(0));
    assert!(clamped.running, "running survives a duration change");
}

#[test]
fn sync_definitions_adds_and_drops_timers() {
    let mut service = TimerService::new(&[TimerDef::new("a", "A", None)]);
    assert!(service.start("a"));

    service.sync_definitions(&[
        TimerDef::new("a", "A", Some(30)),
        TimerDef::new("b", "B", None),
    ]);
    assert!(service.is_running("a"));
    assert!(service.snapshot("b").is_some());
    assert!(!service.is_running("b"));
    assert_eq!(service.all().len(), 2);

    service.sync_definitions(&[TimerDef::new("b", "B", None)]);
    assert!(service.snapshot("a").is_none());
    assert!(service.snapshot("b").is_some());
    assert_eq!(service.all().len(), 1);
}

#[test]
fn formats_clock_presets() {
    let now = Local
        .with_ymd_and_hms(2026, 11, 17, 14, 5, 9)
        .single()
        .expect("valid local time");

    assert_eq!(format_clock(ClockPreset::H24, now), "14:05");
    assert_eq!(format_clock(ClockPreset::H24Seconds, now), "14:05:09");
    assert_eq!(format_clock(ClockPreset::H12, now), "02:05 PM");
    assert_eq!(format_clock(ClockPreset::H12Seconds, now), "02:05:09 PM");
    assert_eq!(format_date(now), "17.11.2026");
    assert_eq!(format_end_at(now), "14:05");
}

#[test]
fn formats_twelve_hour_midnight_and_noon() {
    let midnight = Local
        .with_ymd_and_hms(2026, 1, 2, 0, 0, 0)
        .single()
        .expect("valid local time");
    let noon = Local
        .with_ymd_and_hms(2026, 1, 2, 12, 0, 0)
        .single()
        .expect("valid local time");
    assert_eq!(format_clock(ClockPreset::H12, midnight), "12:00 AM");
    assert_eq!(format_clock(ClockPreset::H12, noon), "12:00 PM");
    assert_eq!(format_date(midnight), "02.01.2026");
}

#[test]
fn formats_eta() {
    assert_eq!(format_eta(Some(0)), "00:00");
    assert_eq!(format_eta(Some(65)), "01:05");
    assert_eq!(format_eta(Some(3725)), "01:02:05");
    assert_eq!(format_eta(None), MISSING);
    assert_eq!(format_eta(Some(-5)), MISSING);
}
