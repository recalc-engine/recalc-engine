//! Parser for `xl/sharedStrings.xml` (ECMA-376 §18.4 `CT_Sst`).
//!
//! Each `<si>` entry (§18.4.4 `CT_Rst`) becomes one flattened, interned
//! [`Text`] via [`crate::xml_util::read_rich_text`] — see that function's
//! docs for exactly which sub-elements contribute text. `xml:space` needs no
//! special handling here: this parser never trims text-node content, so
//! leading/trailing whitespace inside `<t>...</t>` is preserved exactly
//! regardless of that attribute.

use quick_xml::events::Event;

use crate::caps::Caps;
use crate::error::IoError;
use crate::xml_util::{
    GuardedReader, enter_root, is_tag, read_rich_text, skip_to_matching_end, unexpected_eof,
};
use xl_value::Text;

/// Parses a `sharedStrings.xml` document into the shared-string table, in
/// table order (an `<si>`'s index in the returned `Vec` is the index cells
/// reference via `<c t="s"><v>N</v></c>`).
pub(crate) fn parse(part: &str, xml: &str, caps: &Caps) -> Result<Vec<Text>, IoError> {
    let mut reader = GuardedReader::new(part, xml, caps);
    let mut out = Vec::new();
    if !enter_root(&mut reader, b"sst")? {
        return Ok(out);
    }
    loop {
        match reader.next()? {
            Event::Start(e) if is_tag(&e, b"si") => {
                out.push(Text::new(&read_rich_text(&mut reader)?))
            }
            Event::Empty(e) if is_tag(&e, b"si") => out.push(Text::new("")),
            Event::Start(_) => skip_to_matching_end(&mut reader)?,
            Event::End(_) => return Ok(out),
            Event::Eof => return Err(unexpected_eof(part)),
            _ => {}
        }
    }
}
