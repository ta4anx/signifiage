//! Background polling of the queue spreadsheet source.
//!
//! Pure Rust, no GTK: the queue page owns a [`SourceWatcher`], calls
//! [`SourceWatcher::tick`] from its 100 ms poll and forwards the events to the
//! controller on the main thread. Reads always run on a worker thread so a large
//! workbook never freezes the operator UI.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime};

use crate::model::QueueSource;
use crate::services::queue::ImportRow;
use crate::services::spreadsheet::{self, ColumnMapping};

/// Minimum time between two change checks.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// A read that never reports back must not block polling forever.
const PENDING_TIMEOUT: Duration = Duration::from_secs(30);

/// Result of one background read of the linked workbook.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceEvent {
    /// Rows normalized for `QueueService::merge_import`.
    Rows(Vec<ImportRow>),
    /// A user-facing read error (already a display string; not localized).
    Error(String),
}

/// File identity used for change detection: size plus modification time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn of(path: &PathBuf) -> std::io::Result<Self> {
        let meta = std::fs::metadata(path)?;
        Ok(Self {
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

/// Watches the linked spreadsheet and reads it again whenever it changes.
pub struct SourceWatcher {
    source: Option<QueueSource>,
    stamp: Option<Stamp>,
    last_check: Option<Instant>,
    pending: bool,
    pending_since: Option<Instant>,
    pending_generation: u64,
    force: bool,
    last_error: Option<String>,
    /// Bumped on every source change so stale worker threads drop their results.
    generation: Arc<AtomicU64>,
    /// Number of reads started; used by the unit tests.
    reads_started: u32,
}

impl Default for SourceWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceWatcher {
    pub fn new() -> Self {
        Self {
            source: None,
            stamp: None,
            last_check: None,
            pending: false,
            pending_since: None,
            pending_generation: 0,
            force: false,
            last_error: None,
            generation: Arc::new(AtomicU64::new(0)),
            reads_started: 0,
        }
    }

    /// Bind the watcher to a source (or unbind with `None`).
    ///
    /// The next tick starts one read, so the page sees the current state even
    /// when the workbook was not touched since the import wizard read it.
    pub fn set_source(&mut self, source: Option<QueueSource>) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.source = source;
        self.stamp = None;
        self.last_check = None;
        self.pending = false;
        self.pending_since = None;
        self.force = false;
        self.last_error = None;
    }

    /// The linked source, with whatever path it was stored with.
    pub fn source(&self) -> Option<&QueueSource> {
        self.source.as_ref()
    }

    /// True while a background read is in flight.
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// Forget the in-flight read state after the page applied its event.
    pub fn complete(&mut self) {
        if self.pending_generation == self.generation.load(Ordering::SeqCst) {
            self.pending = false;
            self.pending_since = None;
        }
    }

    /// Check the workbook and start a read when it changed.
    ///
    /// Returns `true` while a source is bound (polling is enabled). The page
    /// calls this on its poll tick; the 2 s interval is enforced here.
    pub fn tick(&mut self, sender: &Sender<SourceEvent>) -> bool {
        let Some(source) = self.source.clone() else {
            return false;
        };
        if self.pending {
            if self
                .pending_since
                .is_some_and(|since| since.elapsed() > PENDING_TIMEOUT)
            {
                self.pending = false;
                self.pending_since = None;
                self.stamp = None;
                self.report_error(
                    sender,
                    "the spreadsheet read did not finish; retrying".to_string(),
                );
            } else {
                return true;
            }
        }
        let forced = self.force;
        if !forced
            && self
                .last_check
                .is_some_and(|at| at.elapsed() < POLL_INTERVAL)
        {
            return true;
        }
        self.last_check = Some(Instant::now());
        self.force = false;

        let path = PathBuf::from(&source.path);
        let stamp = match Stamp::of(&path) {
            Ok(stamp) => stamp,
            Err(err) => {
                // A missing/unreadable file is reported once per change, not on
                // every poll, and a later appearance triggers a read.
                self.stamp = None;
                self.report_error(sender, format!("{path}: {err}", path = path.display()));
                return true;
            }
        };
        if !forced && Some(stamp) == self.stamp {
            return true;
        }
        self.stamp = Some(stamp);
        self.spawn_read(source, sender);
        true
    }

    /// Read the workbook now, ignoring the change stamp and poll interval.
    pub fn force_reload(&mut self, sender: &Sender<SourceEvent>) -> bool {
        if self.source.is_none() {
            return false;
        }
        self.force = true;
        self.last_check = None;
        self.tick(sender)
    }

    fn spawn_read(&mut self, source: QueueSource, sender: &Sender<SourceEvent>) {
        let mapping = ColumnMapping {
            name_col: source.name_col,
            ticket_col: source.ticket_col,
            note_col: source.note_col,
            status_col: source.status_col,
        };
        let generation = self.generation.load(Ordering::SeqCst);
        let shared = self.generation.clone();
        let thread_sender = sender.clone();
        self.reads_started = self.reads_started.saturating_add(1);
        self.pending = true;
        self.pending_since = Some(Instant::now());
        self.pending_generation = generation;

        let spawned = std::thread::Builder::new()
            .name("signifiage-queue-source".to_string())
            .spawn(move || {
                let path = PathBuf::from(&source.path);
                let event = match spreadsheet::read_import_rows(
                    &path,
                    &source.sheet,
                    source.header_row.max(1),
                    &mapping,
                ) {
                    Ok(rows) => SourceEvent::Rows(rows),
                    Err(err) => SourceEvent::Error(format!("{}: {}", path.display(), clean(err))),
                };
                // The source changed while we were reading: drop the result.
                if shared.load(Ordering::SeqCst) == generation {
                    let _ = thread_sender.send(event);
                }
            });

        if spawned.is_err() {
            self.pending = false;
            self.pending_since = None;
            self.report_error(
                sender,
                "could not start the background spreadsheet reader".to_string(),
            );
        }
    }

    fn report_error(&mut self, sender: &Sender<SourceEvent>, message: String) {
        if self.last_error.as_deref() == Some(message.as_str()) {
            return;
        }
        self.last_error = Some(message.clone());
        let _ = sender.send(SourceEvent::Error(message));
    }

    /// Record a successfully applied read so a later identical error is shown.
    pub fn clear_error(&mut self) {
        self.last_error = None;
    }
}

/// One-line display string for a spreadsheet error.
fn clean(err: spreadsheet::SheetError) -> String {
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::mpsc::{Receiver, channel};

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/queue.xlsx")
    }

    fn temp_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("signifiage-queue-source-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn source_for(path: &Path) -> QueueSource {
        let sheets = spreadsheet::sheet_names(path).expect("sheet names");
        QueueSource {
            path: path.display().to_string(),
            sheet: sheets.first().cloned().unwrap_or_default(),
            header_row: 1,
            name_col: 1,
            ticket_col: Some(0),
            note_col: Some(2),
            status_col: None,
        }
    }

    fn next_event(rx: &Receiver<SourceEvent>, millis: u64) -> Option<SourceEvent> {
        rx.recv_timeout(Duration::from_millis(millis)).ok()
    }

    /// Skip the 2 s gate in tests.
    fn poll_now(watcher: &mut SourceWatcher) {
        watcher.last_check = None;
    }

    #[test]
    fn disabled_until_a_source_is_bound() {
        let mut watcher = SourceWatcher::new();
        let (tx, rx) = channel();
        assert!(!watcher.tick(&tx));
        assert!(!watcher.force_reload(&tx));
        assert!(next_event(&rx, 50).is_none());
        assert_eq!(watcher.reads_started, 0);
    }

    #[test]
    fn first_tick_reads_rows_and_clears_pending() {
        let dir = temp_dir();
        let path = dir.join("queue.xlsx");
        fs::copy(fixture(), &path).expect("copy fixture");
        let (tx, rx) = channel();
        let mut watcher = SourceWatcher::new();
        watcher.set_source(Some(source_for(&path)));

        assert!(watcher.tick(&tx));
        assert!(watcher.is_pending());
        let Some(SourceEvent::Rows(rows)) = next_event(&rx, 5000) else {
            panic!("expected rows");
        };
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "Ayşe Yılmaz");
        watcher.complete();
        assert!(!watcher.is_pending());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pending_flag_prevents_overlapping_reads() {
        let dir = temp_dir();
        let path = dir.join("queue.xlsx");
        fs::copy(fixture(), &path).expect("copy fixture");
        let (tx, rx) = channel();
        let mut watcher = SourceWatcher::new();
        watcher.set_source(Some(source_for(&path)));

        assert!(watcher.tick(&tx));
        assert_eq!(watcher.reads_started, 1);
        poll_now(&mut watcher);
        assert!(watcher.tick(&tx), "still enabled while reading");
        assert_eq!(watcher.reads_started, 1, "no second thread while pending");
        assert!(next_event(&rx, 5000).is_some());
        watcher.complete();

        // Same stamp after the interval: nothing to do.
        poll_now(&mut watcher);
        assert!(watcher.tick(&tx));
        assert_eq!(watcher.reads_started, 1);
        assert!(next_event(&rx, 200).is_none());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn size_change_starts_a_new_read() {
        let dir = temp_dir();
        let path = dir.join("queue.xlsx");
        fs::copy(fixture(), &path).expect("copy fixture");
        let (tx, rx) = channel();
        let mut watcher = SourceWatcher::new();
        watcher.set_source(Some(source_for(&path)));
        assert!(watcher.tick(&tx));
        assert!(next_event(&rx, 5000).is_some());
        watcher.complete();

        // Grow the file so the stamp changes even with coarse mtime.
        let mut bytes = fs::read(&path).expect("read fixture");
        bytes.extend_from_slice(b"stale");
        fs::write(&path, bytes).expect("rewrite fixture");

        poll_now(&mut watcher);
        assert!(watcher.tick(&tx));
        assert_eq!(watcher.reads_started, 2);
        assert!(
            next_event(&rx, 5000).is_some(),
            "a changed file must be read again"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn force_reload_ignores_the_stamp() {
        let dir = temp_dir();
        let path = dir.join("queue.xlsx");
        fs::copy(fixture(), &path).expect("copy fixture");
        let (tx, rx) = channel();
        let mut watcher = SourceWatcher::new();
        watcher.set_source(Some(source_for(&path)));
        assert!(watcher.tick(&tx));
        assert!(next_event(&rx, 5000).is_some());
        watcher.complete();

        assert!(watcher.force_reload(&tx));
        assert_eq!(watcher.reads_started, 2);
        assert!(next_event(&rx, 5000).is_some());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_reports_once_then_reads_when_it_appears() {
        let dir = temp_dir();
        let path = dir.join("queue.xlsx");
        let sheets = spreadsheet::sheet_names(&fixture()).expect("fixture sheets");
        let source = QueueSource {
            path: path.display().to_string(),
            sheet: sheets.first().cloned().unwrap_or_default(),
            header_row: 1,
            name_col: 1,
            ticket_col: Some(0),
            note_col: Some(2),
            status_col: None,
        };
        let (tx, rx) = channel();
        let mut watcher = SourceWatcher::new();
        watcher.set_source(Some(source));

        assert!(watcher.tick(&tx));
        let Some(SourceEvent::Error(first)) = next_event(&rx, 2000) else {
            panic!("expected a missing-file error");
        };
        assert!(first.contains("queue.xlsx"), "{first}");

        poll_now(&mut watcher);
        assert!(watcher.tick(&tx));
        assert!(
            next_event(&rx, 200).is_none(),
            "the same error must not repeat every poll"
        );

        fs::copy(fixture(), &path).expect("copy fixture");
        poll_now(&mut watcher);
        assert!(watcher.tick(&tx));
        assert!(matches!(next_event(&rx, 5000), Some(SourceEvent::Rows(_))));
        fs::remove_dir_all(&dir).ok();
    }
}
