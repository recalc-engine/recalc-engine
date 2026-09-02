//! Zip container opening and hardened part extraction, plus the top-level
//! assembly of a [`Workbook`] from its constituent OOXML parts.
//!
//! ## Provenance
//! The package structure (parts, relationships, conventional paths) is Open
//! Packaging Conventions, ECMA-376 Part 2 §9-10; `xl/workbook.xml`'s
//! relationship to its worksheets is resolved through
//! `xl/_rels/workbook.xml.rels` per §9.3, not by guessing conventional
//! paths — `xl/sharedStrings.xml`/`xl/styles.xml` are resolved the same way,
//! with a fallback to the conventional path only if the relationship isn't
//! present (some producers omit it) but the file exists anyway.
//!
//! ## Hardening
//! See [`crate::caps`] for the cap model. This module is the enforcement
//! point: [`read_part`] checks declared zip metadata *before* allocating
//! (fast rejection of an obviously-hostile central directory), then streams
//! the actual decompressed bytes under a hard cap that does **not** trust
//! that declared metadata (defending against an entry whose header
//! under-reports its true uncompressed size).

use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::path::Path;

use crate::caps::Caps;
use crate::error::{CapKind, IoError};
use crate::model::{Sheet, Workbook, WorkbookFlags};
use crate::relationships::Relationship;
use crate::styles::Styles;
use crate::{relationships, shared_strings, sheet_xml, styles, workbook_xml};

const WORKBOOK_PART: &str = "xl/workbook.xml";
const WORKBOOK_RELS_PART: &str = "xl/_rels/workbook.xml.rels";
const SHARED_STRINGS_CONVENTIONAL: &str = "xl/sharedStrings.xml";
const STYLES_CONVENTIONAL: &str = "xl/styles.xml";
const VBA_PROJECT_PART: &str = "xl/vbaProject.bin";

/// Opens a workbook from a filesystem path, using `caps` as the hardening
/// limits.
pub(crate) fn open(path: impl AsRef<Path>, caps: Caps) -> Result<Workbook, IoError> {
    let file = File::open(path).map_err(|e| IoError::Io(e.to_string()))?;
    let archive = zip::ZipArchive::new(file).map_err(|e| IoError::zip(None, e))?;
    build_workbook(archive, caps)
}

/// Opens a workbook from an in-memory byte slice, using `caps` as the
/// hardening limits.
pub(crate) fn from_bytes(bytes: &[u8], caps: Caps) -> Result<Workbook, IoError> {
    let archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| IoError::zip(None, e))?;
    build_workbook(archive, caps)
}

fn build_workbook<R: Read + Seek>(
    mut archive: zip::ZipArchive<R>,
    caps: Caps,
) -> Result<Workbook, IoError> {
    if archive.len() > caps.max_parts {
        return Err(IoError::cap(
            None,
            CapKind::PartCount,
            caps.max_parts as u64,
            archive.len() as u64,
        ));
    }

    // Presence-only check: never decompressed, per
    // `implementation-plan.md` §1/§8 (no VBA execution, ever).
    let has_vba_project = archive.index_for_name(VBA_PROJECT_PART).is_some();

    let mut running_total: u64 = 0;

    let wb_bytes =
        read_part(&mut archive, WORKBOOK_PART, &caps, &mut running_total)?.ok_or_else(|| {
            IoError::structure(WORKBOOK_PART, "required part is missing from the package")
        })?;
    let wb_xml = decode_utf8_part(WORKBOOK_PART, &wb_bytes)?;
    let wb = workbook_xml::parse(WORKBOOK_PART, &wb_xml, &caps)?;

    let rels_bytes = read_part(&mut archive, WORKBOOK_RELS_PART, &caps, &mut running_total)?
        .ok_or_else(|| {
            IoError::structure(
                WORKBOOK_RELS_PART,
                "required part is missing from the package",
            )
        })?;
    let rels_xml = decode_utf8_part(WORKBOOK_RELS_PART, &rels_bytes)?;
    let rels = relationships::parse(WORKBOOK_RELS_PART, &rels_xml, &caps)?;

    let shared_strings = match resolve_optional_part(
        &archive,
        &rels,
        "sharedStrings",
        SHARED_STRINGS_CONVENTIONAL,
    ) {
        Some(p) => match read_part(&mut archive, &p, &caps, &mut running_total)? {
            Some(bytes) => {
                let xml = decode_utf8_part(&p, &bytes)?;
                shared_strings::parse(&p, &xml, &caps)?
            }
            None => Vec::new(),
        },
        None => Vec::new(),
    };

    let styles = match resolve_optional_part(&archive, &rels, "styles", STYLES_CONVENTIONAL) {
        Some(p) => match read_part(&mut archive, &p, &caps, &mut running_total)? {
            Some(bytes) => {
                let xml = decode_utf8_part(&p, &bytes)?;
                styles::parse(&p, &xml, &caps)?
            }
            None => Styles::default(),
        },
        None => Styles::default(),
    };

    let mut sheets = Vec::with_capacity(wb.sheets.len());
    let mut skipped_sheets: u32 = 0;
    for (sheets_index, meta) in wb.sheets.iter().enumerate() {
        // Empty/absent `r:id` (`workbook_xml::SheetMeta::r_id` is `None`)
        // means this `<sheet>` has no worksheet part at all — real Excel
        // output for `state="veryHidden"` VBA code/module sheets (Enron
        // corpus finding). That's not a broken workbook; skip the sheet
        // and keep loading the rest. A *non-empty* `r:id` that doesn't
        // resolve in the rels part is still a genuinely broken reference.
        let Some(r_id) = meta.r_id.as_ref() else {
            skipped_sheets += 1;
            continue;
        };
        let rel = rels.get(r_id).ok_or_else(|| {
            IoError::structure(
                WORKBOOK_PART,
                format!(
                    "sheet `{}` references relationship id `{}`, which is not in {WORKBOOK_RELS_PART}",
                    meta.name, r_id
                ),
            )
        })?;
        // Not every `<sheet>` is a data worksheet: real Excel workbooks
        // (Enron corpus) contain dialogsheets (`<dialogsheet>`, Excel 5
        // dialog sheets), chartsheets (`<chartsheet>`), and Excel 4.0
        // macrosheets (`<macrosheet>`) — each with a valid `r:id` and its
        // own part, but a non-`<worksheet>` root the sheet parser can't
        // read. We identify them by relationship *Type* rather than sniffing
        // the root element, so a non-worksheet part is never fed to the
        // worksheet reader. Skip them (they contribute no cell data); a part
        // whose Type *is* worksheet but whose XML is malformed still errors
        // below.
        if !is_worksheet_rel_type(&rel.rel_type) {
            skipped_sheets += 1;
            continue;
        }
        let sheet_part = resolve_target(&rel.target);
        let bytes =
            read_part(&mut archive, &sheet_part, &caps, &mut running_total)?.ok_or_else(|| {
                IoError::structure(
                    &sheet_part,
                    "worksheet part referenced by xl/workbook.xml is missing from the package",
                )
            })?;
        let xml = decode_utf8_part(&sheet_part, &bytes)?;
        let sheet_data = sheet_xml::parse(&sheet_part, &xml, &caps, &shared_strings, &styles)?;
        sheets.push(Sheet {
            name: meta.name.clone(),
            sheet_id: meta.sheet_id,
            // Position in the full `<sheets>` collection (skipped entries
            // included) — the `definedName@localSheetId` index space
            // (ECMA-376 §18.2.6). `sheets_index` enumerates `wb.sheets`,
            // which holds every `<sheet>` element in document order.
            sheets_index: sheets_index as u32,
            cells: sheet_data.cells,
            hidden_rows: sheet_data.hidden_rows,
        });
    }

    Ok(Workbook {
        sheets,
        date_system: wb.date_system,
        calc_settings: wb.calc_settings,
        defined_names: wb.defined_names,
        flags: WorkbookFlags {
            has_vba_project,
            skipped_sheets,
        },
    })
}

/// Resolves an optional singleton part (shared strings, styles) by
/// relationship type first, falling back to the conventional path if the
/// relationship is absent (or points somewhere not actually in the
/// archive) but the conventional part exists anyway. Returns `None` if
/// neither resolves — the part is genuinely absent, which is valid OOXML
/// (both are optional).
fn resolve_optional_part<R: Read + Seek>(
    archive: &zip::ZipArchive<R>,
    rels: &std::collections::HashMap<String, Relationship>,
    rel_type_suffix: &str,
    conventional: &str,
) -> Option<String> {
    for rel in rels.values() {
        if rel.rel_type.ends_with(rel_type_suffix) {
            let candidate = resolve_target(&rel.target);
            if archive.index_for_name(&candidate).is_some() {
                return Some(candidate);
            }
        }
    }
    if archive.index_for_name(conventional).is_some() {
        return Some(conventional.to_string());
    }
    None
}

/// Whether a workbook relationship's `Type` URI is the worksheet type,
/// i.e. its target part is a `<worksheet>` this crate can parse for cell
/// data. Everything else referenced from `<sheets>` (dialogsheet,
/// chartsheet, macrosheet, ...) is a non-data sheet and is skipped.
///
/// The canonical Type is
/// `http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet`;
/// we match on the `/worksheet` path suffix so the transitional/strict
/// namespace variants both resolve. The leading `/` in the suffix is load-
/// bearing: it prevents a hypothetical `.../relationships/chartworksheet`
/// from matching, and the sibling non-worksheet types (`.../dialogsheet`,
/// `.../chartsheet`, `.../macrosheet`) end in `sheet`, never `worksheet`.
fn is_worksheet_rel_type(rel_type: &str) -> bool {
    rel_type.ends_with("/worksheet")
}

/// Resolves a `.rels` `Target` (always relative to `xl/workbook.xml`'s own
/// directory in this crate, since the only `.rels` part parsed is
/// `xl/_rels/workbook.xml.rels`) to a package-root-relative part path.
fn resolve_target(target: &str) -> String {
    match target.strip_prefix('/') {
        Some(absolute) => absolute.to_string(),
        None => format!("xl/{target}"),
    }
}

/// Decodes a part's bytes as UTF-8 text (stripping a leading BOM if
/// present), for handoff to `quick_xml::Reader::from_str`. OOXML parts are
/// always UTF-8 in practice (Excel never writes anything else); a part that
/// isn't valid UTF-8 is treated as malformed XML rather than guessed at.
fn decode_utf8_part(part: &str, bytes: &[u8]) -> Result<String, IoError> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    std::str::from_utf8(bytes).map(str::to_string).map_err(|e| {
        IoError::xml(
            part,
            e.valid_up_to() as u64,
            format!("part is not valid UTF-8: {e}"),
        )
    })
}

/// Reads one named part's bytes, enforcing every cap in [`Caps`] before and
/// during decompression. Returns `Ok(None)` if the part simply isn't in the
/// archive (many OOXML parts are optional).
///
/// Caps are checked **twice**, deliberately:
/// 1. Against the zip central directory's *declared* metadata
///    (`size()`/`compressed_size()`), before any buffer is allocated —
///    cheap, and rejects an obviously-hostile entry immediately.
/// 2. Against the *actual* bytes produced while streaming the decompressor,
///    in bounded chunks, aborting the instant the running total would
///    exceed [`Caps::max_single_part`] — this is the real backstop, since a
///    crafted central directory entry can under-report its own
///    uncompressed size while the real DEFLATE stream expands far past it.
fn read_part<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
    caps: &Caps,
    running_total: &mut u64,
) -> Result<Option<Vec<u8>>, IoError> {
    let mut file = match archive.by_name(name) {
        Ok(f) => f,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(IoError::zip(Some(name), e)),
    };

    let declared_size = file.size();
    let compressed_size = file.compressed_size();

    if declared_size > caps.max_single_part {
        return Err(IoError::cap(
            Some(name),
            CapKind::SinglePart,
            caps.max_single_part,
            declared_size,
        ));
    }
    if compressed_size == 0 {
        if declared_size > 0 {
            return Err(IoError::cap(
                Some(name),
                CapKind::CompressionRatio,
                caps.max_compression_ratio,
                declared_size,
            ));
        }
    } else if u128::from(declared_size)
        > u128::from(compressed_size) * u128::from(caps.max_compression_ratio)
    {
        return Err(IoError::cap(
            Some(name),
            CapKind::CompressionRatio,
            caps.max_compression_ratio,
            declared_size / compressed_size,
        ));
    }
    if running_total.saturating_add(declared_size) > caps.max_total_uncompressed {
        return Err(IoError::cap(
            Some(name),
            CapKind::TotalUncompressed,
            caps.max_total_uncompressed,
            running_total.saturating_add(declared_size),
        ));
    }

    let mut buf = Vec::with_capacity(declared_size.min(caps.max_single_part) as usize);
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut chunk)
            .map_err(|e| IoError::zip(Some(name), e))?;
        if n == 0 {
            break;
        }
        if buf.len() as u64 + n as u64 > caps.max_single_part {
            return Err(IoError::cap(
                Some(name),
                CapKind::SinglePart,
                caps.max_single_part,
                buf.len() as u64 + n as u64,
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    drop(file);

    let new_total = running_total.saturating_add(buf.len() as u64);
    if new_total > caps.max_total_uncompressed {
        return Err(IoError::cap(
            Some(name),
            CapKind::TotalUncompressed,
            caps.max_total_uncompressed,
            new_total,
        ));
    }
    *running_total = new_total;

    Ok(Some(buf))
}
