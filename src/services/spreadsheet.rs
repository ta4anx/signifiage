//! Spreadsheet (`.xlsx`/`.xls`/`.ods`) reading and column mapping.
//!
//! Pure data access: calamine parses the workbook, this module turns cells into
//! display strings and normalized [`ImportRow`]s. No GTK, no localization, and
//! the workbook is never written back.

use std::path::Path;

use calamine::{Data, Range, Reader, Sheets, open_workbook_auto};

use crate::model::queue::{QueueStatus, fold_ascii};
use crate::services::queue::{ImportRow, QueueService};

/// Sheets with more non-empty data rows than this are rejected.
pub const MAX_ROWS: usize = 20_000;

/// Number of data rows returned by [`preview`].
const PREVIEW_ROWS: usize = 12;

type AutoWorkbook = Sheets<std::io::BufReader<std::fs::File>>;

/// Anything that can go wrong while reading a queue spreadsheet.
#[derive(Debug, thiserror::Error)]
pub enum SheetError {
    #[error("cannot read {path}: {source}")]
    Io { path: String, source: anyhow::Error },
    #[error("too many rows: {rows}")]
    TooManyRows { rows: usize },
    #[error("no sheet named {0}")]
    UnknownSheet(String),
    #[error("no non-empty row found")]
    Empty,
    #[error("column {0} is out of range")]
    BadColumn(u32),
}

/// Which spreadsheet columns feed each queue field. Indices are zero-based
/// absolute columns (A = 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColumnMapping {
    pub name_col: u32,
    pub ticket_col: Option<u32>,
    pub note_col: Option<u32>,
    pub status_col: Option<u32>,
}

/// What the import wizard needs to show before committing an import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetPreview {
    pub sheets: Vec<String>,
    pub sheet: String,
    /// 1-based row number of the header.
    pub header_row: u32,
    /// Header cells in column order; empty cells are `""`.
    pub headers: Vec<String>,
    /// Up to 12 non-empty data rows as display strings.
    pub rows: Vec<Vec<String>>,
}

/// List the sheet names in a workbook.
pub fn sheet_names(path: &Path) -> Result<Vec<String>, SheetError> {
    let workbook = open(path)?;
    Ok(workbook.sheet_names())
}

/// Read a preview for the wizard: the first sheet (or a chosen one), its first
/// non-empty header row and a handful of data rows.
pub fn preview(path: &Path, sheet: Option<&str>) -> Result<SheetPreview, SheetError> {
    let mut workbook = open(path)?;
    let sheets = workbook.sheet_names();
    let sheet = match sheet {
        Some(name) => {
            if !sheets.iter().any(|known| known == name) {
                return Err(SheetError::UnknownSheet(name.to_string()));
            }
            name.to_string()
        }
        None => sheets.first().cloned().ok_or_else(|| SheetError::Empty)?,
    };
    let range = range_for(&mut workbook, path, &sheet)?;
    let Some((start_row, _end_row, end_col)) = bounds(&range) else {
        return Err(SheetError::Empty);
    };

    let mut header_index = None;
    for (offset, row) in range.rows().enumerate() {
        if !row_is_empty(row) {
            header_index = Some(offset);
            break;
        }
    }
    let Some(header_index) = header_index else {
        return Err(SheetError::Empty);
    };
    let header_abs = start_row + header_index as u32;
    let headers = (0..=end_col)
        .map(|col| cell_text(&range, header_abs, col))
        .collect();

    let mut rows = Vec::new();
    let mut data_rows = 0usize;
    for (offset, _) in range.rows().enumerate().skip(header_index + 1) {
        let abs_row = start_row + offset as u32;
        let row: Vec<String> = (0..=end_col)
            .map(|col| cell_text(&range, abs_row, col))
            .collect();
        if row.iter().all(|cell| cell.is_empty()) {
            continue;
        }
        data_rows += 1;
        if data_rows > MAX_ROWS {
            return Err(SheetError::TooManyRows { rows: data_rows });
        }
        if rows.len() < PREVIEW_ROWS {
            rows.push(row);
        }
    }

    Ok(SheetPreview {
        sheets,
        sheet,
        header_row: header_abs + 1,
        headers,
        rows,
    })
}

/// Read every non-empty data row after `header_row` (1-based) using `mapping`.
///
/// Rows with an empty name are skipped. Tickets, notes and statuses are only
/// read when their column is mapped; unmapped fields are empty/`None`.
pub fn read_import_rows(
    path: &Path,
    sheet: &str,
    header_row: u32,
    mapping: &ColumnMapping,
) -> Result<Vec<ImportRow>, SheetError> {
    let mut workbook = open(path)?;
    let range = range_for(&mut workbook, path, sheet)?;
    let Some((start_row, _end_row, end_col)) = bounds(&range) else {
        return Ok(Vec::new());
    };

    let mapped = [
        Some(mapping.name_col),
        mapping.ticket_col,
        mapping.note_col,
        mapping.status_col,
    ];
    for col in mapped.into_iter().flatten() {
        if col > end_col {
            return Err(SheetError::BadColumn(col));
        }
    }

    let data_start = header_row.max(1);
    let skip = data_start.saturating_sub(start_row) as usize;
    let mut rows = Vec::new();
    let mut data_rows = 0usize;
    for (offset, cells) in range.rows().enumerate().skip(skip) {
        if row_is_empty(cells) {
            continue;
        }
        data_rows += 1;
        if data_rows > MAX_ROWS {
            return Err(SheetError::TooManyRows { rows: data_rows });
        }
        let abs_row = start_row + offset as u32;
        let name = cell_text(&range, abs_row, mapping.name_col);
        if name.is_empty() {
            continue;
        }
        let ticket = mapping
            .ticket_col
            .map(|col| cell_text(&range, abs_row, col))
            .unwrap_or_default();
        let note = mapping
            .note_col
            .map(|col| cell_text(&range, abs_row, col))
            .unwrap_or_default();
        let status = mapping
            .status_col
            .map(|col| cell_text(&range, abs_row, col))
            .and_then(|token| QueueStatus::parse_token(&token));
        rows.push(ImportRow {
            source_key: QueueService::source_key(&ticket, &name),
            name,
            note,
            ticket,
            status,
        });
    }
    Ok(rows)
}

/// Guess a mapping from header labels, case-insensitively and without Turkish
/// diacritics. Unmatched optional columns stay unmapped; the name column falls
/// back to the first column.
pub fn guess_mapping(headers: &[String]) -> ColumnMapping {
    let find = |candidates: &[&str]| -> Option<u32> {
        headers
            .iter()
            .position(|header| {
                let folded = fold_ascii(header);
                candidates.contains(&folded.as_str())
            })
            .map(|index| index as u32)
    };
    ColumnMapping {
        name_col: find(&["name", "adsoyad", "ad", "soyad", "isim"]).unwrap_or(0),
        ticket_col: find(&["ticket", "no", "sira", "numara", "id"]),
        note_col: find(&["note", "not", "aciklama"]),
        status_col: find(&["status", "durum"]),
    }
}

/// Convert one cell to the display string used by the wizard and the importer.
pub fn display_string(cell: &Data) -> String {
    match cell {
        Data::Empty | Data::Error(_) => String::new(),
        Data::String(text) => collapse(text),
        Data::Int(value) => value.to_string(),
        Data::Float(value) => format_float(*value),
        Data::Bool(value) => {
            if *value {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Data::DateTime(value) => {
            let (year, month, day, _, _, _, _) = value.to_ymd_hms_milli();
            format!("{year:04}-{month:02}-{day:02}")
        }
        Data::DateTimeIso(text) => date_part(text),
        Data::DurationIso(text) => collapse(text),
    }
}

fn open(path: &Path) -> Result<AutoWorkbook, SheetError> {
    open_workbook_auto(path).map_err(|error| SheetError::Io {
        path: path.display().to_string(),
        source: anyhow::Error::new(error),
    })
}

fn range_for(
    workbook: &mut AutoWorkbook,
    path: &Path,
    sheet: &str,
) -> Result<Range<Data>, SheetError> {
    if !workbook.sheet_names().iter().any(|known| known == sheet) {
        return Err(SheetError::UnknownSheet(sheet.to_string()));
    }
    workbook
        .worksheet_range(sheet)
        .map_err(|error| SheetError::Io {
            path: path.display().to_string(),
            source: anyhow::Error::new(error),
        })
}

/// `(first row, last row, last column)` in absolute coordinates, if any cell
/// exists.
fn bounds(range: &Range<Data>) -> Option<(u32, u32, u32)> {
    let start = range.start()?;
    let end = range.end()?;
    Some((start.0, end.0, end.1))
}

fn cell_text(range: &Range<Data>, row: u32, col: u32) -> String {
    range
        .get_value((row, col))
        .map(display_string)
        .unwrap_or_default()
}

fn row_is_empty(row: &[Data]) -> bool {
    row.iter().all(|cell| display_string(cell).is_empty())
}

/// Trim and collapse runs of internal whitespace to single spaces.
fn collapse(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Integral floats render without a trailing `.0`; other floats use the
/// shortest round-trippable form.
fn format_float(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    format!("{value}")
}

/// Keep just the date of an ISO-8601 date-time string, otherwise the text.
fn date_part(raw: &str) -> String {
    let text = raw.trim();
    let bytes = text.as_bytes();
    if bytes.len() >= 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..10].iter().all(u8::is_ascii_digit)
    {
        return text[..10].to_string();
    }
    collapse(text)
}
