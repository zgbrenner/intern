use std::fs::File;
use std::io::Write;

use intern_worker::extract::{CancellationToken, extract_anydoc};
use intern_worker::limits::ResourceLimits;
use tempfile::tempdir;
use zip::write::SimpleFileOptions;

#[test]
fn generated_docx_is_converted_by_anydoc_0_1_8() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("employment-agreement.docx");
    let file = File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default();

    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
</Types>"#).unwrap();

    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#).unwrap();

    zip.start_file("word/_rels/document.xml.rels", options)
        .unwrap();
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#,
    )
    .unwrap();

    zip.start_file("word/styles.xml", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:outlineLvl w:val="0"/></w:style>
</w:styles>"#).unwrap();

    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Employment Agreement</w:t></w:r></w:p>
    <w:p><w:r><w:t>John Smith begins employment on April 12, 2024.</w:t></w:r></w:p>
    <w:p><w:r><w:t>This agreement describes the terms of employment.</w:t></w:r></w:p>
    <w:tbl>
      <w:tr><w:tc><w:p><w:r><w:t>Employer</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Acme Corporation</w:t></w:r></w:p></w:tc></w:tr>
      <w:tr><w:tc><w:p><w:r><w:t>Employee</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>John Smith</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl>
    <w:sectPr/>
  </w:body>
</w:document>"#).unwrap();
    zip.finish().unwrap();

    let extracted =
        extract_anydoc(&path, &ResourceLimits::default(), &CancellationToken::new()).unwrap();
    let markdown = &extracted.pages[0].text;

    assert!(markdown.contains("# Employment Agreement"), "{markdown}");
    assert!(markdown.contains("John Smith"), "{markdown}");
    assert!(markdown.contains("Acme Corporation"), "{markdown}");
}

/// A workbook that anydoc reads happily: one sheet, one cell.
fn write_workbook(path: &std::path::Path) {
    let file = File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default();

    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#).unwrap();

    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#).unwrap();

    zip.start_file("xl/workbook.xml", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets><sheet name="Ledger" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#).unwrap();

    zip.start_file("xl/_rels/workbook.xml.rels", options)
        .unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#).unwrap();

    zip.start_file("xl/worksheets/sheet1.xml", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Retainer balance</t></is></c></row></sheetData>
</worksheet>"#).unwrap();
    zip.finish().unwrap();
}

/// anydoc picks its parser from the file's content, so a workbook renamed
/// `.docx` reaches anydoc's Excel path — which renders every cell of every
/// sheet with none of the row and column caps spreadsheets are routed
/// through here. The extension decides which parser runs, so content that
/// disagrees with it is a routing failure rather than a document.
#[test]
fn an_xlsx_renamed_docx_is_rejected_not_rendered_uncapped() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("ledger.docx");
    write_workbook(&path);

    let error =
        extract_anydoc(&path, &ResourceLimits::default(), &CancellationToken::new()).unwrap_err();

    assert_eq!(error.code(), "UNSUPPORTED_FORMAT");
}

/// A PDF renamed `.pptx` skips the Office decompression check entirely and
/// lands in anydoc's PDF reader: no page cap, no OCR, no page image.
#[test]
fn a_pdf_renamed_pptx_is_rejected_before_any_parser_sees_it() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("scan.pptx");
    std::fs::write(&path, b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n").unwrap();

    let error =
        extract_anydoc(&path, &ResourceLimits::default(), &CancellationToken::new()).unwrap_err();

    assert_eq!(error.code(), "UNSUPPORTED_FORMAT");
}
