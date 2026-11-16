//! Timer runtime and formatting.
//!
//! Timers are plain data: runtime state is derived on read from monotonic
//! [`Instant`]s, so there is no background thread and no GTK dependency. The
//! service is driven by the UI, which simply reads [`TimerService::all`] or
//! [`TimerService::text_for_layer`] when rendering a frame.

use std::time::{Duration, Instant};

use chrono::{DateTime, Local};

use crate::model::{ClockPreset, TimerDef, TimerMode};

/// Placeholder drawn when a timer id no longer exists.
pub const MISSING: &str = "--:--";

/// Read-only view of one timer, used by the renderer, the Run page and the web
/// remote snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct TimerSnapshot {
    pub id: String,
    pub name: String,
    pub running: bool,
    pub elapsed_secs: i64,
    pub remaining_secs: Option<i64>,
    pub end_at: Option<DateTime<Local>>,
}

struct TimerRuntime {
    id: String,
    name: String,
    duration: Option<Duration>,
    started_at: Option<Instant>,
    accumulated: Duration,
    running: bool,
}

impl TimerRuntime {
    fn from_def(def: &TimerDef) -> Self {
        Self {
            id: def.id.clone(),
            name: def.name.clone(),
            duration: def.duration_secs.map(|s| Duration::from_secs(s as u64)),
            started_at: None,
            accumulated: Duration::ZERO,
            running: false,
        }
    }

    fn elapsed(&self) -> Duration {
        let live = self
            .started_at
            .map(|at| at.elapsed())
            .unwrap_or(Duration::ZERO);
        self.accumulated + live
    }

    fn snapshot(&self) -> TimerSnapshot {
        let elapsed = self.elapsed();
        let elapsed_secs = elapsed.as_secs().min(i64::MAX as u64) as i64;
        let remaining_secs = self.duration.map(|duration| {
            let duration_secs = duration.as_secs();
            let elapsed_secs = elapsed.as_secs();
            if elapsed_secs <= duration_secs {
                (duration_secs - elapsed_secs) as i64
            } else {
                -((elapsed_secs - duration_secs) as i64)
            }
        });
        let end_at = remaining_secs.map(|secs| Local::now() + chrono::Duration::seconds(secs));
        TimerSnapshot {
            id: self.id.clone(),
            name: self.name.clone(),
            running: self.running,
            elapsed_secs,
            remaining_secs,
            end_at,
        }
    }
}

/// Per-project timer runtime.
pub struct TimerService {
    timers: Vec<TimerRuntime>,
}

impl TimerService {
    pub fn new(defs: &[TimerDef]) -> Self {
        let mut timers: Vec<TimerRuntime> = Vec::with_capacity(defs.len());
        for def in defs {
            if timers.iter().any(|timer| timer.id == def.id) {
                continue;
            }
            timers.push(TimerRuntime::from_def(def));
        }
        Self { timers }
    }

    /// Reconcile with the persisted definitions: add new timers, drop deleted
    /// ones and keep the runtime of survivors. A duration change clamps the
    /// elapsed time to the new duration.
    pub fn sync_definitions(&mut self, defs: &[TimerDef]) {
        let mut next: Vec<TimerRuntime> = Vec::with_capacity(defs.len());
        for def in defs {
            if next.iter().any(|timer| timer.id == def.id) {
                continue;
            }
            let duration = def.duration_secs.map(|s| Duration::from_secs(s as u64));
            match self.timers.iter().position(|timer| timer.id == def.id) {
                Some(index) => {
                    let mut timer = self.timers.remove(index);
                    timer.name = def.name.clone();
                    if timer.duration != duration {
                        let elapsed = timer.elapsed();
                        let clamped = match duration {
                            Some(duration) => elapsed.min(duration),
                            None => elapsed,
                        };
                        timer.accumulated = clamped;
                        timer.started_at = timer.running.then(Instant::now);
                        timer.duration = duration;
                    }
                    next.push(timer);
                }
                None => next.push(TimerRuntime::from_def(def)),
            }
        }
        self.timers = next;
    }

    pub fn start(&mut self, id: &str) -> bool {
        match self.timer_mut(id) {
            Some(timer) if !timer.running => {
                timer.started_at = Some(Instant::now());
                timer.running = true;
                true
            }
            _ => false,
        }
    }

    pub fn pause(&mut self, id: &str) -> bool {
        match self.timer_mut(id) {
            Some(timer) if timer.running => {
                timer.accumulated = timer.elapsed();
                timer.started_at = None;
                timer.running = false;
                true
            }
            _ => false,
        }
    }

    pub fn toggle(&mut self, id: &str) -> bool {
        match self.timer(id) {
            Some(timer) if timer.running => self.pause(id),
            Some(_) => self.start(id),
            None => false,
        }
    }

    pub fn reset(&mut self, id: &str) -> bool {
        match self.timer_mut(id) {
            Some(timer) => {
                timer.accumulated = Duration::ZERO;
                timer.started_at = None;
                timer.running = false;
                true
            }
            None => false,
        }
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.timer(id).is_some_and(|timer| timer.running)
    }

    /// Monotonic elapsed time, useful for callers that need sub-second
    /// precision.
    pub fn elapsed(&self, id: &str) -> Option<Duration> {
        self.timer(id).map(TimerRuntime::elapsed)
    }

    pub fn snapshot(&self, id: &str) -> Option<TimerSnapshot> {
        self.timer(id).map(TimerRuntime::snapshot)
    }

    pub fn all(&self) -> Vec<TimerSnapshot> {
        self.timers.iter().map(TimerRuntime::snapshot).collect()
    }

    /// Text for a timer layer. Unknown ids render [`MISSING`]; count-up timers
    /// have no remaining/end-at value and therefore render [`MISSING`] in
    /// those modes.
    pub fn text_for_layer(&self, id: &str, mode: TimerMode) -> String {
        let Some(snapshot) = self.snapshot(id) else {
            return MISSING.to_string();
        };
        match mode {
            TimerMode::Elapsed => format_hms(snapshot.elapsed_secs),
            TimerMode::Remaining => match snapshot.remaining_secs {
                Some(remaining) => format_remaining(remaining),
                None => MISSING.to_string(),
            },
            TimerMode::EndAt => match snapshot.end_at {
                Some(end_at) => format_end_at(end_at),
                None => MISSING.to_string(),
            },
        }
    }

    fn timer(&self, id: &str) -> Option<&TimerRuntime> {
        self.timers.iter().find(|timer| timer.id == id)
    }

    fn timer_mut(&mut self, id: &str) -> Option<&mut TimerRuntime> {
        self.timers.iter_mut().find(|timer| timer.id == id)
    }
}

fn hms_abs(total_secs: u64) -> String {
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// `3725` → `"01:02:05"`, `65` → `"01:05"`.
pub fn format_hms(total_secs: i64) -> String {
    hms_abs(total_secs.unsigned_abs())
}

/// Non-negative values format as [`format_hms`]; overtime is prefixed with `+`.
pub fn format_remaining(remaining_secs: i64) -> String {
    if remaining_secs >= 0 {
        format_hms(remaining_secs)
    } else {
        format!("+{}", hms_abs(remaining_secs.unsigned_abs()))
    }
}

/// Clock layer text for the given preset.
pub fn format_clock(preset: ClockPreset, now: DateTime<Local>) -> String {
    match preset {
        ClockPreset::H24 => now.format("%H:%M").to_string(),
        ClockPreset::H24Seconds => now.format("%H:%M:%S").to_string(),
        ClockPreset::H12 => now.format("%I:%M %p").to_string(),
        ClockPreset::H12Seconds => now.format("%I:%M:%S %p").to_string(),
    }
}

/// Numeric local date, e.g. `17.11.2026`.
pub fn format_date(now: DateTime<Local>) -> String {
    now.format("%d.%m.%Y").to_string()
}

/// Wall-clock end time, e.g. `14:05`.
pub fn format_end_at(end_at: DateTime<Local>) -> String {
    end_at.format("%H:%M").to_string()
}

/// Program ETA: remaining time for a playlist item with a duration. Overtime
/// and untimed items have no meaningful ETA and render [`MISSING`].
pub fn format_eta(remaining_secs: Option<i64>) -> String {
    match remaining_secs {
        Some(secs) if secs >= 0 => format_hms(secs),
        _ => MISSING.to_string(),
    }
}
