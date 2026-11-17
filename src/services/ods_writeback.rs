//! Experimental: write a called status back into a linked `.ods` sheet.
//!
//! Spreadsheets are normally read-only (the app owns queue state); this module is
//! only used when the operator enables both "random pick" and "write back" in
//! Preferences → Experimental. It edits `content.xml` in place, keeps every other
//! archive entry byte-identical and rewrites the package atomically.
//!
//! Only the small subset of ODF produced by LibreOffice/Excel for plain text
//! sheets is handled; anything unexpected returns an error and leaves the file
//! untouched.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Replace the status cell of the row whose name (and ticket) match.
///
/// Returns `Ok(true)` when the cell was rewritten, `Ok(false)` when no row
/// matched. The file is replaced atomically on success only.
#[allow(clippy::too_many_arguments)]
pub fn write_status(
    path: &Path,
    sheet: &str,
    header_row: u32,
    name_col: u32,
    ticket_col: Option<u32>,
    status_col: u32,
    name: &str,
    ticket: &str,
    status: &str,
) -> Result<bool> {
    let file =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("{} is not a valid zip package", path.display()))?;

    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let mut body = Vec::new();
        entry.read_to_end(&mut body)?;
        entries.push((entry.name().unwrap_or_default().to_string(), body));
    }
    drop(archive);

    let content_index = entries
        .iter()
        .position(|(name, _)| name == "content.xml")
        .context("the package has no content.xml")?;
    let xml =
        String::from_utf8(entries[content_index].1.clone()).context("content.xml is not UTF-8")?;

    let Some(updated) = rewrite(
        &xml, sheet, header_row, name_col, ticket_col, status_col, name, ticket, status,
    )?
    else {
        return Ok(false);
    };
    entries[content_index].1 = updated.into_bytes();

    let tmp = unique_tmp(path);
    {
        let file = std::fs::File::create(&tmp)
            .with_context(|| format!("cannot create {}", tmp.display()))?;
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in &entries {
            writer.start_file(name.as_str(), options)?;
            writer.write_all(body)?;
        }
        writer.finish()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(true)
}

fn unique_tmp(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".signifiage.tmp");
    path.with_file_name(name)
}

/// Rewrite the matching row's status cell inside one sheet.
#[allow(clippy::too_many_arguments)]
fn rewrite(
    xml: &str,
    sheet: &str,
    header_row: u32,
    name_col: u32,
    ticket_col: Option<u32>,
    status_col: u32,
    name: &str,
    ticket: &str,
    status: &str,
) -> Result<Option<String>> {
    let (table_start, table_end) =
        find_table(xml, sheet).with_context(|| format!("sheet {sheet} is not in the workbook"))?;
    let table = &xml[table_start..table_end];

    let mut cursor = 0usize;
    let mut row_number = 0u32;
    while let Some(row_start) = table[cursor..].find("<table:table-row").map(|i| cursor + i) {
        let Some(tag_end) = find_tag_end(table, row_start) else {
            break;
        };
        let self_closing = table[row_start..tag_end].ends_with("/>");
        let body_end = if self_closing {
            tag_end
        } else {
            match table[tag_end..].find("</table:table-row>") {
                Some(offset) => tag_end + offset,
                None => break,
            }
        };
        row_number += 1;
        if row_number > header_row && !self_closing {
            let body = &table[tag_end..body_end];
            if row_matches(body, name_col, ticket_col, name, ticket) {
                let Some(new_body) = set_status_cell(body, status_col, status) else {
                    return Ok(None);
                };
                let mut updated = String::with_capacity(xml.len() + 32);
                updated.push_str(&xml[..table_start + tag_end]);
                updated.push_str(&new_body);
                updated.push_str(&xml[table_start + body_end..]);
                return Ok(Some(updated));
            }
        }
        cursor = if self_closing {
            tag_end
        } else {
            body_end + "</table:table-row>".len()
        };
        if cursor >= table.len() {
            break;
        }
    }
    Ok(None)
}

/// Byte range of `<table:table ...>…</table:table>` for the requested sheet.
fn find_table(xml: &str, sheet: &str) -> Option<(usize, usize)> {
    let mut search = 0usize;
    while let Some(offset) = xml[search..].find("<table:table ") {
        let start = search + offset;
        let end = find_tag_end(xml, start)?;
        let tag = &xml[start..end];
        if let Some(name) = attribute(tag, "table:name")
            && name == sheet
        {
            let close = xml[end..].find("</table:table>")? + end;
            return Some((start, close));
        }
        search = end;
    }
    None
}

fn find_tag_end(xml: &str, start: usize) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (offset, ch) in xml[start..].char_indices() {
        match ch {
            '"' | '\'' if quote.is_none() => quote = Some(ch),
            '"' | '\'' if quote == Some(ch) => quote = None,
            '>' if quote.is_none() => return Some(start + offset + 1),
            _ => {}
        }
    }
    None
}

fn attribute(tag: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(unescape(&rest[..end]))
}

fn unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Iterate cells in a row body: `(start, end, columns_spanned, text)`.
fn cells(body: &str) -> Vec<Cell> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(offset) = body[cursor..].find("<table:") {
        let start = cursor + offset;
        let rest = &body[start..];
        let is_covered = rest.starts_with("<table:covered-table-cell");
        if !(rest.starts_with("<table:table-cell") || is_covered) {
            cursor = start + 7;
            continue;
        }
        let Some(tag_end) = find_tag_end(body, start) else {
            break;
        };
        let tag = &body[start..tag_end];
        let self_closing = tag.ends_with("/>");
        let close_tag = if is_covered {
            "</table:covered-table-cell>"
        } else {
            "</table:table-cell>"
        };
        let end = if self_closing {
            tag_end
        } else {
            match body[tag_end..].find(close_tag) {
                Some(found) => tag_end + found + close_tag.len(),
                None => break,
            }
        };
        let inner = &body[tag_end..end - if self_closing { 0 } else { close_tag.len() }];
        let span = attribute(tag, "table:number-columns-repeated")
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(1)
            .clamp(1, 1024);
        out.push(Cell {
            start,
            end,
            span,
            text: cell_text(inner),
        });
        cursor = end.max(start + 7);
        if cursor >= body.len() {
            break;
        }
    }
    out
}

struct Cell {
    start: usize,
    end: usize,
    span: u32,
    text: String,
}

fn cell_text(inner: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0usize;
    while let Some(offset) = inner[cursor..].find("<text:p") {
        let start = cursor + offset;
        let Some(tag_end) = find_tag_end(inner, start) else {
            break;
        };
        let self_closing = inner[start..tag_end].ends_with("/>");
        if self_closing {
            cursor = tag_end;
            continue;
        }
        let Some(close) = inner[tag_end..].find("</text:p>") else {
            break;
        };
        let text = strip_tags(&inner[tag_end..tag_end + close]);
        if !text.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&text);
        }
        cursor = tag_end + close + "</text:p>".len();
        if cursor >= inner.len() {
            break;
        }
    }
    out.trim().to_string()
}

fn strip_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            other if !in_tag => out.push(other),
            _ => {}
        }
    }
    unescape(&out)
}

fn row_matches(
    body: &str,
    name_col: u32,
    ticket_col: Option<u32>,
    name: &str,
    ticket: &str,
) -> bool {
    let cells = cells(body);
    let text_at = |wanted: u32| -> Option<&str> {
        let mut column = 0u32;
        for cell in &cells {
            if wanted < column + cell.span {
                return Some(cell.text.as_str());
            }
            column += cell.span;
        }
        None
    };
    let name_matches = text_at(name_col).is_some_and(|value| value.trim() == name.trim());
    if !name_matches {
        return false;
    }
    match ticket_col {
        Some(column) if !ticket.trim().is_empty() => {
            text_at(column).is_some_and(|value| value.trim() == ticket.trim())
        }
        _ => true,
    }
}

/// Replace the status cell with a plain string cell (calamine needs the string
/// value type to return the text again).
fn set_status_cell(body: &str, status_col: u32, status: &str) -> Option<String> {
    let cells = cells(body);
    let mut column = 0u32;
    let target = cells.iter().find(|cell| {
        let hit = status_col >= column && status_col < column + cell.span;
        column += cell.span;
        hit
    })?;
    let open_end = find_tag_end(body, target.start)?;
    let open = &body[target.start..open_end];
    let style = attribute(open, "table:style-name");
    let mut tag = String::from("<table:table-cell");
    if let Some(style) = style {
        tag.push_str(&format!(" table:style-name=\"{style}\""));
    }
    tag.push_str(" office:value-type=\"string\" calcext:value-type=\"string\">");
    let replacement = format!(
        "{}<text:p>{}</text:p></table:table-cell>",
        tag,
        escape(status)
    );
    let mut updated = String::with_capacity(body.len() + replacement.len());
    updated.push_str(&body[..target.start]);
    updated.push_str(&replacement);
    updated.push_str(&body[target.end..]);
    Some(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/queue.ods");
        let dir = std::env::temp_dir().join(format!("signifiage-ods-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("queue.ods");
        std::fs::copy(source, &target).unwrap();
        target
    }

    #[test]
    fn writes_the_status_of_a_matching_row() {
        let path = fixture();
        let wrote = write_status(
            &path,
            "signifiage-queue",
            1,
            1,
            Some(0),
            2,
            "Ayşe Yılmaz",
            "1",
            "called",
        )
        .unwrap();
        assert!(wrote, "row must be found");

        // The workbook still parses, and only that row's note changed.
        let mapping = crate::services::spreadsheet::ColumnMapping {
            name_col: 1,
            ticket_col: Some(0),
            note_col: Some(2),
            status_col: None,
        };
        let rows =
            crate::services::spreadsheet::read_import_rows(&path, "signifiage-queue", 1, &mapping)
                .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "Ayşe Yılmaz");
        assert_eq!(rows[0].note, "called");
        assert_eq!(rows[1].note, "Kontrol");
        assert_eq!(rows[2].note, "");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn unknown_row_changes_nothing() {
        let path = fixture();
        let before = std::fs::read(&path).unwrap();
        let wrote = write_status(
            &path,
            "signifiage-queue",
            1,
            1,
            Some(0),
            2,
            "Nobody",
            "99",
            "called",
        )
        .unwrap();
        assert!(!wrote);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn unknown_sheet_is_an_error() {
        let path = fixture();
        assert!(write_status(&path, "missing", 1, 0, None, 1, "x", "", "called").is_err());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn attribute_helpers_round_trip() {
        let tag = r#"<table:table-cell office:value-type="string" table:style-name="ce1">"#;
        assert_eq!(attribute(tag, "table:style-name").as_deref(), Some("ce1"));
        assert_eq!(
            attribute(tag, "office:value-type").as_deref(),
            Some("string")
        );
        assert_eq!(strip_tags("<text:p>a &amp; b</text:p>").trim(), "a & b");
    }
}
