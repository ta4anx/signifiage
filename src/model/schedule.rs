//! Weekday/time schedule windows evaluated by the player.

use serde::{Deserialize, Serialize};

/// `days` uses 0 = Monday … 6 = Sunday.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub playlist_id: String,
    pub days: Vec<u8>,
    /// Local start time `HH:MM`, inclusive.
    pub start: String,
    /// Local end time `HH:MM`, exclusive.
    pub end: String,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            playlist_id: String::new(),
            days: vec![0, 1, 2, 3, 4],
            start: "09:00".to_string(),
            end: "17:00".to_string(),
        }
    }
}

/// Parse `HH:MM` into minutes since midnight.
pub fn parse_hhmm(value: &str) -> Option<u32> {
    let (h, m) = value.trim().split_once(':')?;
    if h.len() > 2 || m.len() != 2 {
        return None;
    }
    let hours: u32 = h.parse().ok()?;
    let minutes: u32 = m.parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(hours * 60 + minutes)
}

/// Format minutes since midnight as `HH:MM`.
pub fn format_hhmm(minutes: u32) -> String {
    format!("{:02}:{:02}", (minutes / 60) % 24, minutes % 60)
}

impl Schedule {
    pub fn start_minutes(&self) -> Option<u32> {
        parse_hhmm(&self.start)
    }

    pub fn end_minutes(&self) -> Option<u32> {
        parse_hhmm(&self.end)
    }

    /// A window is valid when both times parse and start < end (no overnight).
    pub fn is_valid(&self) -> bool {
        match (self.start_minutes(), self.end_minutes()) {
            (Some(start), Some(end)) => start < end,
            _ => false,
        }
    }

    pub fn day_enabled(&self, weekday: u8) -> bool {
        weekday < 7 && self.days.contains(&weekday)
    }

    /// True when `weekday`/`minutes` falls inside this window.
    pub fn matches(&self, weekday: u8, minutes: u32) -> bool {
        if !self.day_enabled(weekday) {
            return false;
        }
        match (self.start_minutes(), self.end_minutes()) {
            (Some(start), Some(end)) => minutes >= start && minutes < end,
            _ => false,
        }
    }

    /// Localised weekday short names, Monday first.
    pub fn weekday_names() -> [&'static str; 7] {
        [
            "schedule.mon",
            "schedule.tue",
            "schedule.wed",
            "schedule.thu",
            "schedule.fri",
            "schedule.sat",
            "schedule.sun",
        ]
    }
}

/// Weekday index (0 = Monday) for a `chrono` weekday.
pub fn weekday_index(weekday: chrono::Weekday) -> u8 {
    weekday.num_days_from_monday() as u8
}

/// First schedule matching the given local time, in list order.
pub fn active_schedule(schedules: &[Schedule], weekday: u8, minutes: u32) -> Option<&Schedule> {
    schedules.iter().find(|s| s.matches(weekday, minutes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats_times() {
        assert_eq!(parse_hhmm("09:00"), Some(540));
        assert_eq!(parse_hhmm("9:00"), Some(540));
        assert_eq!(parse_hhmm("23:59"), Some(1439));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("10:60"), None);
        assert_eq!(parse_hhmm("noon"), None);
        assert_eq!(format_hhmm(540), "09:00");
        assert_eq!(format_hhmm(0), "00:00");
    }

    #[test]
    fn window_bounds_are_start_inclusive_end_exclusive() {
        let s = Schedule {
            playlist_id: "p".into(),
            days: vec![0, 1, 2, 3, 4],
            start: "09:00".into(),
            end: "17:00".into(),
        };
        assert!(s.is_valid());
        assert!(s.matches(0, 540));
        assert!(s.matches(4, 1019));
        assert!(!s.matches(0, 1020));
        assert!(!s.matches(5, 600));
        assert!(!s.matches(7, 600));
    }

    #[test]
    fn overnight_windows_are_invalid() {
        let s = Schedule {
            start: "22:00".into(),
            end: "02:00".into(),
            ..Schedule::default()
        };
        assert!(!s.is_valid());
        assert!(!s.matches(0, 23 * 60));
    }

    #[test]
    fn first_matching_schedule_wins() {
        let morning = Schedule {
            playlist_id: "morning".into(),
            days: vec![0],
            start: "08:00".into(),
            end: "12:00".into(),
        };
        let all_day = Schedule {
            playlist_id: "all".into(),
            days: vec![0],
            start: "00:00".into(),
            end: "23:59".into(),
        };
        let schedules = vec![morning, all_day];
        let found = active_schedule(&schedules, 0, 600).unwrap();
        assert_eq!(found.playlist_id, "morning");
        let found = active_schedule(&schedules, 0, 13 * 60).unwrap();
        assert_eq!(found.playlist_id, "all");
        assert!(active_schedule(&schedules, 2, 600).is_none());
    }

    #[test]
    fn weekday_index_matches_plan_convention() {
        assert_eq!(weekday_index(chrono::Weekday::Mon), 0);
        assert_eq!(weekday_index(chrono::Weekday::Sun), 6);
    }
}
