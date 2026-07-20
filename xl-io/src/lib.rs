//! `xl-io` — OOXML (`.xlsx`/`.xlsm`) read path for Recalc.
//!
//! Opens a package (zip archive), reads the parts a calc engine needs —
//! `xl/workbook.xml` (sheet list, 1900/1904 date system, calc settings,
//! defined names), `xl/_rels/workbook.xml.rels` (relationship resolution),
//! `xl/sharedStrings.xml`, `xl/styles.xml` (just enough to resolve number
//! formats), and each `xl/worksheets/sheetN.xml` — and builds a per-sheet
//! cell value store ([`Workbook`]/[`Sheet`]/[`Cell`]) on top of
//! `xl-value`'s [`xl_value::Value`].
//!
//! # Provenance
//! Element/attribute semantics are reconstructed clean-room from ECMA-376
//! 5th edition ("Office Open XML File Formats"), Part 1 §18 "SpreadsheetML"
//! (workbook/worksheet/shared-strings/styles) and Part 2 §9-10 (Open
//! Packaging Conventions — parts, relationships). See each module's header
//! for the specific subclauses it implements. No GPL spreadsheet source
//! (LibreOffice/Gnumeric) was consulted (`implementation-plan.md` §9,
//! clean-room only).
//!
//! # What this crate does *not* do
//! - **No formula parsing.** `<f>` text is stored raw
//!   ([`RawFormula::text`]); `xl-ast` owns the grammar.
//! - **No number-format rendering.** [`NumFmtId`] resolves the `s` → `cellXfs`
//!   → `numFmtId` chain and captures custom format codes, but does not
//!   implement Excel's format mini-language.
//! - **No date-serial conversion.** `t="d"` cells (a rare, spec-legal cell
//!   type carrying an ISO-8601 date-time directly) resolve to
//!   `Value::Error(ErrorKind::Unsupported)` rather than a guessed serial —
//!   see `sheet_xml`'s module docs for why.
//! - **No VBA execution, ever.** `xl/vbaProject.bin`'s presence is recorded
//!   in [`WorkbookFlags::has_vba_project`] by checking the zip's part list;
//!   its contents are never decompressed, let alone parsed or run.
//! - **No network access.**
//!
//! # Hardening (untrusted input)
//! Every package is treated as adversarial input, never as a trusted file:
//!
//! - **Resource caps** ([`Caps`]) bound total/per-part uncompressed size,
//!   part count, and compression ratio, checked against declared zip
//!   metadata *and* against actual streamed bytes (a hostile central
//!   directory entry can under-report its own size) — see `package`'s
//!   module docs.
//! - **No DTD, no XXE.** A `<!DOCTYPE>` is rejected the instant it's seen;
//!   combined with `quick-xml`'s architecture (every entity/character
//!   reference is surfaced as an event the caller must explicitly resolve,
//!   and only the five XML-predefined entities and numeric character
//!   references are ever resolved here), no external or custom entity can
//!   ever expand — see `xml_util`'s module docs for the exact mechanism.
//! - **Bounded XML nesting** ([`Caps::max_xml_depth`]) defends against
//!   depth-bomb inputs; the reader is iterative, not recursive.
//! - **Never panics on untrusted data.** Malformed zip/XML/OOXML structure
//!   always becomes an [`IoError`], never a panic — this crate has no
//!   `unwrap`/`expect`/indexing panic on attacker-controlled data, and
//!   `#![forbid(unsafe_code)]` rules out a whole class of memory-safety
//!   bugs outright.
//!
//! # Storage note
//! [`Sheet::cells`] is a `BTreeMap<(u32, u32), Cell>` — simple, correct,
//! and adequate for v1. `implementation-plan.md` §2 specifies a chunked
//! column-major sparse store as the eventual performance design; that is a
//! **later** task, not this crate's v1 contract.

#![forbid(unsafe_code)]

mod caps;
mod cellref;
mod error;
mod model;
mod package;
mod relationships;
mod shared_strings;
mod sheet_xml;
mod styles;
mod workbook_xml;
mod xml_util;

pub use caps::Caps;
pub use error::{CapKind, IoError};
pub use model::{
    CalcMode, CalcSettings, Cell, DateSystem, DefinedName, FormulaKind, NumFmtId, RawFormula,
    Sheet, Workbook, WorkbookFlags,
};

use std::path::Path;

/// Opens a workbook from a filesystem path, with default hardening caps
/// ([`Caps::default`]).
pub fn open(path: impl AsRef<Path>) -> Result<Workbook, IoError> {
    package::open(path, Caps::default())
}

/// Opens a workbook from a filesystem path, with caller-supplied hardening
/// caps.
pub fn open_with_caps(path: impl AsRef<Path>, caps: Caps) -> Result<Workbook, IoError> {
    package::open(path, caps)
}

/// Opens a workbook from an in-memory zip byte slice, with default
/// hardening caps ([`Caps::default`]).
pub fn from_bytes(bytes: &[u8]) -> Result<Workbook, IoError> {
    package::from_bytes(bytes, Caps::default())
}

/// Opens a workbook from an in-memory zip byte slice, with caller-supplied
/// hardening caps.
pub fn from_bytes_with_caps(bytes: &[u8], caps: Caps) -> Result<Workbook, IoError> {
    package::from_bytes(bytes, caps)
}
