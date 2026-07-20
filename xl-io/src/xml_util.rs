//! Hardened XML event stream shared by every part parser.
//!
//! [`GuardedReader`] wraps a `quick-xml` slice reader and enforces, for
//! **every** XML part this crate parses, without each parser having to
//! remember to:
//!
//! - **No DTD.** A `<!DOCTYPE ...>` declaration is rejected the instant the
//!   reader sees it ([`crate::IoError::Doctype`]) — this crate never parses
//!   or acts on a DTD/internal subset.
//! - **No entity expansion beyond the five XML predefined entities and
//!   numeric character references.** `quick-xml` 0.41 does not resolve any
//!   entity automatically; it surfaces every reference as its own
//!   [`Event::GeneralRef`] and leaves resolution to the caller. Combined
//!   with the DOCTYPE rejection above (so no custom entity could ever be
//!   *declared*), [`resolve_general_ref`] can only ever resolve `&lt;`,
//!   `&gt;`, `&amp;`, `&apos;`, `&quot;`, and `&#NNN;`/`&#xHH;` — anything
//!   else is a hard parse error. This is the crate's XXE defense: it is
//!   structural (nothing to disable), not a runtime check that could be
//!   forgotten in one call site.
//! - **Bounded nesting.** [`crate::Caps::max_xml_depth`] caps `Start`/`End`
//!   nesting to defend against depth-bomb inputs (deeply nested elements
//!   that blow a naive recursive-descent parser's stack). This reader is
//!   iterative (an explicit depth counter, not recursion), so the cap is a
//!   deliberate ceiling rather than a crash-avoidance patch.

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::reader::Reader;

use crate::caps::Caps;
use crate::error::{CapKind, IoError};

/// An XML reader over one OOXML part's decoded text, enforcing depth and
/// DOCTYPE hardening on every event. All part parsers in this crate read
/// through this type rather than calling `quick_xml::Reader` directly.
pub(crate) struct GuardedReader<'a> {
    reader: Reader<&'a [u8]>,
    depth: u32,
    max_depth: u32,
    part: &'a str,
}

impl<'a> GuardedReader<'a> {
    /// Builds a guarded reader over `xml`, attributing errors to `part`.
    pub(crate) fn new(part: &'a str, xml: &'a str, caps: &Caps) -> GuardedReader<'a> {
        GuardedReader {
            reader: Reader::from_str(xml),
            depth: 0,
            max_depth: caps.max_xml_depth,
            part,
        }
    }

    /// The part name, for error messages built by callers.
    pub(crate) fn part(&self) -> &'a str {
        self.part
    }

    /// The reader's character decoder, needed by [`get_attr`] to decode
    /// attribute values. Always UTF-8 in practice (`Reader::from_str` is
    /// fed an already-decoded `&str`).
    pub(crate) fn decoder(&self) -> quick_xml::encoding::Decoder {
        self.reader.decoder()
    }

    /// The reader's current byte offset into the part's XML text, used to
    /// attribute non-parse XML errors (attribute decode failures,
    /// unresolvable entity references) to a position. Points just past the
    /// most recently returned event, i.e. at (or immediately after) the
    /// offending content.
    pub(crate) fn offset(&self) -> u64 {
        self.reader.buffer_position()
    }

    /// Reads the next event, applying depth tracking and the DOCTYPE/entity
    /// hardening described on [`GuardedReader`]. `Event::GeneralRef`,
    /// `Event::Text`, etc. are passed through for the caller to interpret
    /// (different parsers need different things from text/entity events).
    pub(crate) fn next(&mut self) -> Result<Event<'a>, IoError> {
        // The reader's position before the read is the byte where the next
        // event's markup starts — captured so a rejected `<!DOCTYPE` can be
        // reported at its own start (`error_position()` only tracks parse
        // *errors*, and a DOCTYPE parses fine; we reject it by policy).
        let event_start = self.reader.buffer_position();
        match self.reader.read_event() {
            Ok(Event::DocType(_)) => Err(IoError::doctype(self.part, event_start)),
            Ok(Event::Start(s)) => {
                self.depth += 1;
                if self.depth > self.max_depth {
                    return Err(IoError::cap(
                        Some(self.part),
                        CapKind::XmlDepth,
                        u64::from(self.max_depth),
                        u64::from(self.depth),
                    ));
                }
                Ok(Event::Start(s))
            }
            Ok(Event::End(e)) => {
                self.depth = self.depth.saturating_sub(1);
                Ok(Event::End(e))
            }
            Ok(other) => Ok(other),
            Err(e) => Err(IoError::xml(self.part, self.reader.error_position(), e)),
        }
    }
}

/// Resolves a `GeneralRef` event to the text it represents. Only the five
/// XML predefined entities and numeric character references can ever
/// resolve (see [`GuardedReader`]'s docs on why); anything else is a hard
/// error rather than a guess.
pub(crate) fn resolve_general_ref(
    reader: &GuardedReader<'_>,
    r: &BytesRef<'_>,
) -> Result<String, IoError> {
    let (part, offset) = (reader.part(), reader.offset());
    if let Some(ch) = r
        .resolve_char_ref()
        .map_err(|e| IoError::xml(part, offset, e))?
    {
        return Ok(ch.to_string());
    }
    let name = r.decode().map_err(|e| IoError::xml(part, offset, e))?;
    match resolve_predefined_entity(&name) {
        Some(s) => Ok(s.to_string()),
        None => Err(IoError::xml(
            part,
            offset,
            format!(
                "unrecognized entity reference `&{name};` (no DTD is parsed, so only the \
                 predefined XML entities and numeric character references resolve)"
            ),
        )),
    }
}

/// Reads and concatenates the text content of the element whose `Start`
/// event was **just consumed** by the caller, up to (and consuming) its
/// matching `End`. Used for simple text-leaf elements (`<t>`, `<f>`, `<v>`):
/// any nested `Start` is a structural error (these elements have no element
/// children in the OOXML grammar this crate targets).
pub(crate) fn read_leaf_text(reader: &mut GuardedReader<'_>) -> Result<String, IoError> {
    let mut out = String::new();
    loop {
        match reader.next()? {
            Event::Text(t) => {
                out.push_str(
                    &t.xml10_content()
                        .map_err(|e| IoError::xml(reader.part(), reader.offset(), e))?,
                );
            }
            Event::CData(c) => {
                out.push_str(
                    &c.xml10_content()
                        .map_err(|e| IoError::xml(reader.part(), reader.offset(), e))?,
                );
            }
            Event::GeneralRef(r) => out.push_str(&resolve_general_ref(reader, &r)?),
            Event::End(_) => return Ok(out),
            Event::Start(s) => {
                return Err(IoError::structure(
                    reader.part(),
                    format!(
                        "unexpected child element `<{}>` inside a text-only element",
                        String::from_utf8_lossy(s.name().as_ref())
                    ),
                ));
            }
            Event::Eof => return Err(unexpected_eof(reader.part())),
            _ => {}
        }
    }
}

/// Skips the subtree of an element whose `Start` was **just consumed**,
/// up to (and consuming) its matching `End`. Used to ignore elements this
/// crate doesn't need (`<rPr>`, `<phoneticPr>`, `<rPh>`, style sub-blocks
/// this task doesn't resolve, ...) without hand-writing a parser for each.
pub(crate) fn skip_to_matching_end(reader: &mut GuardedReader<'_>) -> Result<(), IoError> {
    let mut depth: u32 = 1;
    loop {
        match reader.next()? {
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            Event::Eof => return Err(unexpected_eof(reader.part())),
            _ => {}
        }
    }
}

pub(crate) fn unexpected_eof(part: &str) -> IoError {
    IoError::structure(part, "unexpected end of file (unclosed element)")
}

/// Advances past the XML declaration and any leading whitespace/comments/
/// processing instructions to the document's root element, verifying its
/// local name is `local_name`.
///
/// Returns `Ok(true)` if the root was a `Start` event — the caller must then
/// read its children and stop at the matching `End` — or `Ok(false)` if it
/// was self-closing (`Empty`, e.g. an empty `<sst/>`), meaning there is
/// nothing further to read for this document.
///
/// Every top-level part parser in this crate goes through this rather than
/// looping directly for its children of interest, because a bare
/// `match ... { Event::Start(e) if is_tag(&e, b"...") => ..., Event::Start(_)
/// => skip_to_matching_end(...) }` loop would otherwise treat the
/// document's own root element as an unrecognized child and skip its
/// **entire** subtree — a bug this function exists specifically to rule
/// out structurally rather than by convention at each call site.
pub(crate) fn enter_root(
    reader: &mut GuardedReader<'_>,
    local_name: &[u8],
) -> Result<bool, IoError> {
    loop {
        match reader.next()? {
            Event::Start(e) if is_tag(&e, local_name) => return Ok(true),
            Event::Empty(e) if is_tag(&e, local_name) => return Ok(false),
            Event::Start(e) => {
                return Err(IoError::structure(
                    reader.part(),
                    format!(
                        "expected root element `<{}>`, found `<{}>`",
                        String::from_utf8_lossy(local_name),
                        String::from_utf8_lossy(e.name().as_ref())
                    ),
                ));
            }
            Event::Eof => return Err(unexpected_eof(reader.part())),
            _ => {}
        }
    }
}

/// Looks up an attribute by **local name** (namespace-prefix-blind: `r:id`
/// and `id` both match local name `id`). OOXML producers use a fixed,
/// well-known set of namespace prefixes (`r:`, etc.); this crate matches on
/// local name rather than fully resolving namespaces as a deliberate v1
/// simplification (documented here rather than silently assumed), since
/// resolving prefixes to their bound namespace URIs would require
/// `NsReader` plumbing through every call site for a case that does not
/// arise in practice for well-formed OOXML.
///
/// Returns the entity-decoded, whitespace-normalized attribute value.
pub(crate) fn get_attr(
    reader: &GuardedReader<'_>,
    e: &BytesStart<'_>,
    local: &[u8],
) -> Result<Option<String>, IoError> {
    let (part, offset) = (reader.part(), reader.offset());
    for attr in e.attributes() {
        let attr = attr.map_err(|err| IoError::xml(part, offset, err))?;
        if attr.key.local_name().as_ref() == local {
            let value = attr
                .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, reader.decoder())
                .map_err(|err| IoError::xml(part, offset, err))?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

/// `true` if the start-tag's **local name** (namespace-prefix-blind, see
/// [`get_attr`]) equals `local`.
pub(crate) fn is_tag(e: &BytesStart<'_>, local: &[u8]) -> bool {
    e.name().local_name().as_ref() == local
}

/// Parses an XML-Schema boolean attribute value (`ST_Boolean`-style):
/// `"1"`/`"true"` -> `true`, `"0"`/`"false"` -> `false`, anything else is a
/// structural error rather than a guess.
pub(crate) fn parse_xsd_bool(part: &str, s: &str) -> Result<bool, IoError> {
    match s {
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        other => Err(IoError::structure(
            part,
            format!("`{other}` is not a valid XML boolean"),
        )),
    }
}

/// Reads a "rich text" content model shared by shared-string `<si>` entries
/// and inline-string `<is>` cells (ECMA-376 §18.4.4 `CT_Rst`): an optional
/// bare `<t>`, then zero or more `<r>` runs (§18.4.7 `CT_RElt`: optional
/// `<rPr>` then `<t>`), then zero or more `<rPh>` phonetic-guide runs (which
/// carry their own `<t>`, deliberately excluded) and an optional
/// `<phoneticPr>`. The element's `Start` must already be consumed; this
/// reads up to (and consumes) its matching `End`.
pub(crate) fn read_rich_text(reader: &mut GuardedReader<'_>) -> Result<String, IoError> {
    let mut out = String::new();
    loop {
        match reader.next()? {
            Event::End(_) => return Ok(out),
            Event::Start(e) if is_tag(&e, b"t") => out.push_str(&read_leaf_text(reader)?),
            Event::Start(e) if is_tag(&e, b"r") => read_run(reader, &mut out)?,
            Event::Start(_) => skip_to_matching_end(reader)?,
            Event::Eof => return Err(unexpected_eof(reader.part())),
            _ => {}
        }
    }
}

/// Reads one `<r>` run's `<t>` text (its `Start` already consumed), skipping
/// `<rPr>`.
fn read_run(reader: &mut GuardedReader<'_>, out: &mut String) -> Result<(), IoError> {
    loop {
        match reader.next()? {
            Event::End(_) => return Ok(()),
            Event::Start(e) if is_tag(&e, b"t") => out.push_str(&read_leaf_text(reader)?),
            Event::Start(_) => skip_to_matching_end(reader)?,
            Event::Eof => return Err(unexpected_eof(reader.part())),
            _ => {}
        }
    }
}
