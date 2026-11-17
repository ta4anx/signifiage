//! Called-name queue: rows, statuses, ticket allocation, spreadsheet source.

use serde::{Deserialize, Serialize};

use crate::model::layer::Rgba;
use crate::model::new_id;

/// Status of a queue row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueueStatus {
    #[default]
    Waiting,
    Called,
    Done,
    Skipped,
}

impl QueueStatus {
    pub const ALL: [QueueStatus; 4] = [
        QueueStatus::Waiting,
        QueueStatus::Called,
        QueueStatus::Done,
        QueueStatus::Skipped,
    ];

    pub fn id(self) -> &'static str {
        match self {
            QueueStatus::Waiting => "waiting",
            QueueStatus::Called => "called",
            QueueStatus::Done => "done",
            QueueStatus::Skipped => "skipped",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "waiting" => Some(QueueStatus::Waiting),
            "called" => Some(QueueStatus::Called),
            "done" => Some(QueueStatus::Done),
            "skipped" => Some(QueueStatus::Skipped),
            _ => None,
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            QueueStatus::Waiting => "queue.status.waiting",
            QueueStatus::Called => "queue.status.called",
            QueueStatus::Done => "queue.status.done",
            QueueStatus::Skipped => "queue.status.skipped",
        }
    }

    /// Accept English and Turkish spreadsheet spellings.
    pub fn parse_token(raw: &str) -> Option<Self> {
        let key = fold_ascii(raw);
        match key.as_str() {
            "waiting" | "bekliyor" | "beklemede" => Some(QueueStatus::Waiting),
            "called" | "cagrildi" => Some(QueueStatus::Called),
            "done" | "tamamlandi" => Some(QueueStatus::Done),
            "skipped" | "atlandi" => Some(QueueStatus::Skipped),
            _ => None,
        }
    }
}

/// Lowercase ASCII fold that keeps letters and digits only.
pub fn fold_ascii(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            'ı' | 'İ' | 'i' => Some('i'),
            'ş' => Some('s'),
            'ğ' => Some('g'),
            'ü' => Some('u'),
            'ö' => Some('o'),
            'ç' => Some('c'),
            'â' => Some('a'),
            'î' => Some('i'),
            'û' => Some('u'),
            c if c.is_ascii_alphanumeric() => Some(c),
            c if c.is_alphanumeric() => Some(c),
            _ => None,
        })
        .collect()
}

/// One person in the queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueRow {
    pub id: String,
    pub ticket: String,
    pub name: String,
    pub note: String,
    pub status: QueueStatus,
    pub created_at: String,
    pub called_at: Option<String>,
    pub call_count: u32,
    /// Identity in the imported spreadsheet; internal.
    pub source_key: String,
}

impl Default for QueueRow {
    fn default() -> Self {
        Self {
            id: new_id(),
            ticket: String::new(),
            name: String::new(),
            note: String::new(),
            status: QueueStatus::Waiting,
            created_at: String::new(),
            called_at: None,
            call_count: 0,
            source_key: String::new(),
        }
    }
}

impl QueueRow {
    pub fn new(
        name: impl Into<String>,
        ticket: impl Into<String>,
        note: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            ticket: ticket.into(),
            note: note.into(),
            created_at: crate::model::now_rfc3339(),
            ..Self::default()
        }
    }

    /// Ticket shown in the UI: padded to three digits when numeric.
    pub fn display_ticket(&self) -> String {
        pad_ticket(&self.ticket)
    }
}

/// Zero-pad a numeric ticket to at least three digits.
pub fn pad_ticket(ticket: &str) -> String {
    let trimmed = ticket.trim();
    if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) && trimmed.len() < 3 {
        format!("{trimmed:0>3}")
    } else {
        trimmed.to_string()
    }
}

/// Binding to an external spreadsheet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueSource {
    pub path: String,
    pub sheet: String,
    pub header_row: u32,
    pub name_col: u32,
    pub ticket_col: Option<u32>,
    pub note_col: Option<u32>,
    pub status_col: Option<u32>,
}

impl Default for QueueSource {
    fn default() -> Self {
        Self {
            path: String::new(),
            sheet: String::new(),
            header_row: 1,
            name_col: 0,
            ticket_col: None,
            note_col: None,
            status_col: None,
        }
    }
}

/// Persisted queue state: rows plus the next ticket counter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueData {
    pub source: Option<QueueSource>,
    pub next_ticket: u32,
    pub rows: Vec<QueueRow>,
}

impl Default for QueueData {
    fn default() -> Self {
        Self {
            source: None,
            next_ticket: 1,
            rows: Vec::new(),
        }
    }
}

impl QueueData {
    /// The single row with `status = called`.
    pub fn current(&self) -> Option<&QueueRow> {
        self.rows
            .iter()
            .find(|row| row.status == QueueStatus::Called)
    }

    pub fn current_index(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.status == QueueStatus::Called)
    }

    pub fn first_waiting_index(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.status == QueueStatus::Waiting)
    }

    pub fn first_waiting(&self) -> Option<&QueueRow> {
        self.first_waiting_index().map(|i| &self.rows[i])
    }

    /// Up to `limit` waiting rows in insertion order.
    pub fn upcoming(&self, limit: usize) -> Vec<&QueueRow> {
        self.rows
            .iter()
            .filter(|row| row.status == QueueStatus::Waiting)
            .take(limit)
            .collect()
    }

    /// Most recently finished call (done/skipped), for the "last called" board.
    pub fn last_called(&self) -> Option<&QueueRow> {
        self.rows
            .iter()
            .filter(|row| {
                matches!(row.status, QueueStatus::Done | QueueStatus::Skipped)
                    && row.called_at.is_some()
            })
            .max_by(|a, b| a.called_at.cmp(&b.called_at))
    }

    pub fn count(&self, status: QueueStatus) -> usize {
        self.rows.iter().filter(|row| row.status == status).count()
    }

    /// Allocate the next free padded ticket starting from `next_ticket`.
    pub fn allocate_ticket(&mut self) -> String {
        loop {
            let candidate = pad_ticket(&self.next_ticket.to_string());
            let taken = self.rows.iter().any(|row| row.ticket.trim() == candidate);
            if self.next_ticket < u32::MAX {
                self.next_ticket += 1;
            }
            if !taken {
                return candidate;
            }
        }
    }

    /// Enforce "at most one called row" after loading or editing.
    pub fn normalize(&mut self) {
        if self.next_ticket == 0 {
            self.next_ticket = 1;
        }
        let mut seen_called = false;
        for row in &mut self.rows {
            if row.call_count == 0 && row.called_at.is_some() {
                row.called_at = None;
            }
            if row.status == QueueStatus::Called {
                if seen_called {
                    row.status = QueueStatus::Waiting;
                } else {
                    seen_called = true;
                }
            }
        }
    }

    /// Replace all rows and the counter, keeping invariants.
    pub fn replace(&mut self, mut rows: Vec<QueueRow>, next_ticket: u32) {
        self.rows = std::mem::take(&mut rows);
        self.next_ticket = next_ticket.max(1);
        self.normalize();
    }
}

/// Colour used by the queue layer's accent when the project colour is invalid.
pub fn accent_rgba(value: &str) -> Rgba {
    Rgba::parse(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, status: QueueStatus) -> QueueRow {
        QueueRow {
            name: name.to_string(),
            status,
            ..QueueRow::default()
        }
    }

    #[test]
    fn at_most_one_called_row() {
        let mut data = QueueData {
            rows: vec![row("a", QueueStatus::Called), row("b", QueueStatus::Called)],
            ..QueueData::default()
        };
        data.normalize();
        assert_eq!(
            data.current().map(|r| r.name.clone()),
            Some("a".to_string())
        );
        assert_eq!(data.count(QueueStatus::Waiting), 1);
    }

    #[test]
    fn tickets_pad_and_skip_collisions() {
        let mut data = QueueData {
            next_ticket: 1,
            rows: vec![QueueRow {
                ticket: "001".into(),
                ..QueueRow::default()
            }],
            ..QueueData::default()
        };
        assert_eq!(data.allocate_ticket(), "002");
        assert_eq!(data.next_ticket, 3);
        assert_eq!(pad_ticket("7"), "007");
        assert_eq!(pad_ticket("1234"), "1234");
        assert_eq!(pad_ticket("A12"), "A12");
    }

    #[test]
    fn status_tokens_accept_turkish() {
        assert_eq!(
            QueueStatus::parse_token("Bekliyor"),
            Some(QueueStatus::Waiting)
        );
        assert_eq!(
            QueueStatus::parse_token("çağrıldı"),
            Some(QueueStatus::Called)
        );
        assert_eq!(
            QueueStatus::parse_token("TAMAMLANDI"),
            Some(QueueStatus::Done)
        );
        assert_eq!(
            QueueStatus::parse_token("atlandı"),
            Some(QueueStatus::Skipped)
        );
        assert_eq!(QueueStatus::parse_token("belirsiz"), None);
    }

    #[test]
    fn upcoming_is_limited_to_waiting_rows() {
        let data = QueueData {
            rows: vec![
                row("a", QueueStatus::Done),
                row("b", QueueStatus::Waiting),
                row("c", QueueStatus::Called),
                row("d", QueueStatus::Waiting),
                row("e", QueueStatus::Waiting),
            ],
            ..QueueData::default()
        };
        let names: Vec<_> = data.upcoming(2).iter().map(|r| r.name.clone()).collect();
        assert_eq!(names, vec!["b".to_string(), "d".to_string()]);
        assert_eq!(data.first_waiting().unwrap().name, "b");
    }

    #[test]
    fn queue_json_round_trips_with_defaults() {
        let data = QueueData::default();
        let json = serde_json::to_string(&data).unwrap();
        let back: QueueData = serde_json::from_str(&json).unwrap();
        assert_eq!(back, data);
        let partial: QueueData = serde_json::from_str(r#"{"rows":[]}"#).unwrap();
        assert_eq!(partial.next_ticket, 1);
        assert!(partial.source.is_none());
    }
}
