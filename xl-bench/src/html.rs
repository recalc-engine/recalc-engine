//! Self-contained HTML fidelity reports.
//!
//! Every page produced here is a single `.html` file with inline `<style>`
//! and **no external assets, no JavaScript** — static tables are sufficient
//! for the M0 gate artifact ("HTML report on 100 real workbooks",
//! `implementation-plan.md` §12 bootstrap queue). This keeps the report
//! trivially viewable offline and trivially diff-friendly in a PR.

use std::collections::BTreeMap;

use xl_value::Value;

use crate::diff::CellStatus;
use crate::json::CorpusEntryResult;
use crate::report::{CellRecord, WorkbookReport, WorkbookSummary};

/// Cap on the number of mismatch rows rendered in the per-workbook report
/// (`implementation-plan.md`/Task 10: "cap 500 rows, note truncation").
pub const MAX_MISMATCH_ROWS: usize = 500;

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn fmt_pct(p: Option<f64>) -> String {
    match p {
        Some(x) => format!("{x:.2}%"),
        None => "n/a".to_string(),
    }
}

fn value_to_html(v: &Value) -> String {
    match v {
        Value::Number(n) => esc(&n.to_string()),
        Value::Text(t) => format!("&quot;{}&quot;", esc(t.as_str())),
        Value::Bool(b) => esc(&b.to_string()),
        Value::Error(e) => esc(e.as_str()),
        Value::Blank => "<em>(blank)</em>".to_string(),
        Value::Array(_) => "<em>(array)</em>".to_string(),
        Value::Ref(_) => "<em>(ref)</em>".to_string(),
        // BC-6 (RFC-0012): a lambda is engine-internal and never a scorable
        // Excel value; render it distinguishably (unsupported), never silently.
        Value::Lambda(_) => "<em>(lambda #UNSUPPORTED!)</em>".to_string(),
    }
}

const STYLE: &str = r#"
body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif;
       margin: 2rem auto; max-width: 1100px; color: #1a1a1a; background: #fff; }
h1 { font-size: 1.4rem; }
h2 { font-size: 1.1rem; margin-top: 2rem; border-bottom: 1px solid #ddd; padding-bottom: 0.25rem; }
table { border-collapse: collapse; width: 100%; margin: 0.75rem 0 1.5rem; font-size: 0.85rem; }
th, td { border: 1px solid #ddd; padding: 0.35rem 0.6rem; text-align: left; vertical-align: top; }
th { background: #f5f5f5; }
tr:nth-child(even) td { background: #fafafa; }
.meta { color: #555; font-size: 0.9rem; margin-bottom: 1rem; }
.meta dt { font-weight: 600; display: inline; }
.meta dd { display: inline; margin: 0 1.2rem 0 0.3rem; }
.pct { font-size: 1.6rem; font-weight: 700; }
.pct-row { display: flex; gap: 2.5rem; margin: 1rem 0; }
.pct-box { border: 1px solid #ddd; border-radius: 6px; padding: 0.6rem 1rem; }
.pct-label { font-size: 0.8rem; color: #555; }
code, .mono { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
.status-exact { color: #1a7f37; }
.status-ulp_diff { color: #9a6700; }
.status-mismatch { color: #cf222e; font-weight: 600; }
.status-engine_unsupported { color: #6e7781; }
.status-no_oracle { color: #6e7781; font-style: italic; }
.truncation-note { color: #9a6700; margin: 0.5rem 0; }
"#;

/// Renders one workbook's [`WorkbookReport`] as a complete, self-contained
/// HTML page.
#[must_use]
pub fn workbook_report_to_html(report: &WorkbookReport) -> String {
    let mut out = String::new();
    out.push_str("<!doctype html>\n<html><head><meta charset=\"utf-8\">");
    out.push_str(&format!(
        "<title>Recalc fidelity report — {}</title>",
        esc(&report.workbook_path)
    ));
    out.push_str(&format!("<style>{STYLE}</style></head><body>\n"));

    out.push_str(&format!(
        "<h1>Recalc fidelity report: <span class=\"mono\">{}</span></h1>\n",
        esc(&report.workbook_path)
    ));

    out.push_str("<div class=\"pct-row\">\n");
    out.push_str(&format!(
        "<div class=\"pct-box\"><div class=\"pct\">{}</div><div class=\"pct-label\">Fidelity % (Exact+UlpDiff / judged, excludes Unsupported &amp; NoOracle)</div></div>\n",
        fmt_pct(report.summary.fidelity_pct())
    ));
    out.push_str(&format!(
        "<div class=\"pct-box\"><div class=\"pct\">{}</div><div class=\"pct-label\">Strict fidelity % (Unsupported counted as failure)</div></div>\n",
        fmt_pct(report.summary.strict_fidelity_pct())
    ));
    out.push_str("</div>\n");

    out.push_str("<dl class=\"meta\">\n");
    out.push_str(&format!(
        "<dt>Engine version</dt><dd class=\"mono\">{}</dd>\n",
        esc(&report.engine.version)
    ));
    out.push_str(&format!(
        "<dt>Git hash</dt><dd class=\"mono\">{}</dd>\n",
        esc(&report.engine.git_hash)
    ));
    out.push_str(&format!(
        "<dt>VBA project</dt><dd>{}</dd>\n",
        report.flags.has_vba_project
    ));
    out.push_str(&format!(
        "<dt>1904 date system</dt><dd>{}</dd>\n",
        report.flags.date_system_1904
    ));
    out.push_str(&format!(
        "<dt>Workbook calcMode</dt><dd class=\"mono\">{}</dd>\n",
        esc(report.flags.calc_mode)
    ));
    out.push_str("</dl>\n");

    out.push_str("<h2>Summary</h2>\n");
    out.push_str(&summary_table(&report.summary));

    out.push_str("<h2>Per-sheet breakdown</h2>\n");
    out.push_str(&per_sheet_table(&report.cells));

    out.push_str("<h2>Mismatches</h2>\n");
    out.push_str(&mismatch_table(&report.cells));

    out.push_str("</body></html>\n");
    out
}

fn summary_table(s: &WorkbookSummary) -> String {
    format!(
        "<table>\n<tr><th>Total formula cells</th><th>Exact</th><th>UlpDiff</th><th>Mismatch</th><th>Unsupported</th><th>NoOracle</th></tr>\n\
         <tr><td>{}</td><td class=\"status-exact\">{}</td><td class=\"status-ulp_diff\">{}</td><td class=\"status-mismatch\">{}</td><td class=\"status-engine_unsupported\">{}</td><td class=\"status-no_oracle\">{}</td></tr>\n</table>\n",
        s.total_formula_cells, s.exact, s.ulp_diff, s.mismatch, s.engine_unsupported, s.no_oracle
    )
}

fn per_sheet_table(cells: &[CellRecord]) -> String {
    let mut by_sheet: BTreeMap<&str, Vec<&CellRecord>> = BTreeMap::new();
    for c in cells {
        by_sheet.entry(c.sheet.as_str()).or_default().push(c);
    }
    let mut out = String::from(
        "<table>\n<tr><th>Sheet</th><th>Total</th><th>Exact</th><th>UlpDiff</th><th>Mismatch</th>\
         <th>Unsupported</th><th>NoOracle</th><th>Fidelity %</th><th>Strict %</th></tr>\n",
    );
    for (sheet, sheet_cells) in by_sheet {
        let owned: Vec<CellRecord> = sheet_cells.into_iter().cloned().collect();
        let s = WorkbookSummary::from_records(&owned);
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            esc(sheet),
            s.total_formula_cells,
            s.exact,
            s.ulp_diff,
            s.mismatch,
            s.engine_unsupported,
            s.no_oracle,
            fmt_pct(s.fidelity_pct()),
            fmt_pct(s.strict_fidelity_pct()),
        ));
    }
    out.push_str("</table>\n");
    out
}

fn mismatch_table(cells: &[CellRecord]) -> String {
    let mismatches: Vec<&CellRecord> = cells.iter().filter(|c| c.status.is_mismatch()).collect();
    if mismatches.is_empty() {
        return "<p>No mismatches.</p>\n".to_string();
    }
    let mut out = String::from(
        "<table>\n<tr><th>Sheet</th><th>Ref</th><th>Formula</th><th>Expected</th><th>Actual</th></tr>\n",
    );
    for c in mismatches.iter().take(MAX_MISMATCH_ROWS) {
        if let CellStatus::Mismatch { expected, actual } = &c.status {
            out.push_str(&format!(
                "<tr><td>{}</td><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td>{}</td><td>{}</td></tr>\n",
                esc(&c.sheet),
                esc(&c.cell_ref),
                esc(&c.formula),
                value_to_html(expected),
                value_to_html(actual),
            ));
        }
    }
    out.push_str("</table>\n");
    if mismatches.len() > MAX_MISMATCH_ROWS {
        out.push_str(&format!(
            "<p class=\"truncation-note\">Showing the first {MAX_MISMATCH_ROWS} of {} mismatches.</p>\n",
            mismatches.len()
        ));
    }
    out
}

/// Renders the corpus-level aggregate index for `recalc verify-dir`.
///
/// `html_links` maps a `CorpusEntryResult::Ok`'s `workbook_path` to the
/// relative href of its own per-file HTML report, when one was written
/// (`--html-dir`); files without an entry render as plain text (no link).
#[must_use]
pub fn corpus_index_to_html(
    entries: &[CorpusEntryResult],
    html_links: &BTreeMap<String, String>,
) -> String {
    let mut out = String::new();
    out.push_str("<!doctype html>\n<html><head><meta charset=\"utf-8\">");
    out.push_str("<title>Recalc corpus fidelity index</title>");
    out.push_str(&format!("<style>{STYLE}</style></head><body>\n"));
    out.push_str("<h1>Recalc corpus fidelity index</h1>\n");

    let total = entries.len();
    let ok = entries
        .iter()
        .filter(|e| matches!(e, CorpusEntryResult::Ok(_)))
        .count();
    let failures = total - ok;
    out.push_str(&format!(
        "<p class=\"meta\">{total} file(s) — {ok} loaded, {failures} load failure(s).</p>\n"
    ));

    out.push_str(
        "<table>\n<tr><th>File</th><th>Cells</th><th>Fidelity %</th><th>Strict %</th><th>Status</th></tr>\n",
    );
    for entry in entries {
        match entry {
            CorpusEntryResult::Ok(entry) => {
                let cell = match html_links.get(&entry.workbook_path) {
                    Some(href) => format!(
                        "<a href=\"{}\">{}</a>",
                        esc(href),
                        esc(&entry.workbook_path)
                    ),
                    None => esc(&entry.workbook_path),
                };
                let status = if entry.summary.has_mismatch() {
                    "<span class=\"status-mismatch\">mismatch</span>"
                } else {
                    "<span class=\"status-exact\">ok</span>"
                };
                out.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                    cell,
                    entry.summary.total_formula_cells,
                    fmt_pct(entry.summary.fidelity_pct()),
                    fmt_pct(entry.summary.strict_fidelity_pct()),
                    status,
                ));
            }
            CorpusEntryResult::LoadFailure { path, message } => {
                out.push_str(&format!(
                    "<tr><td>{}</td><td>—</td><td>—</td><td>—</td><td class=\"status-mismatch\">load failure: {}</td></tr>\n",
                    esc(path),
                    esc(message),
                ));
            }
        }
    }
    out.push_str("</table>\n</body></html>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{EngineMeta, WorkbookFlagsReport};

    fn sample_report() -> WorkbookReport {
        let cells = vec![
            CellRecord {
                sheet: "Sheet1".to_string(),
                cell_ref: "A1".to_string(),
                row: 0,
                col: 0,
                formula: "=1+1".to_string(),
                status: CellStatus::Exact,
            },
            CellRecord {
                sheet: "Sheet1".to_string(),
                cell_ref: "A2".to_string(),
                row: 1,
                col: 0,
                formula: "=1+1".to_string(),
                status: CellStatus::Mismatch {
                    expected: Value::number(2.0),
                    actual: Value::number(3.0),
                },
            },
        ];
        let summary = WorkbookSummary::from_records(&cells);
        WorkbookReport {
            workbook_path: "book.xlsx".to_string(),
            engine: EngineMeta {
                version: "0.1.0".to_string(),
                git_hash: "abc123".to_string(),
            },
            flags: WorkbookFlagsReport {
                has_vba_project: false,
                date_system_1904: false,
                calc_mode: "auto",
            },
            cells,
            summary,
        }
    }

    #[test]
    fn renders_self_contained_html_with_no_external_refs() {
        let html = workbook_report_to_html(&sample_report());
        assert!(html.contains("<html>"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("http://") && !html.contains("https://"));
        assert!(html.contains("A2"));
        assert!(html.contains("mismatch"));
    }

    #[test]
    fn escapes_formula_text() {
        let mut report = sample_report();
        report.cells[1].formula = "=A1<B1&\"x\"".to_string();
        let html = workbook_report_to_html(&report);
        assert!(html.contains("&lt;"));
        assert!(html.contains("&amp;"));
    }
}
