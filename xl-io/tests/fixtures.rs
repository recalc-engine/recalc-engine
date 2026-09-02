//! Fixture-based tests: builds minimal `.xlsx` packages in-process (see
//! `tests/support/mod.rs`) covering the parsing matrix required by Task 5 —
//! shared/inline/number/bool/error cells, rich-text runs, the 1904 flag,
//! `calcPr`, defined names, numFmt resolution, missing optional parts, and
//! case-insensitive sheet lookup.

mod support;

use xl_value::{ErrorKind, Value};

#[test]
fn number_cell_is_a_finite_f64() {
    let bytes = support::minimal_xlsx(r#"<row r="1"><c r="A1"><v>42.5</v></c></row>"#);
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();
    let cell = sheet.cell(0, 0).unwrap();
    assert_eq!(cell.value, Value::Number(42.5));
    assert!(cell.formula.is_none());
}

#[test]
fn bool_cell_true_and_false() {
    let bytes = support::minimal_xlsx(
        r#"<row r="1"><c r="A1" t="b"><v>1</v></c><c r="B1" t="b"><v>0</v></c></row>"#,
    );
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();
    assert_eq!(sheet.cell(0, 0).unwrap().value, Value::Bool(true));
    assert_eq!(sheet.cell(0, 1).unwrap().value, Value::Bool(false));
}

#[test]
fn error_cells_map_to_error_kind() {
    let bytes = support::minimal_xlsx(
        r#"<row r="1">
            <c r="A1" t="e"><v>#DIV/0!</v></c>
            <c r="B1" t="e"><v>#N/A</v></c>
            <c r="C1" t="e"><v>#NAME?</v></c>
            <c r="D1" t="e"><v>#REF!</v></c>
        </row>"#,
    );
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();
    assert_eq!(
        sheet.cell(0, 0).unwrap().value,
        Value::Error(ErrorKind::Div0)
    );
    assert_eq!(sheet.cell(0, 1).unwrap().value, Value::Error(ErrorKind::Na));
    assert_eq!(
        sheet.cell(0, 2).unwrap().value,
        Value::Error(ErrorKind::Name)
    );
    assert_eq!(
        sheet.cell(0, 3).unwrap().value,
        Value::Error(ErrorKind::Ref)
    );
}

#[test]
fn unrecognized_error_string_is_unsupported_sentinel_not_a_guess() {
    let bytes = support::minimal_xlsx(r#"<row r="1"><c r="A1" t="e"><v>#WEIRD!</v></c></row>"#);
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();
    assert_eq!(
        sheet.cell(0, 0).unwrap().value,
        Value::Error(ErrorKind::Unsupported)
    );
}

#[test]
fn inline_string_cell() {
    let bytes = support::minimal_xlsx(
        r#"<row r="1"><c r="A1" t="inlineStr"><is><t>Hello, world</t></is></c></row>"#,
    );
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();
    assert_eq!(sheet.cell(0, 0).unwrap().value, Value::text("Hello, world"));
}

#[test]
fn inline_string_with_rich_text_runs() {
    let bytes = support::minimal_xlsx(
        r#"<row r="1"><c r="A1" t="inlineStr"><is><r><t>Hello, </t></r><r><rPr><b/></rPr><t>world</t></r></is></c></row>"#,
    );
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();
    assert_eq!(sheet.cell(0, 0).unwrap().value, Value::text("Hello, world"));
}

#[test]
fn shared_string_cell_resolves_by_index() {
    let shared_strings = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="2" uniqueCount="2">
<si><t>Zero</t></si>
<si><t>One</t></si>
</sst>"#;
    let bytes = support::xlsx_with_parts(
        "Sheet1",
        "",
        r#"<row r="1"><c r="A1" t="s"><v>1</v></c><c r="B1" t="s"><v>0</v></c></row>"#,
        Some(shared_strings),
        None,
    );
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();
    assert_eq!(sheet.cell(0, 0).unwrap().value, Value::text("One"));
    assert_eq!(sheet.cell(0, 1).unwrap().value, Value::text("Zero"));
}

#[test]
fn shared_string_rich_text_runs_flatten_and_preserve_whitespace() {
    let shared_strings = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1">
<si><r><t>Bold</t></r><r><t xml:space="preserve"> and </t></r><r><t>italic</t></r></si>
</sst>"#;
    let bytes = support::xlsx_with_parts(
        "Sheet1",
        "",
        r#"<row r="1"><c r="A1" t="s"><v>0</v></c></row>"#,
        Some(shared_strings),
        None,
    );
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();
    assert_eq!(
        sheet.cell(0, 0).unwrap().value,
        Value::text("Bold and italic")
    );
}

#[test]
fn shared_string_preserved_leading_trailing_whitespace() {
    let shared_strings = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1">
<si><t xml:space="preserve">  padded  </t></si>
</sst>"#;
    let bytes = support::xlsx_with_parts(
        "Sheet1",
        "",
        r#"<row r="1"><c r="A1" t="s"><v>0</v></c></row>"#,
        Some(shared_strings),
        None,
    );
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();
    assert_eq!(sheet.cell(0, 0).unwrap().value, Value::text("  padded  "));
}

#[test]
fn formula_cell_stores_raw_text_and_cached_value() {
    let bytes = support::minimal_xlsx(r#"<row r="1"><c r="A1"><f>1+2</f><v>3</v></c></row>"#);
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();
    let cell = sheet.cell(0, 0).unwrap();
    assert_eq!(cell.value, Value::Number(3.0));
    let f = cell.formula.as_ref().unwrap();
    assert_eq!(f.text.as_deref(), Some("1+2"));
    assert_eq!(f.kind, xl_io::FormulaKind::Normal);
}

#[test]
fn shared_formula_group_master_and_follow_on() {
    let bytes = support::minimal_xlsx(
        r#"<row r="1">
            <c r="A1"><f t="shared" ref="A1:A2" si="0">1</f><v>1</v></c>
        </row>
        <row r="2">
            <c r="A2"><f t="shared" si="0"/><v>2</v></c>
        </row>"#,
    );
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();

    let master = sheet.cell(0, 0).unwrap().formula.as_ref().unwrap();
    assert_eq!(master.kind, xl_io::FormulaKind::Shared);
    assert_eq!(master.shared_index, Some(0));
    assert_eq!(master.range.as_deref(), Some("A1:A2"));
    assert_eq!(master.text.as_deref(), Some("1"));

    let follow_on = sheet.cell(1, 0).unwrap().formula.as_ref().unwrap();
    assert_eq!(follow_on.kind, xl_io::FormulaKind::Shared);
    assert_eq!(follow_on.shared_index, Some(0));
    assert_eq!(follow_on.text, None);
}

#[test]
fn blank_styled_cell_has_blank_value() {
    let bytes = support::minimal_xlsx(r#"<row r="1"><c r="A1" s="0"/></row>"#);
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();
    assert_eq!(sheet.cell(0, 0).unwrap().value, Value::Blank);
}

#[test]
fn date_system_defaults_to_1900() {
    let bytes = support::minimal_xlsx("");
    let wb = xl_io::from_bytes(&bytes).unwrap();
    assert_eq!(wb.date_system, xl_io::DateSystem::Excel1900);
}

#[test]
fn date_system_1904_flag() {
    let bytes = support::minimal_xlsx_named("Sheet1", r#"<workbookPr date1904="1"/>"#, "");
    let wb = xl_io::from_bytes(&bytes).unwrap();
    assert_eq!(wb.date_system, xl_io::DateSystem::Excel1904);
}

#[test]
fn calc_pr_settings_are_read() {
    let bytes = support::minimal_xlsx_named(
        "Sheet1",
        r#"<calcPr calcMode="manual" iterate="1" iterateCount="50" iterateDelta="0.01" fullCalcOnLoad="1"/>"#,
        "",
    );
    let wb = xl_io::from_bytes(&bytes).unwrap();
    assert_eq!(wb.calc_settings.calc_mode, xl_io::CalcMode::Manual);
    assert!(wb.calc_settings.iterate);
    assert_eq!(wb.calc_settings.iterate_count, 50);
    assert_eq!(wb.calc_settings.iterate_delta, 0.01);
    assert!(wb.calc_settings.full_calc_on_load);
}

#[test]
fn calc_pr_defaults_when_absent() {
    let bytes = support::minimal_xlsx("");
    let wb = xl_io::from_bytes(&bytes).unwrap();
    assert_eq!(wb.calc_settings.calc_mode, xl_io::CalcMode::Auto);
    assert!(!wb.calc_settings.iterate);
    assert_eq!(wb.calc_settings.iterate_count, 100);
    assert_eq!(wb.calc_settings.iterate_delta, 0.001);
    assert!(!wb.calc_settings.full_calc_on_load);
}

#[test]
fn defined_names_workbook_and_sheet_scoped() {
    let extra = r#"<definedNames>
        <definedName name="Global">Sheet1!$A$1:$A$10</definedName>
        <definedName name="Local" localSheetId="0">Sheet1!$B$1</definedName>
    </definedNames>"#;
    let bytes = support::minimal_xlsx_named("Sheet1", extra, "");
    let wb = xl_io::from_bytes(&bytes).unwrap();
    assert_eq!(wb.defined_names.len(), 2);
    let global = wb
        .defined_names
        .iter()
        .find(|d| d.name == "Global")
        .unwrap();
    assert_eq!(global.formula, "Sheet1!$A$1:$A$10");
    assert_eq!(global.sheet_scope, None);
    let local = wb.defined_names.iter().find(|d| d.name == "Local").unwrap();
    assert_eq!(local.formula, "Sheet1!$B$1");
    assert_eq!(local.sheet_scope, Some(0));
    // With no skipped sheets, the collection index equals the tab position.
    assert_eq!(wb.sheet_at(0).unwrap().sheets_index, 0);
}

#[test]
fn num_fmt_resolves_custom_format_code() {
    let styles = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<numFmts count="1"><numFmt numFmtId="167" formatCode="0.00%"/></numFmts>
<cellXfs count="2">
<xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>
<xf numFmtId="167" fontId="0" fillId="0" borderId="0" applyNumberFormat="1"/>
</cellXfs>
</styleSheet>"#;
    let bytes = support::xlsx_with_parts(
        "Sheet1",
        "",
        r#"<row r="1"><c r="A1" s="1"><v>0.5</v></c><c r="B1" s="0"><v>1</v></c></row>"#,
        None,
        Some(styles),
    );
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();

    let a1 = &sheet.cell(0, 0).unwrap().num_fmt;
    assert_eq!(a1.id, 167);
    assert_eq!(a1.format_code.as_deref(), Some("0.00%"));

    let b1 = &sheet.cell(0, 1).unwrap().num_fmt;
    assert_eq!(b1.id, 0);
    assert_eq!(b1.format_code, None);
}

#[test]
fn missing_style_index_falls_back_to_general() {
    // No `s` attribute at all, and no styles.xml present.
    let bytes = support::minimal_xlsx(r#"<row r="1"><c r="A1"><v>1</v></c></row>"#);
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();
    assert_eq!(
        sheet.cell(0, 0).unwrap().num_fmt,
        xl_io::NumFmtId::general()
    );
}

#[test]
fn missing_shared_strings_and_styles_parts_are_tolerated() {
    let bytes = support::minimal_xlsx(r#"<row r="1"><c r="A1"><v>1</v></c></row>"#);
    // Should simply parse; absent optional parts are not an error.
    let wb = xl_io::from_bytes(&bytes).unwrap();
    assert_eq!(wb.sheets.len(), 1);
}

#[test]
fn case_insensitive_sheet_lookup() {
    let bytes = support::minimal_xlsx_named("Budget2024", "", "");
    let wb = xl_io::from_bytes(&bytes).unwrap();
    assert!(wb.sheet("budget2024").is_some());
    assert!(wb.sheet("BUDGET2024").is_some());
    assert!(wb.sheet("BuDgEt2024").is_some());
    assert!(wb.sheet("nope").is_none());
}

/// OXP-061 (RUN-2026-07-11-oracle01): non-ASCII sheet-name case folding on an
/// en-US install. Excel folds Latin-1 accented pairs (`ä`/`Ä`) but uses a
/// *simple* (non-expanding, locale-independent) fold: the sharp-s does not
/// expand (`straße` ≠ `STRASSE`) and dotted capital `İ` (U+0130) does not fold
/// to ASCII `i`. Each probe is checked against a workbook holding the exact
/// stored sheet name.
#[test]
fn non_ascii_sheet_name_folding_oxp061() {
    // Sheet stored as `Ä`: both `ä` (H1) and `Ä` (H2) resolve.
    let wb = xl_io::from_bytes(&support::minimal_xlsx_named("Ä", "", "")).unwrap();
    assert!(wb.sheet("ä").is_some(), "H1: 'ä' folds to sheet 'Ä'");
    assert!(wb.sheet("Ä").is_some(), "H2: 'Ä' matches sheet 'Ä'");

    // Sheet stored as `straße`: `straße` (H4) resolves, `STRASSE` (H3) does
    // not — the sharp-s is not expanded to `SS`.
    let wb = xl_io::from_bytes(&support::minimal_xlsx_named("straße", "", "")).unwrap();
    assert!(wb.sheet("straße").is_some(), "H4: 'straße' matches");
    assert!(
        wb.sheet("STRASSE").is_none(),
        "H3: 'STRASSE' must NOT match 'straße' (#REF! in Excel)"
    );

    // Sheet stored as `İ` (U+0130): `İ` (H6) resolves, ASCII `i` (H5) does not.
    let wb = xl_io::from_bytes(&support::minimal_xlsx_named("İ", "", "")).unwrap();
    assert!(wb.sheet("İ").is_some(), "H6: 'İ' matches");
    assert!(
        wb.sheet("i").is_none(),
        "H5: ASCII 'i' must NOT fold to dotted capital 'İ' (#REF! in Excel)"
    );
}

#[test]
fn vba_project_presence_is_flagged_but_never_read() {
    let bytes = support::Fixture::new()
        .part("[Content_Types].xml", "<Types/>")
        .part("_rels/.rels", "<Relationships/>")
        .part("xl/workbook.xml", support::workbook_xml("Sheet1", ""))
        .part("xl/_rels/workbook.xml.rels", support::WORKBOOK_RELS)
        .part("xl/worksheets/sheet1.xml", support::sheet_xml(""))
        .part("xl/vbaProject.bin", vec![0u8, 1, 2, 3, b'M', b'Z'])
        .build();
    let wb = xl_io::from_bytes(&bytes).unwrap();
    assert!(wb.flags.has_vba_project);
}

#[test]
fn no_vba_project_by_default() {
    let bytes = support::minimal_xlsx("");
    let wb = xl_io::from_bytes(&bytes).unwrap();
    assert!(!wb.flags.has_vba_project);
}

#[test]
fn sheet_id_and_tab_order_are_surfaced() {
    let bytes = support::minimal_xlsx_named("Sheet1", "", "");
    let wb = xl_io::from_bytes(&bytes).unwrap();
    assert_eq!(wb.sheet_at(0).unwrap().sheet_id, 1);
    assert_eq!(wb.sheet_at(0).unwrap().name, "Sheet1");
}

/// Regression test for a real Excel 2013 (`Application="Microsoft Excel"`)
/// workbook from the Enron corpus that xl-io previously rejected outright.
/// Real Excel writes `<sheet>` entries for `state="veryHidden"` VBA
/// code/module sheets with a genuinely **empty** (or, per this test,
/// entirely absent) `r:id` — those sheets are backed by no worksheet part
/// at all. This mirrors the corpus file's actual `<sheets>` block:
///
/// ```xml
/// <sheets>
///   <sheet name="Preschedule" sheetId="1" r:id="rId1"/>
///   <sheet name="Code" sheetId="8" state="veryHidden" r:id=""/>
///   <sheet name="dlgAddRows" sheetId="13" state="hidden" r:id="rId12"/>
///   <sheet name="Module1" sheetId="14" state="veryHidden"/>
/// </sheets>
/// ```
///
/// The fix: skip `r:id`-less `<sheet>` entries instead of rejecting the
/// whole workbook. The normal sheets (including a merely `hidden`, not
/// `veryHidden`, one — `dlgAddRows` — which *does* have a valid `r:id` and
/// must still load normally) keep their data.
#[test]
fn very_hidden_vba_module_sheets_with_empty_or_missing_r_id_are_skipped() {
    let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets>
<sheet name="Preschedule" sheetId="1" r:id="rId1"/>
<sheet name="Code" sheetId="8" state="veryHidden" r:id=""/>
<sheet name="dlgAddRows" sheetId="13" state="hidden" r:id="rId12"/>
<sheet name="Module1" sheetId="14" state="veryHidden"/>
</sheets>
</workbook>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId12" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/>
</Relationships>"#;
    let bytes = support::Fixture::new()
        .part("[Content_Types].xml", "<Types/>")
        .part("_rels/.rels", "<Relationships/>")
        .part("xl/workbook.xml", workbook)
        .part("xl/_rels/workbook.xml.rels", rels)
        .part(
            "xl/worksheets/sheet1.xml",
            support::sheet_xml(r#"<row r="1"><c r="A1"><v>42</v></c></row>"#),
        )
        .part(
            "xl/worksheets/sheet2.xml",
            support::sheet_xml(r#"<row r="1"><c r="A1"><v>7</v></c></row>"#),
        )
        .build();

    let wb = xl_io::from_bytes(&bytes)
        .expect("empty/missing r:id sheets should be skipped, not reject the workbook");

    // Only the two real, worksheet-backed sheets are present as data sheets.
    assert_eq!(wb.sheets.len(), 2);
    assert_eq!(wb.sheet_at(0).unwrap().name, "Preschedule");
    assert_eq!(
        wb.sheet("Preschedule").unwrap().cell(0, 0).unwrap().value,
        Value::Number(42.0)
    );
    assert_eq!(wb.sheet_at(1).unwrap().name, "dlgAddRows");
    assert_eq!(
        wb.sheet("dlgAddRows").unwrap().cell(0, 0).unwrap().value,
        Value::Number(7.0)
    );

    // The r:id-less VBA module sheets aren't data sheets at all.
    assert!(wb.sheet("Code").is_none());
    assert!(wb.sheet("Module1").is_none());
    assert_eq!(wb.flags.skipped_sheets, 2);

    // `sheets_index` records each retained sheet's position in the FULL
    // `<sheets>` collection — the `definedName@localSheetId` index space
    // (ECMA-376 §18.2.6) — so the skipped `Code` entry at collection
    // index 1 shifts `dlgAddRows` to collection index 2 even though it loads
    // at tab position 1.
    assert_eq!(wb.sheet_at(0).unwrap().sheets_index, 0);
    assert_eq!(wb.sheet_at(1).unwrap().sheets_index, 2);
}

/// Regression test for the next Enron-corpus layer: real Excel workbooks
/// contain non-worksheet sheet types that DO have a valid `r:id` and their
/// own part, but whose root element isn't `<worksheet>` — dialogsheets
/// (`<dialogsheet>`, Excel 5 dialog sheets), chartsheets (`<chartsheet>`),
/// and Excel 4.0 macrosheets (`<macrosheet>`). xl-io identifies these by
/// their relationship `Type` and skips them rather than feeding a
/// non-`<worksheet>` root to the worksheet parser (which used to reject the
/// whole workbook). The normal worksheet keeps its data.
#[test]
fn non_worksheet_sheet_parts_are_skipped_by_relationship_type() {
    let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets>
<sheet name="Data" sheetId="1" r:id="rId1"/>
<sheet name="dlgAddRows" sheetId="2" state="hidden" r:id="rId2"/>
<sheet name="Chart1" sheetId="3" r:id="rId3"/>
<sheet name="Macro1" sheetId="4" state="veryHidden" r:id="rId4"/>
</sheets>
</workbook>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/dialogsheet" Target="dialogsheets/sheet1.xml"/>
<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chartsheet" Target="chartsheets/sheet1.xml"/>
<Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/xlMacrosheet" Target="macrosheets/sheet1.xml"/>
</Relationships>"#;
    let dialogsheet = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<dialogsheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetPr/></dialogsheet>"#;
    let chartsheet = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<chartsheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetPr/></chartsheet>"#;
    let macrosheet = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<macrosheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></macrosheet>"#;
    let bytes = support::Fixture::new()
        .part("[Content_Types].xml", "<Types/>")
        .part("_rels/.rels", "<Relationships/>")
        .part("xl/workbook.xml", workbook)
        .part("xl/_rels/workbook.xml.rels", rels)
        .part(
            "xl/worksheets/sheet1.xml",
            support::sheet_xml(r#"<row r="1"><c r="A1"><v>99</v></c></row>"#),
        )
        .part("xl/dialogsheets/sheet1.xml", dialogsheet)
        .part("xl/chartsheets/sheet1.xml", chartsheet)
        .part("xl/macrosheets/sheet1.xml", macrosheet)
        .build();

    let wb = xl_io::from_bytes(&bytes)
        .expect("non-worksheet sheet parts should be skipped, not reject the workbook");

    // Only the real worksheet is a data sheet, and it keeps its data.
    assert_eq!(wb.sheets.len(), 1);
    assert_eq!(wb.sheet_at(0).unwrap().name, "Data");
    assert_eq!(
        wb.sheet("Data").unwrap().cell(0, 0).unwrap().value,
        Value::Number(99.0)
    );

    // The dialog/chart/macro sheets aren't data sheets.
    assert!(wb.sheet("dlgAddRows").is_none());
    assert!(wb.sheet("Chart1").is_none());
    assert!(wb.sheet("Macro1").is_none());
    assert_eq!(wb.flags.skipped_sheets, 3);
}

// ---- hidden rows (`<row hidden="1">`) — OXP-121 -------------------------

#[test]
fn hidden_row_attribute_is_parsed_into_the_hidden_row_set() {
    // A1:A5 = 10,20,30,40,50 with **row 3 manually hidden** — the exact layout
    // of `RUN-2026-07-11-oracle01` / OXP-121. The `hidden="1"` on `<row r="3">`
    // must land in the sheet's 0-based `hidden_rows` set (row 3 → index 2), and
    // no other row is hidden. The row's cell (A3=30) is still parsed normally.
    let bytes = support::minimal_xlsx(
        r#"<row r="1"><c r="A1"><v>10</v></c></row>
           <row r="2"><c r="A2"><v>20</v></c></row>
           <row r="3" hidden="1"><c r="A3"><v>30</v></c></row>
           <row r="4"><c r="A4"><v>40</v></c></row>
           <row r="5"><c r="A5"><v>50</v></c></row>"#,
    );
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();
    assert!(sheet.is_row_hidden(2), "row 3 (0-based 2) is hidden");
    for r in [0u32, 1, 3, 4] {
        assert!(
            !sheet.is_row_hidden(r),
            "row {} (0-based) is visible",
            r + 1
        );
    }
    assert_eq!(sheet.hidden_rows.len(), 1);
    // The hidden row's cell is still present and correct.
    assert_eq!(sheet.cell(2, 0).unwrap().value, Value::Number(30.0));
}

#[test]
fn hidden_true_and_self_closing_hidden_rows_are_recorded() {
    // `hidden="true"` (the XSD-boolean spelling) is accepted alongside `"1"`,
    // and a self-closing hidden `<row/>` with no cells is still recorded.
    let bytes = support::minimal_xlsx(
        r#"<row r="1" hidden="true"><c r="A1"><v>1</v></c></row>
           <row r="2" hidden="1"/>
           <row r="3"><c r="A3"><v>3</v></c></row>"#,
    );
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();
    assert!(sheet.is_row_hidden(0));
    assert!(sheet.is_row_hidden(1));
    assert!(!sheet.is_row_hidden(2));
    assert_eq!(sheet.hidden_rows.len(), 2);
}

#[test]
fn no_hidden_rows_leaves_the_set_empty() {
    // Regression: a sheet with no `hidden` attributes has an empty hidden set,
    // and `hidden="0"` is explicitly *not* hidden.
    let bytes = support::minimal_xlsx(
        r#"<row r="1"><c r="A1"><v>1</v></c></row>
           <row r="2" hidden="0"><c r="A2"><v>2</v></c></row>"#,
    );
    let wb = xl_io::from_bytes(&bytes).unwrap();
    let sheet = wb.sheet("Sheet1").unwrap();
    assert!(sheet.hidden_rows.is_empty());
    assert!(!sheet.is_row_hidden(0));
    assert!(!sheet.is_row_hidden(1));
}
