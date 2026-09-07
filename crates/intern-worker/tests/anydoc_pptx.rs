use std::fs::File;
use std::io::Write;

use intern_worker::extract::{CancellationToken, extract_anydoc};
use intern_worker::limits::ResourceLimits;
use tempfile::tempdir;
use zip::write::SimpleFileOptions;

const SLIDE_NAMESPACES: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;

fn slide(paragraphs: &[&str]) -> String {
    let body = paragraphs
        .iter()
        .map(|text| format!("<a:p><a:r><a:rPr lang=\"en-US\"/><a:t>{text}</a:t></a:r></a:p>"))
        .collect::<String>();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld {SLIDE_NAMESPACES}><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/><p:sp><p:nvSpPr><p:cNvPr id="2" name="Content"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/>{body}</p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
    )
}

#[test]
fn a_generated_deck_is_read_slide_by_slide_by_anydoc() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("board-deck.pptx");
    let file = File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default();

    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
  <Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
  <Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
</Types>"#).unwrap();
    zip.start_file("_rels/.rels", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#).unwrap();
    zip.start_file("ppt/presentation.xml", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId3"/></p:sldIdLst>
  <p:sldSz cx="12192000" cy="6858000"/>
</p:presentation>"#).unwrap();
    zip.start_file("ppt/_rels/presentation.xml.rels", options)
        .unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/>
</Relationships>"#).unwrap();
    zip.start_file("ppt/slides/slide1.xml", options).unwrap();
    zip.write_all(
        slide(&[
            "Quarterly Business Review",
            "Prepared for Vistage Worldwide, Inc.",
            "Presented May 21, 2026",
        ])
        .as_bytes(),
    )
    .unwrap();
    zip.start_file("ppt/slides/slide2.xml", options).unwrap();
    zip.write_all(
        slide(&[
            "Agenda",
            "Member map renewals",
            "Next quarterly review: August 20, 2026",
        ])
        .as_bytes(),
    )
    .unwrap();
    zip.finish().unwrap();

    let extracted =
        extract_anydoc(&path, &ResourceLimits::default(), &CancellationToken::new()).unwrap();
    let markdown = &extracted.pages[0].text;
    assert!(markdown.contains("Quarterly Business Review"), "{markdown}");
    assert!(markdown.contains("Vistage Worldwide, Inc."), "{markdown}");
    assert!(markdown.contains("May 21, 2026"), "{markdown}");
    let first = markdown.find("Quarterly Business Review").unwrap();
    let second = markdown.find("Next quarterly review").unwrap();
    assert!(first < second, "slides come out in order: {markdown}");
}
