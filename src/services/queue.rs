//! Queue actions, undo and spreadsheet merge.
//!
//! Pure data service: no GTK, no I/O, no localized strings. The UI layer owns
//! presentation; this module owns the at-most-one-called invariant and the undo
//! journal of `rows` + `next_ticket`.

use std::collections::VecDeque;

use rand::Rng;

use crate::model::queue::fold_ascii;
use crate::model::{QueueData, QueueRow, QueueStatus, now_rfc3339};

/// Number of undo snapshots kept.
const UNDO_LIMIT: usize = 50;
/// Number of waiting rows exposed to the UI/web snapshot.
const SNAPSHOT_UPCOMING: usize = 5;

/// Read-only view of the queue for rendering and the web remote.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueSnapshot {
    pub current: Option<QueueRow>,
    pub last_called: Option<QueueRow>,
    pub upcoming: Vec<QueueRow>,
    pub waiting: usize,
    pub called: usize,
    pub done: usize,
    pub skipped: usize,
}

/// One normalized row coming from a spreadsheet read.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImportRow {
    pub source_key: String,
    pub name: String,
    pub note: String,
    pub ticket: String,
    pub status: Option<QueueStatus>,
}

/// What a merge actually changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImportReport {
    pub added: usize,
    pub updated: usize,
    pub changed: bool,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum QueueError {
    #[error("name is required")]
    EmptyName,
    #[error("ticket already used")]
    DuplicateTicket,
    #[error("unknown row")]
    UnknownRow,
}

#[derive(Debug, Clone)]
struct UndoState {
    rows: Vec<QueueRow>,
    next_ticket: u32,
}

/// Queue mutations with a 50-entry undo stack.
pub struct QueueService {
    data: QueueData,
    undo: VecDeque<UndoState>,
}

impl QueueService {
    pub fn new(data: QueueData) -> Self {
        let mut data = data;
        data.normalize();
        Self {
            data,
            undo: VecDeque::new(),
        }
    }

    pub fn data(&self) -> &QueueData {
        &self.data
    }

    /// Load/sync path: replace state, enforce invariants, clear history.
    pub fn replace(&mut self, mut data: QueueData) {
        data.normalize();
        self.data = data;
        self.undo.clear();
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            current: self.data.current().cloned(),
            last_called: self.data.last_called().cloned(),
            upcoming: self
                .data
                .upcoming(SNAPSHOT_UPCOMING)
                .into_iter()
                .cloned()
                .collect(),
            waiting: self.data.count(QueueStatus::Waiting),
            called: self.data.count(QueueStatus::Called),
            done: self.data.count(QueueStatus::Done),
            skipped: self.data.count(QueueStatus::Skipped),
        }
    }

    /// Identity for merge: trimmed ticket when present, else lowercased name.
    pub fn source_key(ticket: &str, name: &str) -> String {
        let ticket = ticket.trim();
        if ticket.is_empty() {
            name.trim().to_lowercase()
        } else {
            ticket.to_string()
        }
    }

    /// Call the first waiting row. False when nobody is waiting.
    pub fn next(&mut self) -> bool {
        let Some(waiting) = self.data.first_waiting_index() else {
            return false;
        };
        self.push_undo();
        if let Some(current) = self.data.current_index() {
            self.data.rows[current].status = QueueStatus::Done;
        }
        self.promote(waiting);
        true
    }

    /// Repeat the current call. Never records undo.
    pub fn recall(&mut self) -> bool {
        let Some(current) = self.data.current_index() else {
            return false;
        };
        let row = &mut self.data.rows[current];
        row.called_at = Some(now_rfc3339());
        row.call_count = row.call_count.saturating_add(1);
        true
    }

    /// Finish the current call.
    pub fn done(&mut self) -> bool {
        let Some(current) = self.data.current_index() else {
            return false;
        };
        self.push_undo();
        self.data.rows[current].status = QueueStatus::Done;
        true
    }

    /// Skip the current row and promote the next waiting one if any.
    pub fn skip(&mut self) -> bool {
        let Some(current) = self.data.current_index() else {
            return false;
        };
        self.push_undo();
        self.data.rows[current].status = QueueStatus::Skipped;
        if let Some(waiting) = self.data.first_waiting_index() {
            self.promote(waiting);
        }
        true
    }

    /// Call a specific waiting row. False for unknown or non-waiting rows.
    pub fn call(&mut self, id: &str) -> bool {
        let Some(index) = self.row_index(id) else {
            return false;
        };
        if self.data.rows[index].status != QueueStatus::Waiting {
            return false;
        }
        self.push_undo();
        if let Some(current) = self.data.current_index() {
            self.data.rows[current].status = QueueStatus::Done;
        }
        self.promote(index);
        true
    }

    /// Append a waiting row with an allocated ticket.
    pub fn add(&mut self, name: &str, note: &str) -> Result<QueueRow, QueueError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(QueueError::EmptyName);
        }
        self.push_undo();
        let ticket = self.data.allocate_ticket();
        let mut row = QueueRow::new(name, ticket.clone(), note);
        row.source_key = Self::source_key(&ticket, name);
        self.data.rows.push(row.clone());
        Ok(row)
    }

    /// Update name/note/ticket. Status and call history are never touched.
    pub fn edit(
        &mut self,
        id: &str,
        name: &str,
        note: &str,
        ticket: &str,
    ) -> Result<(), QueueError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(QueueError::EmptyName);
        }
        let index = self.row_index(id).ok_or(QueueError::UnknownRow)?;
        let requested = ticket.trim();
        if !requested.is_empty() && self.ticket_taken(requested, Some(id)) {
            return Err(QueueError::DuplicateTicket);
        }

        // Snapshot before allocating, since allocation advances `next_ticket`.
        let before = self.undo_state();
        let target_ticket = if requested.is_empty() {
            self.data.allocate_ticket()
        } else {
            requested.to_string()
        };
        let row = &mut self.data.rows[index];
        let changed = row.name != name || row.note != note || row.ticket != target_ticket;
        row.name = name.to_string();
        row.note = note.to_string();
        row.ticket = target_ticket;
        if changed {
            self.push_state(before);
        }
        Ok(())
    }

    /// Remove a row. If it was current, the queue has no current row afterwards.
    pub fn remove(&mut self, id: &str) -> Result<(), QueueError> {
        let index = self.row_index(id).ok_or(QueueError::UnknownRow)?;
        self.push_undo();
        self.data.rows.remove(index);
        Ok(())
    }

    /// Call a random waiting row (experimental "random pick").
    pub fn call_random(&mut self) -> Option<QueueRow> {
        let waiting: Vec<usize> = self
            .data
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.status == QueueStatus::Waiting)
            .map(|(index, _)| index)
            .collect();
        if waiting.is_empty() {
            return None;
        }
        let row_index = waiting[rand::rng().random_range(0..waiting.len())];
        let id = self.data.rows[row_index].id.clone();
        if self.call(&id) {
            self.data.current().cloned()
        } else {
            None
        }
    }

    /// Put every row back to `waiting` and restart the ticket counter.
    pub fn restart(&mut self) -> bool {
        let unchanged = self.data.next_ticket == 1
            && self
                .data
                .rows
                .iter()
                .all(|row| row.status == QueueStatus::Waiting && row.call_count == 0);
        if unchanged {
            return false;
        }
        self.push_undo();
        for row in &mut self.data.rows {
            row.status = QueueStatus::Waiting;
            row.called_at = None;
            row.call_count = 0;
        }
        self.data.next_ticket = 1;
        true
    }

    pub fn undo(&mut self) -> bool {
        let Some(state) = self.undo.pop_back() else {
            return false;
        };
        self.data.rows = state.rows;
        self.data.next_ticket = state.next_ticket.max(1);
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Additive merge of spreadsheet rows.
    ///
    /// Matched rows only refresh name/note (and an empty `source_key`); status,
    /// `called_at` and `call_count` are preserved. At most one undo snapshot is
    /// recorded, and only when rows actually changed.
    pub fn merge_import(&mut self, rows: &[ImportRow]) -> ImportReport {
        let before = self.undo_state();
        let mut added = 0;
        let mut updated = 0;

        for import in rows {
            let name = import.name.trim();
            if name.is_empty() {
                continue;
            }
            let key = if import.source_key.trim().is_empty() {
                Self::source_key(&import.ticket, name)
            } else {
                import.source_key.trim().to_string()
            };

            if let Some(index) = self.match_index(&key, name) {
                let row = &mut self.data.rows[index];
                let mut row_changed = false;
                if row.name != name {
                    row.name = name.to_string();
                    row_changed = true;
                }
                if row.note != import.note {
                    row.note = import.note.clone();
                    row_changed = true;
                }
                if row.source_key.trim().is_empty() {
                    row.source_key = key;
                    row_changed = true;
                }
                if row_changed {
                    updated += 1;
                }
                continue;
            }

            let mut status = import.status.unwrap_or(QueueStatus::Waiting);
            if status == QueueStatus::Called && self.data.current().is_some() {
                status = QueueStatus::Waiting;
            }
            let mapped = import.ticket.trim();
            let ticket = if mapped.is_empty() {
                self.data.allocate_ticket()
            } else {
                self.unique_ticket(mapped, &key)
            };
            let mut row = QueueRow::new(name, ticket, import.note.clone());
            row.status = status;
            row.source_key = key;
            self.data.rows.push(row);
            added += 1;
        }

        let changed = added > 0 || updated > 0;
        if changed {
            self.push_state(before);
        }
        ImportReport {
            added,
            updated,
            changed,
        }
    }

    pub fn ticket_taken(&self, ticket: &str, except_row: Option<&str>) -> bool {
        let ticket = ticket.trim();
        self.data
            .rows
            .iter()
            .any(|row| row.ticket.trim() == ticket && except_row != Some(row.id.as_str()))
    }

    pub fn clear_undo(&mut self) {
        self.undo.clear();
    }

    fn row_index(&self, id: &str) -> Option<usize> {
        self.data.rows.iter().position(|row| row.id == id)
    }

    fn promote(&mut self, index: usize) {
        let row = &mut self.data.rows[index];
        row.status = QueueStatus::Called;
        row.called_at = Some(now_rfc3339());
        row.call_count = row.call_count.saturating_add(1);
    }

    /// Existing row for a spreadsheet key: exact `source_key`, else a legacy row
    /// with no key yet and the same folded name.
    fn match_index(&self, key: &str, name: &str) -> Option<usize> {
        if key.is_empty() {
            return None;
        }
        if let Some(index) = self
            .data
            .rows
            .iter()
            .position(|row| !row.source_key.trim().is_empty() && row.source_key.trim() == key)
        {
            return Some(index);
        }
        let folded = fold_ascii(name);
        self.data
            .rows
            .iter()
            .position(|row| row.source_key.trim().is_empty() && fold_ascii(&row.name) == folded)
    }

    /// Make a mapped ticket unique against rows with a different source key.
    fn unique_ticket(&self, base: &str, key: &str) -> String {
        let taken = |candidate: &str| {
            self.data
                .rows
                .iter()
                .any(|row| row.ticket.trim() == candidate && row.source_key.trim() != key)
        };
        if !taken(base) {
            return base.to_string();
        }
        let mut suffix = 2u32;
        loop {
            let candidate = format!("{base}-{suffix}");
            if !taken(&candidate) {
                return candidate;
            }
            suffix = suffix.saturating_add(1);
        }
    }

    fn undo_state(&self) -> UndoState {
        UndoState {
            rows: self.data.rows.clone(),
            next_ticket: self.data.next_ticket,
        }
    }

    fn push_undo(&mut self) {
        let state = self.undo_state();
        self.push_state(state);
    }

    fn push_state(&mut self, state: UndoState) {
        if self.undo.len() == UNDO_LIMIT {
            self.undo.pop_front();
        }
        self.undo.push_back(state);
    }
}
