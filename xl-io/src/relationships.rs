//! Parser for OOXML relationship parts (`.rels`), Open Packaging
//! Conventions (ECMA-376 Part 2) §9.3 `Relationships`.
//!
//! Used to resolve `xl/_rels/workbook.xml.rels`: maps a relationship `Id`
//! (as referenced by `<sheet r:id="...">` in `xl/workbook.xml`) to its
//! `Target` part path and `Type` URI.

use std::collections::HashMap;

use quick_xml::events::{BytesStart, Event};

use crate::caps::Caps;
use crate::error::IoError;
use crate::xml_util::{
    GuardedReader, enter_root, get_attr, is_tag, skip_to_matching_end, unexpected_eof,
};

#[derive(Clone, Debug)]
pub(crate) struct Relationship {
    pub(crate) target: String,
    pub(crate) rel_type: String,
}

/// Parses a `.rels` document into `Id -> Relationship`.
pub(crate) fn parse(
    part: &str,
    xml: &str,
    caps: &Caps,
) -> Result<HashMap<String, Relationship>, IoError> {
    let mut reader = GuardedReader::new(part, xml, caps);
    let mut out = HashMap::new();
    if !enter_root(&mut reader, b"Relationships")? {
        return Ok(out);
    }
    loop {
        match reader.next()? {
            Event::Empty(e) if is_tag(&e, b"Relationship") => {
                insert_rel(&reader, &e, &mut out)?;
            }
            Event::Start(e) if is_tag(&e, b"Relationship") => {
                insert_rel(&reader, &e, &mut out)?;
                skip_to_matching_end(&mut reader)?;
            }
            Event::Start(_) => skip_to_matching_end(&mut reader)?,
            Event::End(_) => return Ok(out),
            Event::Eof => return Err(unexpected_eof(part)),
            _ => {}
        }
    }
}

fn insert_rel(
    reader: &GuardedReader<'_>,
    e: &BytesStart<'_>,
    out: &mut HashMap<String, Relationship>,
) -> Result<(), IoError> {
    let part = reader.part();
    let id = get_attr(reader, e, b"Id")?.ok_or_else(|| {
        IoError::structure(part, "<Relationship> is missing required `Id` attribute")
    })?;
    let target = get_attr(reader, e, b"Target")?.ok_or_else(|| {
        IoError::structure(
            part,
            "<Relationship> is missing required `Target` attribute",
        )
    })?;
    let rel_type = get_attr(reader, e, b"Type")?.unwrap_or_default();
    out.insert(id, Relationship { target, rel_type });
    Ok(())
}
