//! Parser for `xl/worksheets/sheetN.xml` (ECMA-376 §18.3.1.99 `CT_Worksheet`).
//!
//! Reads `<sheetData>` (§18.3.1.80 `CT_SheetData` / §18.3.1.73 `CT_Row` /
//! §18.3.1.4 `CT_Cell`) into a `(row, col) -> Cell` map. Everything else
//! (`<sheetPr>`, `<dimension>`, `<sheetViews>`, `<cols>`, `<mergeCells>`,
//! `<conditionalFormatting>`, `<dataValidations>`, `<hyperlinks>`,
//! `<pageMargins>`, `<drawing>`, `<extLst>`, ...) is skipped without being
//! interpreted — none of it is calc-relevant.
//!
//! A cell's `<v>` cached value is interpreted per its `t` attribute
//! (§18.18.11 `ST_CellType`): `s` (shared-string index), `str` (formula
//! string result, inline text), `b` (boolean, `"1"`/`"0"`/`"true"`/`"false"`),
//! `e` (error string), `inlineStr` (text from a sibling `<is>` rather than
//! `<v>`), `d` (ISO-8601 date-time text), and `n`/absent (a plain number).
//!
//! `t="d"` is a documented gap, not a guess: converting an ISO-8601
//! date-time string to an Excel serial requires the 1900-system fictitious
//! leap-day adjustment, which `implementation-plan.md`'s semantics hit-list
//! calls out as its own dedicated task (and no approved date/time
//! dependency exists to parse ISO-8601 in the first place). `t="d"` cells
//! therefore resolve to `Value::Error(ErrorKind::Unsupported)` rather than a
//! guessed serial; this is exactly what that sentinel is for
//! (`implementation-plan.md` §0).
//!
//! **Duplicate cell references within a sheet (`OXP-060`, RESOLVED by
//! RUN-2026-07-11-oracle01):** ECMA-376 doesn't specify what a second
//! `<c r="A1">` for an already-seen reference means. The oracle probe
//! hand-built a package repeating a ref
//! (`<c r="A1"><v>1</v></c><c r="A1"><v>2</v></c>` in one row) and opened it
//! in the pinned Excel build: **Excel refuses to open the package** (treats
//! it as corrupt) rather than showing the first or the last value — the
//! refusal *is* the answer. So `xl-io` mirrors it: a repeated `(row, col)`
//! within one `<sheetData>` is a hard [`crate::IoError::Structure`] load
//! error, never a silent last-wins/first-wins pick (`implementation-plan.md`
//! §0, "never silently wrong"; human owner's decision: reject). Scope is
//! deliberately narrow — the same `r` in *different* sheets is fine; only a
//! repeat within one sheet's `<sheetData>` is rejected. See
//! `docs/oracle-experiments.md` OXP-060.

use std::collections::{BTreeMap, BTreeSet};

use quick_xml::events::{BytesStart, Event};

use xl_value::{ErrorKind, Text, Value};

use crate::caps::Caps;
use crate::cellref::parse_a1;
use crate::error::IoError;
use crate::model::{Cell, FormulaKind, NumFmtId, RawFormula};
use crate::styles::Styles;
use crate::xml_util::{
    GuardedReader, enter_root, get_attr, is_tag, parse_xsd_bool, read_leaf_text, read_rich_text,
    skip_to_matching_end, unexpected_eof,
};

/// The parsed calc-relevant contents of one worksheet part: the cell map plus
/// the set of **0-based** indices of rows carrying `<row hidden="1">`.
pub(crate) struct SheetData {
    pub cells: BTreeMap<(u32, u32), Cell>,
    pub hidden_rows: BTreeSet<u32>,
}

pub(crate) fn parse(
    part: &str,
    xml: &str,
    caps: &Caps,
    shared_strings: &[Text],
    styles: &Styles,
) -> Result<SheetData, IoError> {
    let mut reader = GuardedReader::new(part, xml, caps);
    let mut cells = BTreeMap::new();
    let mut hidden_rows = BTreeSet::new();
    if !enter_root(&mut reader, b"worksheet")? {
        return Ok(SheetData { cells, hidden_rows });
    }
    loop {
        match reader.next()? {
            Event::Start(e) if is_tag(&e, b"sheetData") => {
                parse_sheet_data(
                    &mut reader,
                    shared_strings,
                    styles,
                    &mut cells,
                    &mut hidden_rows,
                )?;
            }
            Event::Start(_) => skip_to_matching_end(&mut reader)?,
            Event::End(_) => return Ok(SheetData { cells, hidden_rows }),
            Event::Eof => return Err(unexpected_eof(part)),
            _ => {}
        }
    }
}

fn parse_sheet_data(
    reader: &mut GuardedReader<'_>,
    shared_strings: &[Text],
    styles: &Styles,
    cells: &mut BTreeMap<(u32, u32), Cell>,
    hidden_rows: &mut BTreeSet<u32>,
) -> Result<(), IoError> {
    loop {
        match reader.next()? {
            Event::End(_) => return Ok(()),
            // A self-closing `<row/>` carries no cells (formatting/visibility
            // only), but it can still be a hidden row we must record.
            Event::Empty(e) if is_tag(&e, b"row") => {
                note_hidden_row(reader, &e, hidden_rows)?;
            }
            Event::Start(e) if is_tag(&e, b"row") => {
                note_hidden_row(reader, &e, hidden_rows)?;
                parse_row(reader, shared_strings, styles, cells)?;
            }
            Event::Start(_) => skip_to_matching_end(reader)?,
            Event::Eof => return Err(unexpected_eof(reader.part())),
            _ => {}
        }
    }
}

/// If the `<row>` start-tag `e` carries `hidden="1"` / `hidden="true"`, record
/// its **0-based** index into `hidden_rows`.
///
/// The row index comes from the `<row r="N">` attribute (§18.3.1.73 `CT_Row/@r`,
/// a **1-based** row number); Excel always writes `r` on any row it gives an
/// explicit `hidden` attribute, so a hidden row without `r` cannot arise from a
/// real package. To stay bounded and never guess, a `hidden` row missing `r` is
/// simply not recorded (rather than error the whole load or invent a position).
/// A present-but-malformed or out-of-range `r` is a structural error, matching
/// the strictness of [`read_cell_ref`]. `hidden="0"`/`"false"` and an absent
/// `hidden` attribute record nothing. OOXML does not distinguish manually-hidden
/// from AutoFilter-hidden rows here — see [`crate::Sheet::hidden_rows`].
fn note_hidden_row(
    reader: &GuardedReader<'_>,
    e: &BytesStart<'_>,
    hidden_rows: &mut BTreeSet<u32>,
) -> Result<(), IoError> {
    let part = reader.part();
    let Some(hidden) = get_attr(reader, e, b"hidden")? else {
        return Ok(());
    };
    if !parse_xsd_bool(part, hidden.trim())? {
        return Ok(());
    }
    let Some(r) = get_attr(reader, e, b"r")? else {
        return Ok(());
    };
    let row1: u32 = r
        .trim()
        .parse()
        .map_err(|_| IoError::structure(part, format!("`<row r=\"{r}\">` is not an integer")))?;
    // `r` is 1-based and bounded to Excel's 1,048,576 rows; reject anything
    // outside so a bogus index can never land in the set.
    if row1 == 0 || row1 > 1_048_576 {
        return Err(IoError::structure(
            part,
            format!("`<row r=\"{r}\">` is outside the 1..=1048576 row range"),
        ));
    }
    hidden_rows.insert(row1 - 1);
    Ok(())
}

fn parse_row(
    reader: &mut GuardedReader<'_>,
    shared_strings: &[Text],
    styles: &Styles,
    cells: &mut BTreeMap<(u32, u32), Cell>,
) -> Result<(), IoError> {
    loop {
        match reader.next()? {
            Event::End(_) => return Ok(()),
            Event::Empty(e) if is_tag(&e, b"c") => {
                let (pos, cell) = parse_cell_attrs_only(reader, &e, shared_strings, styles)?;
                insert_cell(reader.part(), cells, pos, cell)?;
            }
            Event::Start(e) if is_tag(&e, b"c") => {
                let (pos, cell) = parse_cell(reader, &e, shared_strings, styles)?;
                insert_cell(reader.part(), cells, pos, cell)?;
            }
            Event::Start(_) => skip_to_matching_end(reader)?,
            Event::Eof => return Err(unexpected_eof(reader.part())),
            _ => {}
        }
    }
}

fn insert_cell(
    part: &str,
    cells: &mut BTreeMap<(u32, u32), Cell>,
    pos: (u32, u32),
    cell: Cell,
) -> Result<(), IoError> {
    // A repeated `(row, col)` within one `<sheetData>` is rejected outright —
    // Excel refuses to open such a package (OXP-060, RUN-2026-07-11-oracle01),
    // so we mirror that refusal rather than silently picking first-/last-wins.
    // See this module's docs. Reconstructing the A1 label happens only on the
    // (cold) error path, so the happy path pays nothing.
    if cells.contains_key(&pos) {
        let a1 = format!("{}{}", col_to_letters(pos.1), pos.0 + 1);
        return Err(IoError::structure(
            part,
            format!(
                "duplicate cell reference `{a1}` within one <sheetData> — Excel \
                 refuses such a package (OXP-060, RUN-2026-07-11-oracle01); \
                 rejected, never last-wins (see sheet_xml module docs)"
            ),
        ));
    }
    cells.insert(pos, cell);
    Ok(())
}

/// Formats a 0-based column index back to its A1 letters (`0` → `"A"`,
/// `25` → `"Z"`, `26` → `"AA"`, `16383` → `"XFD"`) — the inverse of the
/// column half of [`crate::cellref::parse_a1`]. Used only to echo the
/// offending reference in the duplicate-cell error message. `XFD` (three
/// letters) is the widest possible column, so the scratch buffer never spills.
fn col_to_letters(mut col: u32) -> String {
    let mut buf = [0u8; 3];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'A' + (col % 26) as u8;
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    String::from_utf8_lossy(&buf[i..]).into_owned()
}

/// A self-closing `<c r="A1" s="2"/>` has no value/formula content at all:
/// a styled-but-blank cell.
fn parse_cell_attrs_only(
    reader: &GuardedReader<'_>,
    e: &BytesStart<'_>,
    _shared_strings: &[Text],
    styles: &Styles,
) -> Result<((u32, u32), Cell), IoError> {
    let pos = read_cell_ref(reader, e)?;
    let num_fmt = resolve_style(reader, e, styles)?;
    Ok((
        pos,
        Cell {
            value: Value::Blank,
            formula: None,
            num_fmt,
        },
    ))
}

fn parse_cell(
    reader: &mut GuardedReader<'_>,
    e: &BytesStart<'_>,
    shared_strings: &[Text],
    styles: &Styles,
) -> Result<((u32, u32), Cell), IoError> {
    // `part()` hands back `&'a str` (tied to the input, not to `&self`), so
    // this is a free borrow, not a per-cell allocation.
    let part = reader.part();
    let pos = read_cell_ref(reader, e)?;
    let num_fmt = resolve_style(reader, e, styles)?;
    let cell_type = get_attr(reader, e, b"t")?;

    let mut formula: Option<RawFormula> = None;
    let mut v_text: Option<String> = None;
    let mut inline_text: Option<String> = None;

    loop {
        match reader.next()? {
            Event::End(_) => break,
            Event::Empty(fe) if is_tag(&fe, b"f") => {
                formula = Some(read_formula_attrs(reader, &fe)?);
            }
            Event::Start(fe) if is_tag(&fe, b"f") => {
                let mut rf = read_formula_attrs(reader, &fe)?;
                rf.text = Some(read_leaf_text(reader)?);
                formula = Some(rf);
            }
            Event::Empty(ve) if is_tag(&ve, b"v") => {
                v_text = Some(String::new());
            }
            Event::Start(ve) if is_tag(&ve, b"v") => {
                v_text = Some(read_leaf_text(reader)?);
            }
            Event::Empty(ie) if is_tag(&ie, b"is") => {
                inline_text = Some(String::new());
            }
            Event::Start(ie) if is_tag(&ie, b"is") => {
                inline_text = Some(read_rich_text(reader)?);
            }
            Event::Start(_) => skip_to_matching_end(reader)?,
            Event::Eof => return Err(unexpected_eof(part)),
            _ => {}
        }
    }

    let value = resolve_value(
        part,
        cell_type.as_deref(),
        v_text,
        inline_text,
        shared_strings,
    )?;

    Ok((
        pos,
        Cell {
            value,
            formula,
            num_fmt,
        },
    ))
}

fn read_cell_ref(reader: &GuardedReader<'_>, e: &BytesStart<'_>) -> Result<(u32, u32), IoError> {
    let part = reader.part();
    let r = get_attr(reader, e, b"r")?
        .ok_or_else(|| IoError::structure(part, "<c> is missing required `r` attribute"))?;
    parse_a1(part, &r)
}

fn resolve_style(
    reader: &GuardedReader<'_>,
    e: &BytesStart<'_>,
    styles: &Styles,
) -> Result<NumFmtId, IoError> {
    let part = reader.part();
    let style_index = match get_attr(reader, e, b"s")? {
        Some(s) => Some(
            s.parse::<u32>()
                .map_err(|_| IoError::structure(part, format!("`s=\"{s}\"` is not an integer")))?,
        ),
        None => None,
    };
    Ok(styles.resolve(style_index))
}

fn read_formula_attrs(
    reader: &GuardedReader<'_>,
    e: &BytesStart<'_>,
) -> Result<RawFormula, IoError> {
    let part = reader.part();
    let kind = match get_attr(reader, e, b"t")? {
        Some(s) => match s.as_str() {
            "shared" => FormulaKind::Shared,
            "array" => FormulaKind::Array,
            "dataTable" => FormulaKind::DataTable,
            "normal" => FormulaKind::Normal,
            other => {
                return Err(IoError::structure(
                    part,
                    format!("`<f t=\"{other}\">` is not a recognized formula type"),
                ));
            }
        },
        None => FormulaKind::Normal,
    };
    let shared_index = match get_attr(reader, e, b"si")? {
        Some(s) => Some(
            s.parse::<u32>()
                .map_err(|_| IoError::structure(part, format!("`si=\"{s}\"` is not an integer")))?,
        ),
        None => None,
    };
    let range = get_attr(reader, e, b"ref")?;
    Ok(RawFormula {
        text: None,
        kind,
        shared_index,
        range,
    })
}

fn resolve_value(
    part: &str,
    cell_type: Option<&str>,
    v_text: Option<String>,
    inline_text: Option<String>,
    shared_strings: &[Text],
) -> Result<Value, IoError> {
    if let Some(text) = inline_text {
        return Ok(Value::Text(Text::new(&text)));
    }
    let Some(text) = v_text else {
        return Ok(Value::Blank);
    };
    match cell_type {
        Some("s") => {
            let index: usize = text.parse().map_err(|_| {
                IoError::structure(
                    part,
                    format!("shared string index `{text}` is not an integer"),
                )
            })?;
            let s = shared_strings.get(index).ok_or_else(|| {
                IoError::structure(
                    part,
                    format!(
                        "shared string index {index} is out of range (table has {} entries)",
                        shared_strings.len()
                    ),
                )
            })?;
            Ok(Value::Text(s.clone()))
        }
        Some("str") => Ok(Value::Text(Text::new(&text))),
        Some("inlineStr") => Ok(Value::Text(Text::new(&text))),
        Some("b") => Ok(Value::Bool(parse_xsd_bool(part, text.trim())?)),
        Some("e") => Ok(Value::Error(parse_error_kind(text.trim()))),
        Some("d") => Ok(Value::Error(ErrorKind::Unsupported)),
        Some("n") | None => {
            let n: f64 = text
                .trim()
                .parse()
                .map_err(|_| IoError::structure(part, format!("`{text}` is not a valid number")))?;
            Ok(Value::number(n))
        }
        Some(other) => Err(IoError::structure(
            part,
            format!("`<c t=\"{other}\">` is not a recognized cell type"),
        )),
    }
}

/// Maps an OOXML cached error string to [`ErrorKind`]. Any text this crate
/// doesn't recognize (including Recalc's own `#UNSUPPORTED!`/`#BLOCKED!`/
/// `#RESOURCE!` sentinels, which genuine Excel never writes into a file, or
/// any other unrecognized string) maps to [`ErrorKind::Unsupported`] — a
/// deliberate no-guess fallback, never a panic.
fn parse_error_kind(s: &str) -> ErrorKind {
    match s {
        "#NULL!" => ErrorKind::Null,
        "#DIV/0!" => ErrorKind::Div0,
        "#VALUE!" => ErrorKind::Value,
        "#REF!" => ErrorKind::Ref,
        "#NAME?" => ErrorKind::Name,
        "#NUM!" => ErrorKind::Num,
        "#N/A" => ErrorKind::Na,
        "#GETTING_DATA" => ErrorKind::GettingData,
        "#SPILL!" => ErrorKind::Spill,
        "#CALC!" => ErrorKind::Calc,
        _ => ErrorKind::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::col_to_letters;
    use crate::cellref::parse_a1;

    #[test]
    fn col_to_letters_known_points() {
        assert_eq!(col_to_letters(0), "A");
        assert_eq!(col_to_letters(25), "Z");
        assert_eq!(col_to_letters(26), "AA");
        assert_eq!(col_to_letters(701), "ZZ");
        assert_eq!(col_to_letters(702), "AAA");
        assert_eq!(col_to_letters(16_383), "XFD"); // last Excel column
    }

    #[test]
    fn col_to_letters_round_trips_through_parse_a1() {
        // The label the duplicate-cell error prints must parse back to the
        // same 0-based column via the canonical parser — no off-by-one.
        for col in [0u32, 1, 25, 26, 27, 51, 700, 701, 702, 16_383] {
            let label = format!("{}1", col_to_letters(col));
            assert_eq!(parse_a1("p", &label).unwrap(), (0, col), "label {label}");
        }
    }
}
