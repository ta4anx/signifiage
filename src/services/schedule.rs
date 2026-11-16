//! Runtime schedule evaluation for the standalone player.
//!
//! The persisted windows live in [`crate::model::schedule`]; this module owns the
//! live decision: which playlist should be on air right now, and whether that
//! answer changed since the previous poll. The player polls every 30 s and only
//! restarts playback when the *active window* changes (entering, leaving or
//! moving to another schedule), so manual transport stays usable in between.

use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, Local, Timelike};

use crate::model::project::Project;

/// How often the player re-evaluates the schedule.
pub const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Tracks the active schedule across polls.
#[derive(Debug, Default)]
pub struct ScheduleRuntime {
    /// Index into `Project::schedules` of the window that is on air, if any.
    active: Option<usize>,
    initialized: bool,
    last_check: Option<Instant>,
}

impl ScheduleRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    /// Index of the schedule window that was active at the last poll.
    pub fn active_index(&self) -> Option<usize> {
        self.active
    }

    /// Forget the previous decision; the next poll reports a target again.
    pub fn reset(&mut self) {
        self.active = None;
        self.initialized = false;
        self.last_check = None;
    }

    /// True when [`POLL_INTERVAL`] elapsed since the last poll (or never polled).
    pub fn due(&self, now: Instant) -> bool {
        match self.last_check {
            None => true,
            Some(at) => now.saturating_duration_since(at) >= POLL_INTERVAL,
        }
    }

    /// Evaluate the schedule at `now`.
    ///
    /// Returns `Some(playlist_id)` only when the active window changed (or this
    /// is the first poll). The playlist is guaranteed to exist and contain at
    /// least one item; when the window's target is invalid the default playlist
    /// is returned instead. `None` means "keep playing what is on air" or "this
    /// project has no playable playlist at all".
    pub fn poll(&mut self, project: &Project, now: DateTime<Local>) -> Option<String> {
        self.last_check = Some(Instant::now());
        let weekday = crate::model::schedule::weekday_index(now.weekday());
        let minutes = now.hour() * 60 + now.minute();
        let index = project
            .schedules
            .iter()
            .position(|schedule| schedule.matches(weekday, minutes));

        if self.initialized && index == self.active {
            return None;
        }
        self.active = index;
        self.initialized = true;
        playlist_for(project, index)
    }

    /// Target playlist without changing the tracked window; used at startup and
    /// by callers that need the current answer only.
    pub fn current_playlist(project: &Project, now: DateTime<Local>) -> Option<String> {
        let weekday = crate::model::schedule::weekday_index(now.weekday());
        let minutes = now.hour() * 60 + now.minute();
        let index = project
            .schedules
            .iter()
            .position(|schedule| schedule.matches(weekday, minutes));
        playlist_for(project, index)
    }
}

/// Resolve the playlist for a window: the schedule target when it is playable,
/// otherwise the project default.
pub fn playlist_for(project: &Project, index: Option<usize>) -> Option<String> {
    if let Some(schedule) = index.and_then(|index| project.schedules.get(index))
        && is_playable(project, &schedule.playlist_id)
    {
        return Some(schedule.playlist_id.clone());
    }
    if is_playable(project, &project.default_playlist_id) {
        return Some(project.default_playlist_id.clone());
    }
    None
}

/// A playlist can go on air when it exists and has at least one item.
pub fn is_playable(project: &Project, playlist_id: &str) -> bool {
    project
        .playlist(playlist_id)
        .is_some_and(|playlist| !playlist.items.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::schedule::Schedule;
    use crate::model::{Playlist, PlaylistItem};

    fn project() -> Project {
        let mut project = Project::default();
        let mut show = Playlist::new("Show");
        show.id = "show".into();
        show.items.push(PlaylistItem::new("scene-a", 0));
        let mut morning = Playlist::new("Morning");
        morning.id = "morning".into();
        morning.items.push(PlaylistItem::new("scene-b", 0));
        let mut empty = Playlist::new("Empty");
        empty.id = "empty".into();
        project.playlists = vec![show, morning, empty];
        project.default_playlist_id = "show".into();
        project
    }

    fn schedule(playlist: &str, days: Vec<u8>, start: &str, end: &str) -> Schedule {
        Schedule {
            playlist_id: playlist.into(),
            days,
            start: start.into(),
            end: end.into(),
        }
    }

    fn at(project: &Project, weekday: chrono::Weekday, hour: u32, minute: u32) -> DateTime<Local> {
        let _ = project;
        use chrono::TimeZone;
        let base = Local
            .with_ymd_and_hms(2026, 11, 16, hour, minute, 0) // Monday
            .single()
            .unwrap();
        let shift = (weekday.num_days_from_monday() as i64
            - chrono::Weekday::Mon.num_days_from_monday() as i64)
            * 86_400;
        base + chrono::Duration::seconds(shift)
    }

    #[test]
    fn first_poll_reports_default_without_windows() {
        let project = project();
        let mut runtime = ScheduleRuntime::new();
        let now = at(&project, chrono::Weekday::Tue, 12, 0);
        assert_eq!(runtime.poll(&project, now), Some("show".to_string()));
        assert!(runtime.poll(&project, now).is_none());
        assert_eq!(runtime.active_index(), None);
    }

    #[test]
    fn entering_and_leaving_a_window_switches_playlists() {
        let mut project = project();
        project.schedules = vec![schedule("morning", vec![0, 1, 2, 3, 4], "09:00", "12:00")];
        let mut runtime = ScheduleRuntime::new();
        let monday = chrono::Weekday::Mon;
        assert_eq!(
            runtime.poll(&project, at(&project, monday, 10, 0)),
            Some("morning".to_string())
        );
        assert_eq!(runtime.active_index(), Some(0));
        assert!(
            runtime
                .poll(&project, at(&project, monday, 11, 59))
                .is_none()
        );
        // Leaving the window falls back to the default playlist.
        assert_eq!(
            runtime.poll(&project, at(&project, monday, 12, 0)),
            Some("show".to_string())
        );
        assert_eq!(runtime.active_index(), None);
    }

    #[test]
    fn start_is_inclusive_and_end_is_exclusive() {
        let mut project = project();
        project.schedules = vec![schedule("morning", vec![0], "09:00", "10:00")];
        let monday = chrono::Weekday::Mon;
        let mut runtime = ScheduleRuntime::new();
        assert_eq!(
            runtime.poll(&project, at(&project, monday, 9, 0)),
            Some("morning".to_string())
        );
        runtime.reset();
        assert_eq!(
            runtime.poll(&project, at(&project, monday, 10, 0)),
            Some("show".to_string())
        );
    }

    #[test]
    fn sunday_matches_index_six() {
        let mut project = project();
        project.schedules = vec![schedule("morning", vec![6], "00:00", "23:59")];
        let mut runtime = ScheduleRuntime::new();
        assert_eq!(
            runtime.poll(&project, at(&project, chrono::Weekday::Sun, 12, 0)),
            Some("morning".to_string())
        );
    }

    #[test]
    fn first_matching_window_wins() {
        let mut project = project();
        project.schedules = vec![
            schedule("show", vec![0], "08:00", "18:00"),
            schedule("morning", vec![0], "09:00", "12:00"),
        ];
        let mut runtime = ScheduleRuntime::new();
        assert_eq!(
            runtime.poll(&project, at(&project, chrono::Weekday::Mon, 10, 0)),
            Some("show".to_string())
        );
        assert_eq!(runtime.active_index(), Some(0));
    }

    #[test]
    fn invalid_time_order_is_ignored() {
        let mut project = project();
        project.schedules = vec![schedule("morning", vec![0], "12:00", "09:00")];
        let mut runtime = ScheduleRuntime::new();
        assert_eq!(
            runtime.poll(&project, at(&project, chrono::Weekday::Mon, 10, 0)),
            Some("show".to_string())
        );
    }

    #[test]
    fn missing_or_empty_target_falls_back_to_default() {
        let mut project = project();
        project.schedules = vec![schedule("empty", vec![0], "09:00", "12:00")];
        let mut runtime = ScheduleRuntime::new();
        assert_eq!(
            runtime.poll(&project, at(&project, chrono::Weekday::Mon, 10, 0)),
            Some("show".to_string())
        );
        project.schedules[0].playlist_id = "does-not-exist".into();
        runtime.reset();
        assert_eq!(
            runtime.poll(&project, at(&project, chrono::Weekday::Mon, 10, 0)),
            Some("show".to_string())
        );
    }

    #[test]
    fn no_playable_playlist_reports_nothing() {
        let mut project = project();
        project.playlists.clear();
        let mut runtime = ScheduleRuntime::new();
        assert_eq!(
            runtime.poll(&project, at(&project, chrono::Weekday::Mon, 10, 0)),
            None
        );
    }

    #[test]
    fn due_after_interval() {
        let runtime = ScheduleRuntime::new();
        assert!(runtime.due(Instant::now()));
        let mut runtime = ScheduleRuntime::new();
        runtime.last_check = Some(Instant::now());
        assert!(!runtime.due(Instant::now()));
        runtime.last_check = Some(Instant::now() - POLL_INTERVAL);
        assert!(runtime.due(Instant::now()));
    }

    #[test]
    fn current_playlist_does_not_track_state() {
        let mut project = project();
        project.schedules = vec![schedule("morning", vec![0], "09:00", "12:00")];
        assert_eq!(
            ScheduleRuntime::current_playlist(&project, at(&project, chrono::Weekday::Mon, 10, 0)),
            Some("morning".to_string())
        );
        assert_eq!(
            ScheduleRuntime::current_playlist(&project, at(&project, chrono::Weekday::Mon, 13, 0)),
            Some("show".to_string())
        );
    }
}
