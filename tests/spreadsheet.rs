//! Spreadsheet import tests: fixtures, header guessing, display formatting and
//! the row cap.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use calamine::{CellErrorType, Data, ExcelDateTime, ExcelDateTimeType};
use signifiage::model::QueueStatus;
use signifiage::services::spreadsheet::{
    ColumnMapping, MAX_ROWS, SheetError, display_string, guess_mapping, preview, read_import_rows,
    sheet_names,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn temp_path(tag: &str, file: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "signifiage-spreadsheet-{tag}-{}",
        signifiage::model::new_id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir.join(file)
}

const EXPECTED_HEADERS: [&str; 3] = ["No", "Ad Soyad", "Not"];

#[test]
fn fixtures_expose_a_single_named_sheet() {
    for name in ["queue.xlsx", "queue.ods"] {
        let path = fixture(name);
        let sheets = sheet_names(&path).unwrap();
        assert_eq!(sheets.len(), 1, "{name} sheets: {sheets:?}");
        assert!(!sheets[0].is_empty(), "{name} sheet name is empty");

        let guessed = preview(&path, None).unwrap();
        assert_eq!(guessed.sheets, sheets, "{name}");
        assert_eq!(guessed.sheet, sheets[0], "{name}");
    }
}

#[test]
fn preview_finds_header_and_guesses_mapping() {
    for name in ["queue.xlsx", "queue.ods"] {
        let path = fixture(name);
        let sheet = sheet_names(&path).unwrap().remove(0);
        let view = preview(&path, Some(&sheet)).unwrap();

        assert_eq!(view.sheet, sheet);
        assert_eq!(view.header_row, 1, "{name}");
        assert_eq!(view.headers, EXPECTED_HEADERS, "{name}");
        assert_eq!(
            guess_mapping(&view.headers),
            ColumnMapping {
                name_col: 1,
                ticket_col: Some(0),
                note_col: Some(2),
                status_col: None,
            },
            "{name}"
        );
        assert_eq!(view.rows.len(), 3, "{name}");
        assert_eq!(view.rows[0], ["1", "Ayşe Yılmaz", "İlk çağrı"], "{name}");
        assert_eq!(view.rows[1], ["2", "Mehmet Öztürk", "Kontrol"], "{name}");
        assert_eq!(view.rows[2], ["3", "Zeynep Şahin", ""], "{name}");
    }
}

#[test]
fn imports_turkish_rows_from_both_formats() {
    for name in ["queue.xlsx", "queue.ods"] {
        let path = fixture(name);
        let sheet = sheet_names(&path).unwrap().remove(0);
        let mapping = guess_mapping(&EXPECTED_HEADERS.map(str::to_string));
        let rows = read_import_rows(&path, &sheet, 1, &mapping).unwrap();

        assert_eq!(rows.len(), 3, "{name}");
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(
            names,
            ["Ayşe Yılmaz", "Mehmet Öztürk", "Zeynep Şahin"],
            "{name}"
        );
        let tickets: Vec<&str> = rows.iter().map(|row| row.ticket.as_str()).collect();
        assert_eq!(tickets, ["1", "2", "3"], "{name}");
        let notes: Vec<&str> = rows.iter().map(|row| row.note.as_str()).collect();
        assert_eq!(notes, ["İlk çağrı", "Kontrol", ""], "{name}");
        let keys: Vec<&str> = rows.iter().map(|row| row.source_key.as_str()).collect();
        assert_eq!(keys, ["1", "2", "3"], "{name}");
        assert!(
            rows.iter().all(|row| row.status.is_none()),
            "{name} has a status without a mapped column"
        );
    }
}

#[test]
fn unmapped_optional_columns_are_ignored() {
    let path = fixture("queue.xlsx");
    let sheet = sheet_names(&path).unwrap().remove(0);
    let mapping = ColumnMapping {
        name_col: 1,
        ticket_col: None,
        note_col: None,
        status_col: None,
    };
    let rows = read_import_rows(&path, &sheet, 1, &mapping).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].ticket, "");
    assert_eq!(rows[0].note, "");
    assert_eq!(rows[0].status, None);
    assert_eq!(rows[0].source_key, "ayşe yılmaz");
}

#[test]
fn missing_sheet_and_bad_column_are_reported() {
    let path = fixture("queue.xlsx");
    let sheet = sheet_names(&path).unwrap().remove(0);

    match preview(&path, Some("does-not-exist")) {
        Err(SheetError::UnknownSheet(name)) => assert_eq!(name, "does-not-exist"),
        other => panic!("expected UnknownSheet, got {other:?}"),
    }

    let mapping = ColumnMapping {
        name_col: 9,
        ticket_col: None,
        note_col: None,
        status_col: None,
    };
    match read_import_rows(&path, &sheet, 1, &mapping) {
        Err(SheetError::BadColumn(col)) => assert_eq!(col, 9),
        other => panic!("expected BadColumn, got {other:?}"),
    }

    let missing = fixture("nope.xlsx");
    match sheet_names(&missing) {
        Err(SheetError::Io { path, .. }) => assert!(path.ends_with("nope.xlsx"), "{path}"),
        other => panic!("expected Io, got {other:?}"),
    }
}

#[test]
fn display_string_normalizes_cell_values() {
    assert_eq!(
        display_string(&Data::String("  Ayşe \n\t Yılmaz  ".to_string())),
        "Ayşe Yılmaz"
    );
    assert_eq!(display_string(&Data::String(String::new())), "");
    assert_eq!(display_string(&Data::Int(42)), "42");
    assert_eq!(display_string(&Data::Float(3.0)), "3");
    assert_eq!(display_string(&Data::Float(-4.0)), "-4");
    assert_eq!(display_string(&Data::Float(-0.0)), "0");
    assert_eq!(display_string(&Data::Float(2.5)), "2.5");
    assert_eq!(display_string(&Data::Float(0.1)), "0.1");
    assert_eq!(display_string(&Data::Bool(true)), "true");
    assert_eq!(display_string(&Data::Bool(false)), "false");
    assert_eq!(display_string(&Data::Empty), "");
    assert_eq!(display_string(&Data::Error(CellErrorType::Div0)), "");
    assert_eq!(
        display_string(&Data::DateTimeIso("2026-11-17T10:30:00".to_string())),
        "2026-11-17"
    );
    assert_eq!(
        display_string(&Data::DurationIso("PT1H30M".to_string())),
        "PT1H30M"
    );
    let serial = ExcelDateTime::new(46000.0, ExcelDateTimeType::DateTime, false);
    assert_eq!(display_string(&Data::DateTime(serial)), "2025-12-09");
}

#[test]
fn guess_mapping_handles_english_and_turkish_headers() {
    let headers = vec![
        "Name".to_string(),
        "Ticket".to_string(),
        "Note".to_string(),
        "Status".to_string(),
    ];
    assert_eq!(
        guess_mapping(&headers),
        ColumnMapping {
            name_col: 0,
            ticket_col: Some(1),
            note_col: Some(2),
            status_col: Some(3),
        }
    );

    let headers = vec![
        "No".to_string(),
        "Ad Soyad".to_string(),
        "Açıklama".to_string(),
        "Durum".to_string(),
    ];
    assert_eq!(
        guess_mapping(&headers),
        ColumnMapping {
            name_col: 1,
            ticket_col: Some(0),
            note_col: Some(2),
            status_col: Some(3),
        }
    );

    let headers = vec!["Unrelated".to_string(), "İsim".to_string()];
    assert_eq!(
        guess_mapping(&headers),
        ColumnMapping {
            name_col: 1,
            ticket_col: None,
            note_col: None,
            status_col: None,
        }
    );

    // No recognizable header: the name column falls back to the first one.
    assert_eq!(
        guess_mapping(&[]),
        ColumnMapping {
            name_col: 0,
            ticket_col: None,
            note_col: None,
            status_col: None,
        }
    );
}

#[test]
fn read_rows_parses_explicit_status_column() {
    let path = temp_path("status", "status.xlsx");
    write_workbook(
        &path,
        &[
            &["Ad Soyad", "No", "Not", "Durum"],
            &["Ayşe Yılmaz", "7", "İlk", "Bekliyor"],
            &["Mehmet Öztürk", "8", "", "çağrıldı"],
            &["", "9", "no name", "tamamlandı"],
        ],
    );
    let mapping = guess_mapping(&[
        "Ad Soyad".to_string(),
        "No".to_string(),
        "Not".to_string(),
        "Durum".to_string(),
    ]);
    let rows = read_import_rows(&path, "Sheet1", 1, &mapping).unwrap();
    assert_eq!(rows.len(), 2, "unnamed row must be skipped");
    assert_eq!(rows[0].status, Some(QueueStatus::Waiting));
    assert_eq!(rows[1].status, Some(QueueStatus::Called));
    assert_eq!(rows[1].source_key, "8");
    cleanup(&path);
}

#[test]
fn too_many_rows_is_rejected() {
    let path = temp_path("big", "big.xlsx");
    write_row_workbook(&path, MAX_ROWS + 1);

    let mapping = ColumnMapping {
        name_col: 0,
        ticket_col: None,
        note_col: None,
        status_col: None,
    };
    match read_import_rows(&path, "Sheet1", 1, &mapping) {
        Err(SheetError::TooManyRows { rows }) => assert_eq!(rows, MAX_ROWS + 1),
        other => panic!("expected TooManyRows, got {other:?}"),
    }
    match preview(&path, None) {
        Err(SheetError::TooManyRows { rows }) => assert_eq!(rows, MAX_ROWS + 1),
        other => panic!("expected TooManyRows from preview, got {other:?}"),
    }
    cleanup(&path);
}

fn cleanup(path: &Path) {
    if let Some(dir) = path.parent() {
        fs::remove_dir_all(dir).ok();
    }
}

/// Write a minimal single-sheet `.xlsx` with a header plus the given rows.
fn write_workbook(path: &Path, rows: &[&[&str]]) {
    let mut sheet = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#,
    );
    for (row_index, values) in rows.iter().enumerate() {
        let number = row_index + 1;
        sheet.push_str(&format!(r#"<row r="{number}">"#));
        for (col_index, value) in values.iter().enumerate() {
            let column = (b'A' + col_index as u8) as char;
            let escaped = value
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            sheet.push_str(&format!(
                r#"<c r="{column}{number}" t="inlineStr"><is><t>{escaped}</t></is></c>"#
            ));
        }
        sheet.push_str("</row>");
    }
    sheet.push_str("</sheetData></worksheet>");
    write_xlsx(path, &sheet);
}

/// Write a worksheet with one header row and `data_rows` generated names.
fn write_row_workbook(path: &Path, data_rows: usize) {
    let mut sheet = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Ad Soyad</t></is></c></row>"#,
    );
    for index in 0..data_rows {
        let number = index + 2;
        sheet.push_str(&format!(
            r#"<row r="{number}"><c r="A{number}" t="inlineStr"><is><t>Person {number}</t></is></c></row>"#
        ));
    }
    sheet.push_str("</sheetData></worksheet>");
    write_xlsx(path, &sheet);
}

fn write_xlsx(path: &Path, sheet_xml: &str) {
    let file = File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();

    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#,
    )
    .unwrap();

    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#,
    )
    .unwrap();

    zip.start_file("xl/workbook.xml", options).unwrap();
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#,
    )
    .unwrap();

    zip.start_file("xl/_rels/workbook.xml.rels", options)
        .unwrap();
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#,
    )
    .unwrap();

    zip.start_file("xl/worksheets/sheet1.xml", options).unwrap();
    zip.write_all(sheet_xml.as_bytes()).unwrap();
    zip.finish().unwrap();
}
