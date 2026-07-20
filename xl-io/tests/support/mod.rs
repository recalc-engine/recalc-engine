//! Test-only fixture builder: assembles minimal `.xlsx` packages **in the
//! test process** using the `zip` crate directly (no fixture files checked
//! in), the same spirit as `tools/corpus/tests/fixtures.py`'s Python
//! fixtures. Every OOXML part is written verbatim from a caller-supplied
//! string, so tests can also build deliberately malformed/hostile packages
//! for the negative/hardening cases.
//!
//! This module is compiled once per integration-test binary (`fixtures.rs`,
//! `negative.rs`, ...), each of which only exercises a subset of the
//! builder's surface — `dead_code` is expected and allowed here rather than
//! suppressed per-item.
#![allow(dead_code)]

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

/// A minimal, correct `[Content_Types].xml` covering workbook + worksheet +
/// (optionally) shared strings / styles. Kept generic (declares an
/// `Override` per extension via `Default`, which real OOXML doesn't do, but
/// nothing in `xl-io` reads `[Content_Types].xml`, so this only needs to be
/// present for the package to look plausible to `zip`/other tools — this
/// crate resolves parts via conventional paths + relationships, not content
/// types).
const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
</Types>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;

/// Builds a one-sheet workbook's `xl/workbook.xml`, naming the sheet
/// `sheet_name` and embedding `extra_xml` (e.g. `<workbookPr .../>`,
/// `<calcPr .../>`, `<definedNames>...</definedNames>`) right after
/// `<sheets>`.
pub fn workbook_xml(sheet_name: &str, extra_xml: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="{sheet_name}" sheetId="1" r:id="rId1"/></sheets>
{extra_xml}
</workbook>"#
    )
}

pub const WORKBOOK_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#;

/// A `.rels` that additionally points at shared strings / styles parts.
pub fn workbook_rels_with(shared_strings: bool, styles: bool) -> String {
    let mut rels = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
"#,
    );
    if shared_strings {
        rels.push_str(r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>"#);
        rels.push('\n');
    }
    if styles {
        rels.push_str(r#"<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#);
        rels.push('\n');
    }
    rels.push_str("</Relationships>");
    rels
}

pub fn sheet_xml(sheet_data_inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData>
{sheet_data_inner}
</sheetData>
</worksheet>"#
    )
}

/// A builder for assembling arbitrary parts into a zip byte buffer.
pub struct Fixture {
    parts: Vec<(String, Vec<u8>, CompressionMethod)>,
}

impl Fixture {
    pub fn new() -> Self {
        Fixture { parts: Vec::new() }
    }

    /// Adds a part, stored uncompressed (simplest for tests; production
    /// files typically use Deflate — see [`Fixture::part_deflated`] for
    /// tests that specifically need a compressed entry, e.g. the
    /// compression-ratio cap tests).
    pub fn part(mut self, name: &str, content: impl Into<Vec<u8>>) -> Self {
        self.parts
            .push((name.to_string(), content.into(), CompressionMethod::Stored));
        self
    }

    pub fn part_deflated(mut self, name: &str, content: impl Into<Vec<u8>>) -> Self {
        self.parts.push((
            name.to_string(),
            content.into(),
            CompressionMethod::Deflated,
        ));
        self
    }

    pub fn build(self) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content, method) in self.parts {
            let options = SimpleFileOptions::default().compression_method(method);
            writer.start_file(name, options).expect("start_file");
            writer.write_all(&content).expect("write_all");
        }
        writer.finish().expect("finish").into_inner()
    }
}

impl Default for Fixture {
    fn default() -> Self {
        Fixture::new()
    }
}

/// Assembles a complete, minimal, single-sheet `.xlsx` with the given
/// `<sheetData>` inner XML and no `sharedStrings.xml`/`styles.xml` (the
/// "missing optional parts" case is thus the *default* fixture, per the
/// task's test matrix).
pub fn minimal_xlsx(sheet_data_inner: &str) -> Vec<u8> {
    minimal_xlsx_named("Sheet1", "", sheet_data_inner)
}

/// Like [`minimal_xlsx`] but with a caller-chosen sheet name and extra XML
/// spliced into `<workbook>` (for `workbookPr`/`calcPr`/`definedNames`
/// tests).
pub fn minimal_xlsx_named(
    sheet_name: &str,
    workbook_extra_xml: &str,
    sheet_data_inner: &str,
) -> Vec<u8> {
    Fixture::new()
        .part("[Content_Types].xml", CONTENT_TYPES)
        .part("_rels/.rels", ROOT_RELS)
        .part(
            "xl/workbook.xml",
            workbook_xml(sheet_name, workbook_extra_xml),
        )
        .part("xl/_rels/workbook.xml.rels", WORKBOOK_RELS)
        .part("xl/worksheets/sheet1.xml", sheet_xml(sheet_data_inner))
        .build()
}

/// Like [`minimal_xlsx`] but also includes `sharedStrings.xml` and/or
/// `styles.xml`, wired up through the workbook relationships part.
#[allow(clippy::too_many_arguments)]
pub fn xlsx_with_parts(
    sheet_name: &str,
    workbook_extra_xml: &str,
    sheet_data_inner: &str,
    shared_strings_xml: Option<&str>,
    styles_xml: Option<&str>,
) -> Vec<u8> {
    let mut fx = Fixture::new()
        .part("[Content_Types].xml", CONTENT_TYPES)
        .part("_rels/.rels", ROOT_RELS)
        .part(
            "xl/workbook.xml",
            workbook_xml(sheet_name, workbook_extra_xml),
        )
        .part(
            "xl/_rels/workbook.xml.rels",
            workbook_rels_with(shared_strings_xml.is_some(), styles_xml.is_some()),
        )
        .part("xl/worksheets/sheet1.xml", sheet_xml(sheet_data_inner));
    if let Some(ss) = shared_strings_xml {
        fx = fx.part("xl/sharedStrings.xml", ss.to_string());
    }
    if let Some(st) = styles_xml {
        fx = fx.part("xl/styles.xml", st.to_string());
    }
    fx.build()
}
