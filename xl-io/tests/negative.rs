//! Negative / hardening tests: malformed input and the resource caps from
//! `Caps` must always produce a typed [`xl_io::IoError`], never a panic.

mod support;

use xl_io::{Caps, IoError};

#[test]
fn not_a_zip_is_a_zip_error() {
    let err = xl_io::from_bytes(b"this is definitely not a zip file").unwrap_err();
    assert!(
        matches!(err, IoError::Zip { .. }),
        "expected Zip error, got {err:?}"
    );
}

#[test]
fn truncated_zip_is_a_zip_error() {
    let full = support::minimal_xlsx(r#"<row r="1"><c r="A1"><v>1</v></c></row>"#);
    let truncated = &full[..full.len() / 2];
    let err = xl_io::from_bytes(truncated).unwrap_err();
    assert!(
        matches!(err, IoError::Zip { .. }),
        "expected Zip error, got {err:?}"
    );
}

#[test]
fn empty_input_is_a_zip_error() {
    let err = xl_io::from_bytes(&[]).unwrap_err();
    assert!(
        matches!(err, IoError::Zip { .. }),
        "expected Zip error, got {err:?}"
    );
}

#[test]
fn doctype_declaration_is_rejected() {
    let malicious_workbook = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<!DOCTYPE workbook [ <!ENTITY xxe SYSTEM "file:///etc/passwd"> ]>
{}"#,
        support::workbook_xml("Sheet1", "")
            .lines()
            .skip(1) // drop the XML declaration line, already added above
            .collect::<Vec<_>>()
            .join("\n")
    );
    let bytes = support::Fixture::new()
        .part("[Content_Types].xml", "<Types/>")
        .part("_rels/.rels", "<Relationships/>")
        .part("xl/workbook.xml", malicious_workbook)
        .part("xl/_rels/workbook.xml.rels", support::WORKBOOK_RELS)
        .part("xl/worksheets/sheet1.xml", support::sheet_xml(""))
        .build();
    let err = xl_io::from_bytes(&bytes).unwrap_err();
    match err {
        IoError::Doctype { ref part, offset } => {
            assert_eq!(part, "xl/workbook.xml");
            // The <!DOCTYPE sits on the second line, after the XML
            // declaration — its start offset must point past it, not at 0.
            assert!(
                offset > 0,
                "expected a nonzero DOCTYPE offset, got {offset}"
            );
        }
        other => panic!("expected Doctype error, got {other:?}"),
    }
}

#[test]
fn entity_reference_without_doctype_is_a_hard_error_not_a_guess() {
    // `&custom;` can never resolve: there is no DTD (rejected outright) to
    // declare it, and it isn't one of the five XML-predefined entities.
    let bytes = support::minimal_xlsx_named("Sheet1 &custom; Name", "", "");
    let err = xl_io::from_bytes(&bytes).unwrap_err();
    assert!(
        matches!(err, IoError::Xml { .. }),
        "expected Xml error, got {err:?}"
    );
}

#[test]
fn oversize_part_hits_single_part_cap() {
    let bytes = support::minimal_xlsx(r#"<row r="1"><c r="A1"><v>1</v></c></row>"#);
    let caps = Caps {
        max_single_part: 8,
        ..Caps::default()
    };
    let err = xl_io::from_bytes_with_caps(&bytes, caps).unwrap_err();
    assert!(
        matches!(
            err,
            IoError::Cap {
                kind: xl_io::CapKind::SinglePart,
                ..
            }
        ),
        "expected SinglePart cap error, got {err:?}"
    );
}

#[test]
fn oversize_total_uncompressed_hits_total_cap() {
    let bytes = support::minimal_xlsx(r#"<row r="1"><c r="A1"><v>1</v></c></row>"#);
    let caps = Caps {
        max_total_uncompressed: 8,
        ..Caps::default()
    };
    let err = xl_io::from_bytes_with_caps(&bytes, caps).unwrap_err();
    assert!(
        matches!(
            err,
            IoError::Cap {
                kind: xl_io::CapKind::TotalUncompressed,
                ..
            }
        ),
        "expected TotalUncompressed cap error, got {err:?}"
    );
}

#[test]
fn too_many_parts_hits_part_count_cap() {
    let bytes = support::Fixture::new()
        .part("[Content_Types].xml", "<Types/>")
        .part("_rels/.rels", "<Relationships/>")
        .part("xl/workbook.xml", support::workbook_xml("Sheet1", ""))
        .build();
    let caps = Caps {
        max_parts: 2,
        ..Caps::default()
    };
    let err = xl_io::from_bytes_with_caps(&bytes, caps).unwrap_err();
    assert!(
        matches!(
            err,
            IoError::Cap {
                kind: xl_io::CapKind::PartCount,
                ..
            }
        ),
        "expected PartCount cap error, got {err:?}"
    );
}

#[test]
fn high_compression_ratio_part_is_rejected() {
    // A long run of one repeated byte deflates extremely well; this part is
    // never even parsed as XML because the ratio check runs before content
    // is handed to `quick-xml` at all.
    let bomb = "A".repeat(2_000_000);
    let bytes = support::Fixture::new()
        .part("[Content_Types].xml", "<Types/>")
        .part("_rels/.rels", "<Relationships/>")
        .part_deflated("xl/workbook.xml", bomb)
        .part("xl/_rels/workbook.xml.rels", support::WORKBOOK_RELS)
        .part("xl/worksheets/sheet1.xml", support::sheet_xml(""))
        .build();
    let err = xl_io::from_bytes(&bytes).unwrap_err();
    assert!(
        matches!(
            err,
            IoError::Cap {
                kind: xl_io::CapKind::CompressionRatio,
                ..
            }
        ),
        "expected CompressionRatio cap error, got {err:?}"
    );
}

#[test]
fn xml_depth_bomb_is_rejected() {
    let depth = 300u32;
    let mut inner = String::new();
    for _ in 0..depth {
        inner.push_str("<a>");
    }
    for _ in 0..depth {
        inner.push_str("</a>");
    }
    let bomb_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">{inner}</worksheet>"#
    );
    let bytes = support::Fixture::new()
        .part("[Content_Types].xml", "<Types/>")
        .part("_rels/.rels", "<Relationships/>")
        .part("xl/workbook.xml", support::workbook_xml("Sheet1", ""))
        .part("xl/_rels/workbook.xml.rels", support::WORKBOOK_RELS)
        .part("xl/worksheets/sheet1.xml", bomb_xml)
        .build();
    let err = xl_io::from_bytes(&bytes).unwrap_err();
    assert!(
        matches!(
            err,
            IoError::Cap {
                kind: xl_io::CapKind::XmlDepth,
                ..
            }
        ),
        "expected XmlDepth cap error, got {err:?}"
    );
}

#[test]
fn xml_depth_within_cap_is_accepted() {
    let caps = Caps {
        max_xml_depth: 400,
        ..Caps::default()
    };
    let depth = 300u32;
    let mut inner = String::new();
    for _ in 0..depth {
        inner.push_str("<a>");
    }
    for _ in 0..depth {
        inner.push_str("</a>");
    }
    let bomb_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">{inner}</worksheet>"#
    );
    let bytes = support::Fixture::new()
        .part("[Content_Types].xml", "<Types/>")
        .part("_rels/.rels", "<Relationships/>")
        .part("xl/workbook.xml", support::workbook_xml("Sheet1", ""))
        .part("xl/_rels/workbook.xml.rels", support::WORKBOOK_RELS)
        .part("xl/worksheets/sheet1.xml", bomb_xml)
        .build();
    xl_io::from_bytes_with_caps(&bytes, caps).unwrap();
}

#[test]
fn duplicate_cell_reference_is_a_structure_error() {
    // OXP-060 (RUN-2026-07-11-oracle01): Excel refuses to open a package that
    // repeats a `<c r>` within one <sheetData>. xl-io mirrors that refusal as
    // a hard load error — never a silent last-wins/first-wins read of `2`/`1`.
    let bytes =
        support::minimal_xlsx(r#"<row r="1"><c r="A1"><v>1</v></c><c r="A1"><v>2</v></c></row>"#);
    let err = xl_io::from_bytes(&bytes).unwrap_err();
    match err {
        IoError::Structure { ref message, .. } => {
            // Distinguishable, not generic: it names the offending ref.
            assert!(
                message.contains("duplicate cell reference") && message.contains("A1"),
                "message should name the duplicate ref, got: {message}"
            );
        }
        other => panic!("expected Structure error, got {other:?}"),
    }
}

#[test]
fn malformed_cell_reference_is_a_structure_error() {
    let bytes = support::minimal_xlsx(r#"<row r="1"><c r="not-a-ref"><v>1</v></c></row>"#);
    let err = xl_io::from_bytes(&bytes).unwrap_err();
    assert!(matches!(err, IoError::Structure { .. }), "got {err:?}");
}

#[test]
fn missing_required_workbook_part_is_a_structure_error() {
    let bytes = support::Fixture::new()
        .part("[Content_Types].xml", "<Types/>")
        .part("_rels/.rels", "<Relationships/>")
        .build();
    let err = xl_io::from_bytes(&bytes).unwrap_err();
    assert!(matches!(err, IoError::Structure { .. }), "got {err:?}");
}

#[test]
fn shared_string_index_out_of_range_is_a_structure_error() {
    // No sharedStrings.xml present at all, but a cell claims a shared-string index.
    let bytes = support::minimal_xlsx(r#"<row r="1"><c r="A1" t="s"><v>0</v></c></row>"#);
    let err = xl_io::from_bytes(&bytes).unwrap_err();
    assert!(matches!(err, IoError::Structure { .. }), "got {err:?}");
}

#[test]
fn malformed_xml_never_panics_and_reports_an_offset() {
    let bytes = support::Fixture::new()
        .part("[Content_Types].xml", "<Types/>")
        .part("_rels/.rels", "<Relationships/>")
        .part("xl/workbook.xml", "<workbook><sheets><sheet")
        .build();
    let err = xl_io::from_bytes(&bytes).unwrap_err();
    match err {
        IoError::Xml {
            ref part, offset, ..
        } => {
            assert_eq!(part, "xl/workbook.xml");
            // quick-xml pins the syntax error to the start of the
            // unterminated `<sheet` markup (byte 18), not offset 0.
            assert!(offset > 0, "expected a nonzero error offset, got {offset}");
        }
        other => panic!("expected Xml error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Missing required attributes: each must be a typed Structure error naming
// the part, never a panic or a silently-defaulted value.
// ---------------------------------------------------------------------------

/// Builds a package whose `xl/workbook.xml` has the given `<sheets>` inner
/// XML verbatim (for malformed-`<sheet>` cases).
fn xlsx_with_raw_sheets_element(sheets_inner: &str) -> Vec<u8> {
    let workbook = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets>{sheets_inner}</sheets>
</workbook>"#
    );
    support::Fixture::new()
        .part("[Content_Types].xml", "<Types/>")
        .part("_rels/.rels", "<Relationships/>")
        .part("xl/workbook.xml", workbook)
        .part("xl/_rels/workbook.xml.rels", support::WORKBOOK_RELS)
        .part("xl/worksheets/sheet1.xml", support::sheet_xml(""))
        .build()
}

#[track_caller]
fn assert_structure_error_in(bytes: &[u8], expected_part: &str) {
    match xl_io::from_bytes(bytes).unwrap_err() {
        IoError::Structure { ref part, .. } => assert_eq!(part, expected_part),
        other => panic!("expected Structure error in `{expected_part}`, got {other:?}"),
    }
}

#[test]
fn sheet_missing_name_attribute_is_a_structure_error() {
    let bytes = xlsx_with_raw_sheets_element(r#"<sheet sheetId="1" r:id="rId1"/>"#);
    assert_structure_error_in(&bytes, "xl/workbook.xml");
}

#[test]
fn sheet_missing_sheet_id_attribute_is_a_structure_error() {
    let bytes = xlsx_with_raw_sheets_element(r#"<sheet name="Sheet1" r:id="rId1"/>"#);
    assert_structure_error_in(&bytes, "xl/workbook.xml");
}

#[test]
fn sheet_missing_r_id_attribute_is_skipped_not_an_error() {
    // Absent `r:id` means "no worksheet part" (same as `r:id=""`, the real
    // Excel veryHidden-VBA-sheet pattern — see xl-io/src/workbook_xml.rs and
    // fixtures.rs's `very_hidden_vba_sheets_with_empty_or_missing_r_id_*`
    // tests). The workbook still loads; the sheet simply contributes no
    // data and isn't in `wb.sheets`.
    let bytes = xlsx_with_raw_sheets_element(r#"<sheet name="Sheet1" sheetId="1"/>"#);
    let wb = xl_io::from_bytes(&bytes)
        .expect("a <sheet> with no r:id should be skipped, not reject the workbook");
    assert!(wb.sheets.is_empty());
    assert_eq!(wb.flags.skipped_sheets, 1);
}

#[test]
fn sheet_with_non_empty_dangling_r_id_is_still_a_structure_error() {
    // Distinct from the empty/absent case above: a *non-empty* `r:id` that
    // simply isn't in `xl/_rels/workbook.xml.rels` is a genuinely broken
    // reference, not the veryHidden-sheet pattern, and must still be
    // rejected.
    let bytes = xlsx_with_raw_sheets_element(r#"<sheet name="Sheet1" sheetId="1" r:id="rId404"/>"#);
    match xl_io::from_bytes(&bytes).unwrap_err() {
        IoError::Structure {
            ref part,
            ref message,
        } => {
            assert_eq!(part, "xl/workbook.xml");
            assert!(
                message.contains("rId404"),
                "expected message to name the dangling r:id, got: {message}"
            );
        }
        other => panic!("expected Structure error, got {other:?}"),
    }
}

#[test]
fn worksheet_typed_part_with_non_worksheet_root_still_errors() {
    // The skip-by-relationship-Type path (dialogsheet/chartsheet/macrosheet)
    // must NOT swallow a genuinely broken worksheet: if the relationship
    // Type *is* worksheet but the part's root isn't `<worksheet>`, that's a
    // corrupt worksheet and must still surface a Structure error rather than
    // being silently skipped.
    let workbook = support::workbook_xml("Sheet1", "");
    let malformed_part = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<dialogsheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></dialogsheet>"#;
    let bytes = support::Fixture::new()
        .part("[Content_Types].xml", "<Types/>")
        .part("_rels/.rels", "<Relationships/>")
        .part("xl/workbook.xml", workbook)
        // WORKBOOK_RELS wires rId1 with the *worksheet* relationship Type
        // pointing at worksheets/sheet1.xml.
        .part("xl/_rels/workbook.xml.rels", support::WORKBOOK_RELS)
        .part("xl/worksheets/sheet1.xml", malformed_part)
        .build();
    assert_structure_error_in(&bytes, "xl/worksheets/sheet1.xml");
}

fn xlsx_with_raw_workbook_rels(rels_xml: &str) -> Vec<u8> {
    support::Fixture::new()
        .part("[Content_Types].xml", "<Types/>")
        .part("_rels/.rels", "<Relationships/>")
        .part("xl/workbook.xml", support::workbook_xml("Sheet1", ""))
        .part("xl/_rels/workbook.xml.rels", rels_xml.to_string())
        .part("xl/worksheets/sheet1.xml", support::sheet_xml(""))
        .build()
}

#[test]
fn relationship_missing_id_attribute_is_a_structure_error() {
    let bytes = xlsx_with_raw_workbook_rels(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#,
    );
    assert_structure_error_in(&bytes, "xl/_rels/workbook.xml.rels");
}

#[test]
fn relationship_missing_target_attribute_is_a_structure_error() {
    let bytes = xlsx_with_raw_workbook_rels(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"/>
</Relationships>"#,
    );
    assert_structure_error_in(&bytes, "xl/_rels/workbook.xml.rels");
}

#[test]
fn cell_missing_r_attribute_is_a_structure_error() {
    // Both the content-bearing and the self-closing <c> forms must reject.
    let bytes = support::minimal_xlsx(r#"<row r="1"><c><v>1</v></c></row>"#);
    assert_structure_error_in(&bytes, "xl/worksheets/sheet1.xml");

    let bytes = support::minimal_xlsx(r#"<row r="1"><c s="0"/></row>"#);
    assert_structure_error_in(&bytes, "xl/worksheets/sheet1.xml");
}

#[test]
fn defined_name_missing_name_attribute_is_a_structure_error() {
    let bytes = support::minimal_xlsx_named(
        "Sheet1",
        r#"<definedNames><definedName>Sheet1!$A$1</definedName></definedNames>"#,
        "",
    );
    assert_structure_error_in(&bytes, "xl/workbook.xml");
}
