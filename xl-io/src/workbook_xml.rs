//! Parser for `xl/workbook.xml` (ECMA-376 §18.2.27 `CT_Workbook`).
//!
//! Reads exactly the four sub-trees this crate's contract needs:
//! `<workbookPr>` (§18.2.28, the `date1904` flag), `<sheets>` (§18.2.20
//! `CT_Sheets` / §18.2.19 `CT_Sheet` — name, internal `sheetId`, and the
//! `r:id` used to resolve the worksheet part via the workbook's
//! relationships part), `<definedNames>` (§18.2.6 `CT_DefinedNames` /
//! §18.2.5 `CT_DefinedName`, stored raw), and `<calcPr>` (§18.2.15
//! `CT_CalcPr`). Everything else (`<fileVersion>`, `<workbookProtection>`,
//! `<bookViews>`, `<externalReferences>`, `<pivotCaches>`, `<extLst>`, ...)
//! is skipped without being interpreted.
//!
//! ## `r:id`-less sheets (veryHidden VBA module sheets)
//! Real Excel (observed in the Enron corpus) writes `<sheet>` entries for
//! `state="veryHidden"` VBA code/module sheets (e.g. named `"Code"`) with a
//! genuinely **empty** `r:id=""` — those sheets have no worksheet part in
//! the package at all. `r_id` is therefore `Option<String>` here: `None`
//! covers both an empty and an absent `r:id` attribute. The caller
//! (`package.rs`) skips such sheets rather than rejecting the whole
//! workbook; a *non-empty* `r:id` that doesn't resolve in the relationships
//! part is still a hard `Structure` error there.

use quick_xml::events::{BytesStart, Event};

use crate::caps::Caps;
use crate::error::IoError;
use crate::model::{CalcMode, CalcSettings, DateSystem, DefinedName};
use crate::xml_util::{
    GuardedReader, enter_root, get_attr, is_tag, parse_xsd_bool, read_leaf_text,
    skip_to_matching_end, unexpected_eof,
};

/// One `<sheet>` entry from `<sheets>`, before its worksheet part has been
/// located (that requires the workbook relationships part, resolved by the
/// caller in `package.rs`).
#[derive(Clone, Debug)]
pub(crate) struct SheetMeta {
    pub(crate) name: String,
    pub(crate) sheet_id: u32,
    /// `None` if `r:id` was absent, or present-but-empty (`r:id=""`) — both
    /// mean "no worksheet part backs this sheet"; see the module doc.
    pub(crate) r_id: Option<String>,
}

/// Everything this crate extracts from `xl/workbook.xml`.
#[derive(Clone, Debug)]
pub(crate) struct WorkbookXml {
    pub(crate) sheets: Vec<SheetMeta>,
    pub(crate) date_system: DateSystem,
    pub(crate) calc_settings: CalcSettings,
    pub(crate) defined_names: Vec<DefinedName>,
}

pub(crate) fn parse(part: &str, xml: &str, caps: &Caps) -> Result<WorkbookXml, IoError> {
    let mut reader = GuardedReader::new(part, xml, caps);
    let mut sheets = Vec::new();
    let mut date_system = DateSystem::Excel1900;
    let mut calc_settings = CalcSettings::default();
    let mut defined_names = Vec::new();

    if !enter_root(&mut reader, b"workbook")? {
        // Self-closing `<workbook/>`: no sheets, no settings — legal XML,
        // if a fairly useless workbook.
        return Ok(WorkbookXml {
            sheets,
            date_system,
            calc_settings,
            defined_names,
        });
    }

    loop {
        match reader.next()? {
            Event::Empty(e) if is_tag(&e, b"workbookPr") => {
                date_system = read_date_system(&reader, &e)?;
            }
            Event::Start(e) if is_tag(&e, b"workbookPr") => {
                date_system = read_date_system(&reader, &e)?;
                skip_to_matching_end(&mut reader)?;
            }
            Event::Start(e) if is_tag(&e, b"sheets") => {
                sheets = parse_sheets(&mut reader)?;
            }
            Event::Start(e) if is_tag(&e, b"definedNames") => {
                defined_names = parse_defined_names(&mut reader)?;
            }
            Event::Empty(e) if is_tag(&e, b"calcPr") => {
                calc_settings = read_calc_pr(&reader, &e)?;
            }
            Event::Start(e) if is_tag(&e, b"calcPr") => {
                calc_settings = read_calc_pr(&reader, &e)?;
                skip_to_matching_end(&mut reader)?;
            }
            Event::Start(_) => skip_to_matching_end(&mut reader)?,
            Event::End(_) => {
                return Ok(WorkbookXml {
                    sheets,
                    date_system,
                    calc_settings,
                    defined_names,
                });
            }
            Event::Eof => return Err(unexpected_eof(part)),
            _ => {}
        }
    }
}

fn read_date_system(reader: &GuardedReader<'_>, e: &BytesStart<'_>) -> Result<DateSystem, IoError> {
    let part = reader.part();
    let is_1904 = match get_attr(reader, e, b"date1904")? {
        Some(v) => parse_xsd_bool(part, &v)?,
        None => false,
    };
    Ok(if is_1904 {
        DateSystem::Excel1904
    } else {
        DateSystem::Excel1900
    })
}

fn read_calc_pr(reader: &GuardedReader<'_>, e: &BytesStart<'_>) -> Result<CalcSettings, IoError> {
    let part = reader.part();
    let calc_mode = match get_attr(reader, e, b"calcMode")? {
        Some(v) => parse_calc_mode(part, &v)?,
        None => CalcMode::Auto,
    };
    let iterate = match get_attr(reader, e, b"iterate")? {
        Some(v) => parse_xsd_bool(part, &v)?,
        None => false,
    };
    let iterate_count = match get_attr(reader, e, b"iterateCount")? {
        Some(v) => v.parse().map_err(|_| {
            IoError::structure(part, format!("`iterateCount=\"{v}\"` is not an integer"))
        })?,
        None => 100,
    };
    let iterate_delta = match get_attr(reader, e, b"iterateDelta")? {
        Some(v) => v.parse().map_err(|_| {
            IoError::structure(part, format!("`iterateDelta=\"{v}\"` is not a number"))
        })?,
        None => 0.001,
    };
    let full_calc_on_load = match get_attr(reader, e, b"fullCalcOnLoad")? {
        Some(v) => parse_xsd_bool(part, &v)?,
        None => false,
    };
    Ok(CalcSettings {
        calc_mode,
        iterate,
        iterate_count,
        iterate_delta,
        full_calc_on_load,
    })
}

fn parse_sheets(reader: &mut GuardedReader<'_>) -> Result<Vec<SheetMeta>, IoError> {
    let mut out = Vec::new();
    loop {
        match reader.next()? {
            Event::End(_) => return Ok(out),
            Event::Empty(e) if is_tag(&e, b"sheet") => out.push(read_sheet_attrs(reader, &e)?),
            Event::Start(e) if is_tag(&e, b"sheet") => {
                out.push(read_sheet_attrs(reader, &e)?);
                skip_to_matching_end(reader)?;
            }
            Event::Start(_) => skip_to_matching_end(reader)?,
            Event::Eof => return Err(unexpected_eof(reader.part())),
            _ => {}
        }
    }
}

fn read_sheet_attrs(reader: &GuardedReader<'_>, e: &BytesStart<'_>) -> Result<SheetMeta, IoError> {
    let part = reader.part();
    let name = get_attr(reader, e, b"name")?
        .ok_or_else(|| IoError::structure(part, "<sheet> is missing required `name` attribute"))?;
    let sheet_id_str = get_attr(reader, e, b"sheetId")?.ok_or_else(|| {
        IoError::structure(part, "<sheet> is missing required `sheetId` attribute")
    })?;
    let sheet_id: u32 = sheet_id_str.parse().map_err(|_| {
        IoError::structure(
            part,
            format!("`sheetId=\"{sheet_id_str}\"` is not an integer"),
        )
    })?;
    // Absent and empty (`r:id=""`) are both treated as "no worksheet part":
    // real Excel writes the latter for veryHidden VBA code/module sheets
    // (Enron corpus finding) — see the module doc. `package.rs` skips these
    // rather than erroring.
    let r_id = get_attr(reader, e, b"id")?.filter(|v| !v.is_empty());
    Ok(SheetMeta {
        name,
        sheet_id,
        r_id,
    })
}

fn parse_defined_names(reader: &mut GuardedReader<'_>) -> Result<Vec<DefinedName>, IoError> {
    let mut out = Vec::new();
    loop {
        match reader.next()? {
            Event::End(_) => return Ok(out),
            Event::Empty(e) if is_tag(&e, b"definedName") => {
                let (name, sheet_scope) = read_defined_name_attrs(reader, &e)?;
                out.push(DefinedName {
                    name,
                    formula: String::new(),
                    sheet_scope,
                });
            }
            Event::Start(e) if is_tag(&e, b"definedName") => {
                let (name, sheet_scope) = read_defined_name_attrs(reader, &e)?;
                let formula = read_leaf_text(reader)?;
                out.push(DefinedName {
                    name,
                    formula,
                    sheet_scope,
                });
            }
            Event::Start(_) => skip_to_matching_end(reader)?,
            Event::Eof => return Err(unexpected_eof(reader.part())),
            _ => {}
        }
    }
}

fn read_defined_name_attrs(
    reader: &GuardedReader<'_>,
    e: &BytesStart<'_>,
) -> Result<(String, Option<u32>), IoError> {
    let part = reader.part();
    let name = get_attr(reader, e, b"name")?.ok_or_else(|| {
        IoError::structure(part, "<definedName> is missing required `name` attribute")
    })?;
    let sheet_scope = match get_attr(reader, e, b"localSheetId")? {
        Some(v) => Some(v.parse().map_err(|_| {
            IoError::structure(part, format!("`localSheetId=\"{v}\"` is not an integer"))
        })?),
        None => None,
    };
    Ok((name, sheet_scope))
}

fn parse_calc_mode(part: &str, s: &str) -> Result<CalcMode, IoError> {
    match s {
        "auto" => Ok(CalcMode::Auto),
        "autoNoTable" => Ok(CalcMode::AutoNoTable),
        "manual" => Ok(CalcMode::Manual),
        other => Err(IoError::structure(
            part,
            format!("`calcMode=\"{other}\"` is not recognized"),
        )),
    }
}
