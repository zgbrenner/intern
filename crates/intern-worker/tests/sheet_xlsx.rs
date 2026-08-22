use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use intern_worker::extract::{CancellationToken, PageSource};
use intern_worker::limits::ResourceLimits;
use intern_worker::sheet::{MAX_SHEET_COLS, MAX_SHEET_ROWS, extract_xlsx};
use tempfile::TempDir;
use zip::write::SimpleFileOptions;

/// Writes a minimal calamine-readable workbook whose sheets carry the given
/// `<sheetData>` inner XML.
fn write_xlsx(directory: &TempDir, sheets: &[(&str, String)]) -> PathBuf {
    let path = directory.path().join("workbook.xlsx");
    let file = File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default();

    let mut content_types = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>"#,
    );
    for index in 1..=sheets.len() {
        content_types.push_str(&format!(
            "\n  <Override PartName=\"/xl/worksheets/sheet{index}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"
        ));
    }
    content_types.push_str("\n</Types>");
    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(content_types.as_bytes()).unwrap();

    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#).unwrap();

    let mut workbook = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>"#,
    );
    let mut workbook_rels = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    for (index, (name, _)) in sheets.iter().enumerate() {
        let id = index + 1;
        workbook.push_str(&format!(
            "\n    <sheet name=\"{name}\" sheetId=\"{id}\" r:id=\"rId{id}\"/>"
        ));
        workbook_rels.push_str(&format!(
            "\n  <Relationship Id=\"rId{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{id}.xml\"/>"
        ));
    }
    workbook.push_str("\n  </sheets>\n</workbook>");
    workbook_rels.push_str("\n</Relationships>");
    zip.start_file("xl/workbook.xml", options).unwrap();
    zip.write_all(workbook.as_bytes()).unwrap();
    zip.start_file("xl/_rels/workbook.xml.rels", options)
        .unwrap();
    zip.write_all(workbook_rels.as_bytes()).unwrap();

    for (index, (_, sheet_data)) in sheets.iter().enumerate() {
        zip.start_file(format!("xl/worksheets/sheet{}.xml", index + 1), options)
            .unwrap();
        zip.write_all(
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>{sheet_data}</sheetData>
</worksheet>"#
            )
            .as_bytes(),
        )
        .unwrap();
    }
    zip.finish().unwrap();
    path
}

fn column_reference(mut index: usize) -> String {
    let mut reference = String::new();
    loop {
        reference.insert(0, (b'A' + (index % 26) as u8) as char);
        if index < 26 {
            return reference;
        }
        index = index / 26 - 1;
    }
}

/// `<sheetData>` rows of inline strings.
fn inline_rows(rows: &[Vec<String>]) -> String {
    let mut data = String::new();
    for (row_index, row) in rows.iter().enumerate() {
        data.push_str(&format!("<row r=\"{}\">", row_index + 1));
        for (col_index, value) in row.iter().enumerate() {
            data.push_str(&format!(
                "<c r=\"{}{}\" t=\"inlineStr\"><is><t>{value}</t></is></c>",
                column_reference(col_index),
                row_index + 1
            ));
        }
        data.push_str("</row>");
    }
    data
}

#[test]
fn each_nonempty_sheet_becomes_one_markdown_page_with_its_name_as_heading() {
    let directory = tempfile::tempdir().unwrap();
    let path = write_xlsx(
        &directory,
        &[
            (
                "Invoices",
                inline_rows(&[
                    vec!["Invoice".to_owned(), "Amount".to_owned()],
                    vec!["INV-2048".to_owned(), "1200".to_owned()],
                ]),
            ),
            ("Blank", String::new()),
            (
                "Notes",
                inline_rows(&[vec!["Reviewed by finance".to_owned()]]),
            ),
        ],
    );

    let document =
        extract_xlsx(&path, &ResourceLimits::default(), &CancellationToken::new()).unwrap();

    assert_eq!(document.pages.len(), 2);
    assert_eq!(document.pages[0].page_number, 1);
    assert_eq!(document.pages[1].page_number, 2);
    assert!(
        document
            .pages
            .iter()
            .all(|page| page.source == PageSource::AnyDoc)
    );
    assert_eq!(
        document.pages[0].text,
        "## Invoices\n\n\
         | Invoice | Amount |\n\
         | --- | --- |\n\
         | INV-2048 | 1200 |\n"
    );
    assert_eq!(
        document.pages[1].text,
        "## Notes\n\n| Reviewed by finance |\n| --- |\n"
    );
    assert!(!document.truncated);
    assert!(document.warnings.is_empty());
}

#[test]
fn rows_beyond_the_cap_are_elided_with_a_marker_instead_of_flooding_the_page() {
    let rows = (0..MAX_SHEET_ROWS + 25)
        .map(|index| vec![format!("row {}", index + 1)])
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().unwrap();
    let path = write_xlsx(&directory, &[("Ledger", inline_rows(&rows))]);

    let document =
        extract_xlsx(&path, &ResourceLimits::default(), &CancellationToken::new()).unwrap();
    let text = &document.pages[0].text;

    assert!(
        text.contains(&format!("| row {} |", MAX_SHEET_ROWS)),
        "{text}"
    );
    assert!(
        !text.contains(&format!("| row {} |", MAX_SHEET_ROWS + 1)),
        "{text}"
    );
    assert!(text.ends_with("[... 25 more rows not shown]\n"), "{text}");
    assert!(document.truncated);
    assert_eq!(
        document.warnings,
        vec![intern_worker::extract::ExtractionWarning::TextTruncated]
    );
}

#[test]
fn columns_beyond_the_cap_are_elided_with_a_marker() {
    let wide_row = (0..MAX_SHEET_COLS + 5)
        .map(|index| format!("col {}", index + 1))
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().unwrap();
    let path = write_xlsx(&directory, &[("Wide", inline_rows(&[wide_row]))]);

    let document =
        extract_xlsx(&path, &ResourceLimits::default(), &CancellationToken::new()).unwrap();
    let text = &document.pages[0].text;

    assert!(
        text.contains(&format!("| col {} |", MAX_SHEET_COLS)),
        "{text}"
    );
    assert!(
        !text.contains(&format!("col {}", MAX_SHEET_COLS + 1)),
        "{text}"
    );
    assert!(text.ends_with("[... 5 more columns not shown]\n"), "{text}");
    assert!(document.truncated);
}

#[test]
fn empty_cells_collapse_and_formulas_surface_their_cached_values() {
    let sheet_data = concat!(
        "<row r=\"1\">",
        "<c r=\"A1\" t=\"inlineStr\"><is><t>Total</t></is></c>",
        "<c r=\"C1\"><f>1+1</f><v>2</v></c>",
        "</row>",
    );
    let directory = tempfile::tempdir().unwrap();
    let path = write_xlsx(&directory, &[("Summary", sheet_data.to_owned())]);

    let document =
        extract_xlsx(&path, &ResourceLimits::default(), &CancellationToken::new()).unwrap();

    assert_eq!(
        document.pages[0].text,
        "## Summary\n\n| Total |  | 2 |\n| --- | --- | --- |\n"
    );
}

#[test]
fn a_workbook_with_no_readable_cells_is_reported_as_unsupported() {
    let directory = tempfile::tempdir().unwrap();
    let path = write_xlsx(&directory, &[("Empty", String::new())]);

    let error =
        extract_xlsx(&path, &ResourceLimits::default(), &CancellationToken::new()).unwrap_err();

    assert_eq!(error.code(), "UNSUPPORTED_FORMAT");
}

#[test]
fn a_corrupt_workbook_reports_a_parse_error_instead_of_panicking() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("corrupt.xlsx");
    std::fs::write(&path, b"this is not a zip archive at all").unwrap();

    let error =
        extract_xlsx(&path, &ResourceLimits::default(), &CancellationToken::new()).unwrap_err();

    assert_eq!(error.code(), "PARSE_FAILED");
    assert!(!error.retryable());
}
