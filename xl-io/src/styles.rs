//! Parser for `xl/styles.xml` (ECMA-376 §18.8 `CT_Stylesheet`) — just enough
//! to resolve a cell's `s` (style index) attribute to a [`NumFmtId`].
//!
//! Only `<numFmts>` (§18.8.31 `CT_NumFmts`, custom format definitions) and
//! `<cellXfs>` (§18.8.10 `CT_CellXfs`, the array a cell's `s` attribute
//! indexes into) are read. Fonts, fills, borders, `cellStyleXfs`,
//! `cellStyles`, differential formats, table styles, and colors are parsed
//! only enough to skip their subtrees — rendering (and everything else
//! style-related) is explicitly out of scope for this crate.

use std::collections::BTreeMap;

use quick_xml::events::Event;

use crate::caps::Caps;
use crate::error::IoError;
use crate::model::NumFmtId;
use crate::xml_util::{
    GuardedReader, enter_root, get_attr, is_tag, skip_to_matching_end, unexpected_eof,
};

/// The subset of `xl/styles.xml` needed to resolve number formats.
#[derive(Clone, Debug, Default)]
pub(crate) struct Styles {
    /// `numFmtId` -> custom `formatCode`, from `<numFmts>`.
    num_fmts: BTreeMap<u32, String>,
    /// `cellXfs[i]` -> `numFmtId`, in declaration order; a cell's `s`
    /// attribute is an index into this.
    cell_xfs: Vec<u32>,
}

impl Styles {
    /// Resolves a cell's `s` (style index) attribute to a [`NumFmtId`].
    /// `None` (no `s` attribute) and any out-of-range index both resolve to
    /// [`NumFmtId::general`] — an untrusted/malformed style index must
    /// never panic or fail the whole parse.
    pub(crate) fn resolve(&self, style_index: Option<u32>) -> NumFmtId {
        let Some(idx) = style_index else {
            return NumFmtId::general();
        };
        let Some(&num_fmt_id) = self.cell_xfs.get(idx as usize) else {
            return NumFmtId::general();
        };
        NumFmtId {
            id: num_fmt_id,
            format_code: self.num_fmts.get(&num_fmt_id).cloned(),
        }
    }
}

/// Parses a `styles.xml` document into [`Styles`].
pub(crate) fn parse(part: &str, xml: &str, caps: &Caps) -> Result<Styles, IoError> {
    let mut reader = GuardedReader::new(part, xml, caps);
    let mut styles = Styles::default();
    if !enter_root(&mut reader, b"styleSheet")? {
        return Ok(styles);
    }
    loop {
        match reader.next()? {
            Event::Start(e) if is_tag(&e, b"numFmts") => {
                styles.num_fmts = parse_num_fmts(&mut reader)?;
            }
            Event::Start(e) if is_tag(&e, b"cellXfs") => {
                styles.cell_xfs = parse_cell_xfs(&mut reader)?;
            }
            Event::Start(_) => skip_to_matching_end(&mut reader)?,
            Event::End(_) => return Ok(styles),
            Event::Eof => return Err(unexpected_eof(part)),
            _ => {}
        }
    }
}

fn parse_num_fmts(reader: &mut GuardedReader<'_>) -> Result<BTreeMap<u32, String>, IoError> {
    let mut out = BTreeMap::new();
    loop {
        match reader.next()? {
            Event::End(_) => return Ok(out),
            Event::Empty(e) if is_tag(&e, b"numFmt") => {
                if let Some((id, code)) = read_num_fmt_attrs(reader, &e)? {
                    out.insert(id, code);
                }
            }
            Event::Start(e) if is_tag(&e, b"numFmt") => {
                if let Some((id, code)) = read_num_fmt_attrs(reader, &e)? {
                    out.insert(id, code);
                }
                skip_to_matching_end(reader)?;
            }
            Event::Start(_) => skip_to_matching_end(reader)?,
            Event::Eof => return Err(unexpected_eof(reader.part())),
            _ => {}
        }
    }
}

fn read_num_fmt_attrs(
    reader: &GuardedReader<'_>,
    e: &quick_xml::events::BytesStart<'_>,
) -> Result<Option<(u32, String)>, IoError> {
    let part = reader.part();
    let id = get_attr(reader, e, b"numFmtId")?;
    let code = get_attr(reader, e, b"formatCode")?;
    match (id, code) {
        (Some(id), Some(code)) => {
            let parsed: u32 = id.parse().map_err(|_| {
                IoError::structure(part, format!("`numFmtId=\"{id}\"` is not a valid integer"))
            })?;
            Ok(Some((parsed, code)))
        }
        _ => Ok(None),
    }
}

fn parse_cell_xfs(reader: &mut GuardedReader<'_>) -> Result<Vec<u32>, IoError> {
    let mut out = Vec::new();
    loop {
        match reader.next()? {
            Event::End(_) => return Ok(out),
            Event::Empty(e) if is_tag(&e, b"xf") => {
                out.push(read_xf_num_fmt_id(reader, &e)?);
            }
            Event::Start(e) if is_tag(&e, b"xf") => {
                out.push(read_xf_num_fmt_id(reader, &e)?);
                skip_to_matching_end(reader)?;
            }
            Event::Start(_) => skip_to_matching_end(reader)?,
            Event::Eof => return Err(unexpected_eof(reader.part())),
            _ => {}
        }
    }
}

fn read_xf_num_fmt_id(
    reader: &GuardedReader<'_>,
    e: &quick_xml::events::BytesStart<'_>,
) -> Result<u32, IoError> {
    let part = reader.part();
    match get_attr(reader, e, b"numFmtId")? {
        Some(s) => s.parse().map_err(|_| {
            IoError::structure(part, format!("`numFmtId=\"{s}\"` is not a valid integer"))
        }),
        None => Ok(0),
    }
}
