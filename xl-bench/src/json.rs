//! Hand-written JSON output.
//!
//! `serde`/`serde_json` are not on the approved-dependency list
//! (the Recalc design rules's zero-dependency rule — only `proptest`, `zip`, and
//! `quick-xml` have been approved, and only for the crates named in that
//! log). Rather than propose a new dependency for what is, structurally, a
//! small and fixed set of report shapes, this module hand-writes JSON with
//! a single escaping primitive ([`escape_str`]) that every string passes
//! through, plus small `write_*` helpers for the handful of value shapes the
//! reports need (numbers, bools, optional numbers, [`xl_value::Value`],
//! [`crate::diff::CellStatus`]). Every object/array is built by directly
//! writing punctuation around already-escaped/formatted pieces — there is
//! no generic serializer here, deliberately: the report shapes are fixed and
//! few, so a generic JSON value tree would be more machinery than the
//! problem needs.
//!
//! Output is always valid JSON for any input `Value`/`CellStatus`/string
//! (control characters and quotes are escaped; [`f64`]s reaching here are
//! always finite, per [`xl_value::Value::Number`]'s invariant, so `Display`
//! never emits `NaN`/`inf`, which are not legal JSON number tokens).

use xl_value::Value;

use crate::diff::CellStatus;
use crate::report::{WorkbookIndexEntry, WorkbookReport};

/// Escapes `s` as a JSON string **including** the surrounding quotes, e.g.
/// `escape_str("a\"b")` → `"\"a\\\"b\""`.
#[must_use]
pub fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Renders an optional percentage as a JSON number, or `null` when `None`
/// (a workbook where the denominator was zero — see
/// [`crate::report::WorkbookSummary::fidelity_pct`]).
fn opt_pct(p: Option<f64>) -> String {
    match p {
        Some(x) => format!("{x}"),
        None => "null".to_string(),
    }
}

/// Renders a [`Value`] as a small tagged JSON object: `{"type": "...",
/// "value": ...}` (`{"type":"blank"}` / `{"type":"array"}` / `{"type":"ref"}`
/// have no `value` key — see the match arms). Arrays/refs are out of v0's
/// scalar-formula-cell scope, so they are only tagged, not expanded.
fn value_to_json(v: &Value) -> String {
    match v {
        Value::Number(n) => format!("{{\"type\":\"number\",\"value\":{n}}}"),
        Value::Text(t) => format!("{{\"type\":\"text\",\"value\":{}}}", escape_str(t.as_str())),
        Value::Bool(b) => format!("{{\"type\":\"bool\",\"value\":{b}}}"),
        Value::Error(e) => format!(
            "{{\"type\":\"error\",\"value\":{}}}",
            escape_str(e.as_str())
        ),
        Value::Blank => "{\"type\":\"blank\"}".to_string(),
        Value::Array(_) => "{\"type\":\"array\"}".to_string(),
        Value::Ref(_) => "{\"type\":\"ref\"}".to_string(),
        // BC-6 (RFC-0012): a lambda is engine-internal and never a scorable
        // Excel value; tag it distinguishably, never silently. Consistent with
        // the existing `{"type":"..."}` schema (no `value` key), so no
        // scoring_mode/schema change is required.
        Value::Lambda(_) => "{\"type\":\"lambda\"}".to_string(),
    }
}

/// Renders a [`CellStatus`] as `{"kind": "...", ...}`, with `expected`/
/// `actual`/`ulps` present only where relevant.
fn status_to_json(status: &CellStatus) -> String {
    match status {
        CellStatus::Exact => "{\"kind\":\"exact\"}".to_string(),
        CellStatus::UlpDiff { ulps } => format!("{{\"kind\":\"ulp_diff\",\"ulps\":{ulps}}}"),
        CellStatus::Mismatch { expected, actual } => format!(
            "{{\"kind\":\"mismatch\",\"expected\":{},\"actual\":{}}}",
            value_to_json(expected),
            value_to_json(actual)
        ),
        CellStatus::EngineUnsupported => "{\"kind\":\"engine_unsupported\"}".to_string(),
        CellStatus::NoOracle => "{\"kind\":\"no_oracle\"}".to_string(),
    }
}

/// Serializes a full [`WorkbookReport`] to a JSON string (pretty-printed,
/// stable key order, one cell record per line for easy diffing/`grep`).
#[must_use]
pub fn workbook_report_to_json(report: &WorkbookReport) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!(
        "  \"workbook\": {},\n",
        escape_str(&report.workbook_path)
    ));
    out.push_str("  \"engine\": {\n");
    out.push_str(&format!(
        "    \"version\": {},\n",
        escape_str(&report.engine.version)
    ));
    out.push_str(&format!(
        "    \"git_hash\": {}\n",
        escape_str(&report.engine.git_hash)
    ));
    out.push_str("  },\n");
    out.push_str("  \"workbook_flags\": {\n");
    out.push_str(&format!(
        "    \"has_vba_project\": {},\n",
        report.flags.has_vba_project
    ));
    out.push_str(&format!(
        "    \"date_system_1904\": {},\n",
        report.flags.date_system_1904
    ));
    out.push_str(&format!(
        "    \"calc_mode\": {}\n",
        escape_str(report.flags.calc_mode)
    ));
    out.push_str("  },\n");
    let s = &report.summary;
    out.push_str("  \"summary\": {\n");
    // Scoring mode, so a published number is self-identifying (TOLERANCES.md
    // "Global numeric comparison"). `verify`/`verify-dir` always run the strict
    // bit-exact floor (they do not expose the 15-sig-fig tolerance); the 15-sig
    // headline is produced by `tier0 --tol=15sig`, which stamps its own mode.
    out.push_str("    \"scoring_mode\": \"bit-exact\",\n");
    out.push_str(&format!(
        "    \"total_formula_cells\": {},\n",
        s.total_formula_cells
    ));
    out.push_str(&format!("    \"exact\": {},\n", s.exact));
    out.push_str(&format!("    \"ulp_diff\": {},\n", s.ulp_diff));
    out.push_str(&format!("    \"mismatch\": {},\n", s.mismatch));
    out.push_str(&format!(
        "    \"engine_unsupported\": {},\n",
        s.engine_unsupported
    ));
    out.push_str(&format!("    \"no_oracle\": {},\n", s.no_oracle));
    out.push_str(&format!(
        "    \"fidelity_pct\": {},\n",
        opt_pct(s.fidelity_pct())
    ));
    out.push_str(&format!(
        "    \"strict_fidelity_pct\": {}\n",
        opt_pct(s.strict_fidelity_pct())
    ));
    out.push_str("  },\n");
    out.push_str("  \"cells\": [\n");
    for (i, cell) in report.cells.iter().enumerate() {
        let comma = if i + 1 < report.cells.len() { "," } else { "" };
        out.push_str(&format!(
            "    {{\"sheet\":{},\"ref\":{},\"row\":{},\"col\":{},\"formula\":{},\"status\":{}}}{comma}\n",
            escape_str(&cell.sheet),
            escape_str(&cell.cell_ref),
            cell.row,
            cell.col,
            escape_str(&cell.formula),
            status_to_json(&cell.status),
        ));
    }
    out.push_str("  ]\n");
    out.push_str("}\n");
    out
}

/// A minimal aggregate record for `recalc verify-dir`'s corpus-level JSON
/// index — one row per attempted file.
///
/// Carries only a [`WorkbookIndexEntry`] (path + summary), **not** the full
/// [`WorkbookReport`]: retaining every per-cell record for the whole corpus
/// run is unbounded memory for data the index never reads — see
/// [`WorkbookIndexEntry`]'s docs.
#[derive(Clone, Debug)]
pub enum CorpusEntryResult {
    /// The workbook loaded and was diffed; carries its summary-only entry.
    Ok(WorkbookIndexEntry),
    /// The workbook failed to load/parse (`recalc verify-dir` records this
    /// rather than aborting the whole corpus run — see
    /// `crate::corpus::verify_dir`'s docs).
    LoadFailure { path: String, message: String },
}

/// Serializes a corpus run's aggregate index to JSON.
#[must_use]
pub fn corpus_index_to_json(entries: &[CorpusEntryResult]) -> String {
    let mut out = String::new();
    out.push_str("{\n  \"files\": [\n");
    for (i, entry) in entries.iter().enumerate() {
        let comma = if i + 1 < entries.len() { "," } else { "" };
        match entry {
            CorpusEntryResult::Ok(entry) => {
                let s = &entry.summary;
                out.push_str(&format!(
                    "    {{\"file\":{},\"status\":\"ok\",\"total_formula_cells\":{},\"mismatch\":{},\"fidelity_pct\":{},\"strict_fidelity_pct\":{}}}{comma}\n",
                    escape_str(&entry.workbook_path),
                    s.total_formula_cells,
                    s.mismatch,
                    opt_pct(s.fidelity_pct()),
                    opt_pct(s.strict_fidelity_pct()),
                ));
            }
            CorpusEntryResult::LoadFailure { path, message } => {
                out.push_str(&format!(
                    "    {{\"file\":{},\"status\":\"load_failure\",\"message\":{}}}{comma}\n",
                    escape_str(path),
                    escape_str(message),
                ));
            }
        }
    }
    out.push_str("  ]\n}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_quotes_backslashes_and_control_chars() {
        assert_eq!(escape_str("a\"b"), "\"a\\\"b\"");
        assert_eq!(escape_str("a\\b"), "\"a\\\\b\"");
        assert_eq!(escape_str("a\nb"), "\"a\\nb\"");
        assert_eq!(escape_str("a\tb"), "\"a\\tb\"");
        assert_eq!(escape_str("a\u{1}b"), "\"a\\u0001b\"");
    }

    #[test]
    fn value_to_json_tags_every_variant() {
        assert!(value_to_json(&Value::number(1.5)).contains("\"type\":\"number\""));
        assert!(value_to_json(&Value::text("x")).contains("\"type\":\"text\""));
        assert!(value_to_json(&Value::bool(true)).contains("\"type\":\"bool\""));
        assert!(value_to_json(&Value::Blank).contains("\"type\":\"blank\""));
    }

    #[test]
    fn status_to_json_mismatch_has_expected_and_actual() {
        let status = CellStatus::Mismatch {
            expected: Value::number(1.0),
            actual: Value::number(2.0),
        };
        let json = status_to_json(&status);
        assert!(json.contains("\"expected\""));
        assert!(json.contains("\"actual\""));
    }
}
