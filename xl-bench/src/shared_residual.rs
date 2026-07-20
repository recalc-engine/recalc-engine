//! Shared-formula residual instrumentation — *why* each bodyless shared
//! follow-on still declines, keyed to the **master formula text** that failed.
//!
//! # What this measures
//! After shared-formula expansion (`xl-engine/src/shared.rs`), a
//! `<f t="shared" si="N"/>` **follow-on** with no body is expanded by
//! translating its group **master**'s formula (ECMA-376 §18.17.2). The engine
//! records a master only when its body *parses* (`shared::collect_masters`), so
//! a follow-on still declines — landing in
//! [`crate::decline::DeclineClass::SharedFollowonUnexpanded`] — in exactly two
//! situations:
//!
//! 1. **orphan / missing master** — no master cell for `(sheet, si)` is present
//!    in the workbook at all (the master lives on another sheet, was never
//!    shipped, or the `si` is dangling). Not a parser gap; nothing to fix here.
//! 2. **unparseable master** — a master cell *is* present but its formula text
//!    fails [`xl_ast::parse`], so the group cannot be expanded. **This is the
//!    fixable residual**: every follow-on in the group inherits the master's
//!    parse failure.
//!
//! This module walks a corpus, and for every bodyless shared follow-on decides
//! which situation applies, **deduplicating unparseable master texts** and
//! tallying how many follow-on cells each one blocks. The ranked output is the
//! triage list for closing real `xl-ast` grammar gaps (Lane A). It emits
//! **formula text only** — never cell values, addresses, or workbook contents.
//!
//! # Why re-derive the master map here (not call the engine)
//! `xl-engine`'s `shared::collect_masters` is `pub(crate)`. Rather than widen
//! that interface, this module rebuilds the same `(sheet_index, si) -> master`
//! map from the already-public `xl-io` shapes (`RawFormula`/`FormulaKind`/
//! `shared_index`) and `xl_ast::parse` — the identical clean-room pattern
//! [`crate::decline`] already uses. It mirrors the engine's master-selection
//! rule exactly: a master is a `t="shared"` cell that carries **both** a body
//! and an `si`; when two cells claim the same `(sheet, si)` the later one in
//! `(row, col)` order wins (the engine's `BTreeMap::insert` semantics).
//!
//! # Provenance
//! ECMA-376 5th ed. Part 1, §18.17.2 (shared formulas) and §18.3.1.40
//! (`CT_CellFormula` / `ST_CellFormulaType`). The residual class it explains is
//! [`crate::decline::DeclineClass::SharedFollowonUnexpanded`].

use std::collections::BTreeMap;
use std::path::Path;

use xl_io::FormulaKind;

use crate::report::RunError;

/// A master cell's text and its cached parse outcome (`None` = parsed OK, `Some`
/// = the `ParseErrorKind`'s display string). Parsing once per master keeps the
/// per-workbook cost linear in distinct masters, not in follow-ons.
struct MasterInfo {
    text: String,
    /// `None` if the body parses; else the parse-error *kind* string (span
    /// dropped, so the same grammatical failure deduplicates cleanly).
    parse_err: Option<String>,
}

/// One deduplicated unparseable-master record, in the corpus-wide ranking.
#[derive(Clone, Debug)]
pub struct FailingMaster {
    /// The master's exact formula text (no leading `=`), as stored in `<f>`.
    pub text: String,
    /// The `ParseErrorKind` display string (the failure class).
    pub parse_error: String,
    /// Total follow-on cells this master text blocks across the corpus.
    pub followon_cells: usize,
    /// Distinct workbooks in which this master text failed to parse.
    pub workbooks: usize,
}

/// One workbook's shared-follow-on accounting (the pure per-file result).
#[derive(Clone, Debug, Default)]
pub struct WorkbookSharedResidual {
    /// Bodyless `t="shared"` follow-on cells seen (the population of interest).
    pub total_followons: usize,
    /// Follow-ons whose master is present and parses (the engine expands these).
    pub would_expand: usize,
    /// Follow-ons whose `(sheet, si)` master is absent from the workbook.
    pub orphan_missing: usize,
    /// Follow-ons whose master is present but whose body fails to parse.
    pub parse_fail_followons: usize,
    /// Bodyless follow-ons carrying no `si` at all (malformed) — cannot be keyed.
    pub malformed_no_si: usize,
    /// Per unparseable-master **text** → (parse-error string, follow-on count)
    /// within this one workbook. Folded into the corpus tally with distinct-
    /// workbook counting.
    pub fails: BTreeMap<String, (String, usize)>,
}

/// Attribute one workbook's bodyless shared follow-ons (pure, engine-free).
///
/// Mirrors `xl-engine`'s load-time expansion decision: build the master map
/// (`t="shared"` cells with a body and an `si`; later `(row,col)` wins), then
/// for each bodyless follow-on classify by whether its `(sheet, si)` master is
/// missing, unparseable, or parseable.
#[must_use]
pub fn attribute_workbook_pure(workbook: &xl_io::Workbook) -> WorkbookSharedResidual {
    // Pass 1: collect masters, parsing each once. Keyed (sheet_index, si).
    let mut masters: BTreeMap<(u32, u32), MasterInfo> = BTreeMap::new();
    for (idx, sheet) in workbook.sheets.iter().enumerate() {
        let sidx = idx as u32;
        for cell in sheet.cells.values() {
            let Some(raw) = &cell.formula else { continue };
            if raw.kind != FormulaKind::Shared {
                continue;
            }
            let (Some(text), Some(si)) = (&raw.text, raw.shared_index) else {
                continue;
            };
            // `sheet.cells` is a BTreeMap, so this iterates in (row, col) order;
            // a later insert overwrites an earlier one, matching the engine.
            let parse_err = xl_ast::parse(text).err().map(|e| e.kind.to_string());
            masters.insert(
                (sidx, si),
                MasterInfo {
                    text: text.clone(),
                    parse_err,
                },
            );
        }
    }

    // Pass 2: classify every bodyless shared follow-on.
    let mut out = WorkbookSharedResidual::default();
    for (idx, sheet) in workbook.sheets.iter().enumerate() {
        let sidx = idx as u32;
        for cell in sheet.cells.values() {
            let Some(raw) = &cell.formula else { continue };
            if raw.kind != FormulaKind::Shared || raw.text.is_some() {
                continue; // masters + non-shared cells are not follow-ons
            }
            out.total_followons += 1;
            let Some(si) = raw.shared_index else {
                out.malformed_no_si += 1;
                continue;
            };
            match masters.get(&(sidx, si)) {
                None => out.orphan_missing += 1,
                Some(mi) => match &mi.parse_err {
                    None => out.would_expand += 1,
                    Some(err) => {
                        out.parse_fail_followons += 1;
                        let entry = out
                            .fails
                            .entry(mi.text.clone())
                            .or_insert_with(|| (err.clone(), 0));
                        entry.1 += 1;
                    }
                },
            }
        }
    }
    out
}

/// Open a workbook and attribute its shared-follow-on residual.
pub fn attribute_workbook(path: &Path) -> Result<WorkbookSharedResidual, RunError> {
    let workbook = xl_io::open(path).map_err(RunError::Load)?;
    Ok(attribute_workbook_pure(&workbook))
}

/// Corpus-wide shared-residual accumulator. Fold one
/// [`WorkbookSharedResidual`] per workbook, then [`render`](SharedResidualTally::render).
#[derive(Clone, Debug, Default)]
pub struct SharedResidualTally {
    pub workbooks: usize,
    pub load_failures: usize,
    pub total_followons: usize,
    pub would_expand: usize,
    pub orphan_missing: usize,
    pub parse_fail_followons: usize,
    pub malformed_no_si: usize,
    /// text -> (parse-error string, total follow-on cells, distinct workbooks).
    fails: BTreeMap<String, (String, usize, usize)>,
}

impl SharedResidualTally {
    /// Fold one workbook's result into the running totals (distinct-workbook
    /// counting for each unparseable-master text).
    pub fn fold(&mut self, r: &WorkbookSharedResidual) {
        self.workbooks += 1;
        self.total_followons += r.total_followons;
        self.would_expand += r.would_expand;
        self.orphan_missing += r.orphan_missing;
        self.parse_fail_followons += r.parse_fail_followons;
        self.malformed_no_si += r.malformed_no_si;
        for (text, (err, cnt)) in &r.fails {
            let e = self
                .fails
                .entry(text.clone())
                .or_insert_with(|| (err.clone(), 0, 0));
            e.1 += *cnt;
            e.2 += 1;
        }
    }

    /// Record a workbook that failed to load (excluded from all counts).
    pub fn note_load_failure(&mut self) {
        self.load_failures += 1;
    }

    /// The deduplicated unparseable-master ranking, most-blocking first (ties
    /// broken by workbook count desc, then text asc for stability).
    #[must_use]
    pub fn ranked_failures(&self) -> Vec<FailingMaster> {
        let mut v: Vec<FailingMaster> = self
            .fails
            .iter()
            .map(|(text, (err, cnt, wbs))| FailingMaster {
                text: text.clone(),
                parse_error: err.clone(),
                followon_cells: *cnt,
                workbooks: *wbs,
            })
            .collect();
        v.sort_by(|a, b| {
            b.followon_cells
                .cmp(&a.followon_cells)
                .then_with(|| b.workbooks.cmp(&a.workbooks))
                .then_with(|| a.text.cmp(&b.text))
        });
        v
    }

    /// Distinct unparseable-master texts seen.
    #[must_use]
    pub fn distinct_failing_masters(&self) -> usize {
        self.fails.len()
    }

    /// Render the human-readable report. `top_n` caps the failing-master list;
    /// `max_text` truncates each formula text for display (0 = no truncation).
    #[must_use]
    pub fn render(&self, top_n: usize, max_text: usize) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(s, "== Shared-formula residual (unexpanded follow-ons) ==");
        let _ = writeln!(
            s,
            "Why each bodyless <f t=\"shared\"/> follow-on still declines. Two buckets:"
        );
        let _ = writeln!(
            s,
            "  orphan_missing = no master cell for (sheet, si); parse_fail = master present but"
        );
        let _ = writeln!(
            s,
            "  its body fails xl_ast::parse (the fixable grammar residual). Formula text only.\n"
        );
        let _ = writeln!(
            s,
            "workbooks: {} attributed, {} load failure(s) (excluded)",
            self.workbooks, self.load_failures
        );
        let pct = |n: usize| -> f64 {
            if self.total_followons == 0 {
                0.0
            } else {
                n as f64 / self.total_followons as f64 * 100.0
            }
        };
        let _ = writeln!(s, "bodyless shared follow-ons: {}", self.total_followons);
        let _ = writeln!(
            s,
            "  would_expand (master parses):   {:>10}  ({:6.2}%)",
            self.would_expand,
            pct(self.would_expand)
        );
        let _ = writeln!(
            s,
            "  orphan_missing (no master):     {:>10}  ({:6.2}%)",
            self.orphan_missing,
            pct(self.orphan_missing)
        );
        let _ = writeln!(
            s,
            "  parse_fail (master unparseable):{:>10}  ({:6.2}%)",
            self.parse_fail_followons,
            pct(self.parse_fail_followons)
        );
        let _ = writeln!(
            s,
            "  malformed_no_si:                {:>10}  ({:6.2}%)",
            self.malformed_no_si,
            pct(self.malformed_no_si)
        );

        // Parse-error-kind rollup: which grammatical failures dominate.
        let mut by_err: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for (err, cnt, _wbs) in self.fails.values() {
            let e = by_err.entry(err.clone()).or_insert((0, 0));
            e.0 += *cnt; // follow-on cells
            e.1 += 1; // distinct master texts
        }
        let mut err_rank: Vec<(String, usize, usize)> =
            by_err.into_iter().map(|(k, (c, m))| (k, c, m)).collect();
        err_rank.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let _ = writeln!(
            s,
            "\n== parse-error-kind rollup (follow-on cells / distinct masters) — {} distinct masters ==",
            self.distinct_failing_masters()
        );
        for (err, cells, masters) in &err_rank {
            let _ = writeln!(s, "  {cells:>10} cells / {masters:>6} masters   {err}");
        }

        let ranking = self.ranked_failures();
        let _ = writeln!(
            s,
            "\n== top {top_n} unparseable master formulas (follow-on cells; distinct workbooks) =="
        );
        if ranking.is_empty() {
            let _ = writeln!(s, "  (none)");
        }
        for fm in ranking.iter().take(top_n) {
            let shown = if max_text > 0 && fm.text.len() > max_text {
                // Truncate on a char boundary to keep the output valid UTF-8.
                let mut end = max_text;
                while !fm.text.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}…", &fm.text[..end])
            } else {
                fm.text.clone()
            };
            let _ = writeln!(
                s,
                "  [{:>7} cells / {:>5} wbs] {} :: ={}",
                fm.followon_cells, fm.workbooks, fm.parse_error, shown
            );
        }
        s
    }

    /// A single machine-parseable summary line for status-log capture.
    #[must_use]
    pub fn summary_line(&self) -> String {
        format!(
            "[SHARED-RESIDUAL] followons={} would_expand={} orphan_missing={} parse_fail={} malformed_no_si={} distinct_masters={} (wbs={}, load_fail={})",
            self.total_followons,
            self.would_expand,
            self.orphan_missing,
            self.parse_fail_followons,
            self.malformed_no_si,
            self.distinct_failing_masters(),
            self.workbooks,
            self.load_failures,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap as Map, BTreeSet};
    use xl_io::{
        CalcSettings, Cell, DateSystem, NumFmtId, RawFormula, Sheet, Workbook, WorkbookFlags,
    };
    use xl_value::Value;

    /// A shared **master** cell: body + si.
    fn master(text: &str, si: u32) -> Cell {
        Cell {
            value: Value::Blank,
            formula: Some(RawFormula {
                text: Some(text.to_string()),
                kind: FormulaKind::Shared,
                shared_index: Some(si),
                range: None,
            }),
            num_fmt: NumFmtId::general(),
        }
    }

    /// A shared **follow-on** cell: no body, just si.
    fn followon(si: Option<u32>) -> Cell {
        Cell {
            value: Value::Blank,
            formula: Some(RawFormula {
                text: None,
                kind: FormulaKind::Shared,
                shared_index: si,
                range: None,
            }),
            num_fmt: NumFmtId::general(),
        }
    }

    fn wb_one_sheet(cells: Vec<((u32, u32), Cell)>) -> Workbook {
        let mut map: Map<(u32, u32), Cell> = Map::new();
        for (coord, c) in cells {
            map.insert(coord, c);
        }
        let sheet = Sheet {
            name: "Sheet1".to_string(),
            sheet_id: 1,
            cells: map,
            hidden_rows: BTreeSet::new(),
        };
        Workbook {
            sheets: vec![sheet],
            date_system: DateSystem::default(),
            calc_settings: CalcSettings::default(),
            defined_names: Vec::new(),
            flags: WorkbookFlags::default(),
        }
    }

    #[test]
    fn parseable_master_makes_followons_expandable() {
        let wb = wb_one_sheet(vec![
            ((0, 0), master("A1*B1", 0)),
            ((1, 0), followon(Some(0))),
            ((2, 0), followon(Some(0))),
        ]);
        let r = attribute_workbook_pure(&wb);
        assert_eq!(r.total_followons, 2);
        assert_eq!(r.would_expand, 2);
        assert_eq!(r.parse_fail_followons, 0);
        assert_eq!(r.orphan_missing, 0);
        assert!(r.fails.is_empty());
    }

    #[test]
    fn unparseable_master_blocks_and_dedups_followons() {
        // `SUM(` never closes → a parse error; both follow-ons inherit it.
        let wb = wb_one_sheet(vec![
            ((0, 0), master("SUM(", 0)),
            ((1, 0), followon(Some(0))),
            ((2, 0), followon(Some(0))),
        ]);
        let r = attribute_workbook_pure(&wb);
        assert_eq!(r.total_followons, 2);
        assert_eq!(r.parse_fail_followons, 2);
        assert_eq!(r.would_expand, 0);
        assert_eq!(r.fails.len(), 1, "one distinct master text");
        let (_err, cnt) = r.fails.get("SUM(").expect("master text keyed");
        assert_eq!(*cnt, 2);
    }

    #[test]
    fn missing_master_is_orphan_not_parse_fail() {
        let wb = wb_one_sheet(vec![((1, 0), followon(Some(7)))]);
        let r = attribute_workbook_pure(&wb);
        assert_eq!(r.total_followons, 1);
        assert_eq!(r.orphan_missing, 1);
        assert_eq!(r.parse_fail_followons, 0);
    }

    #[test]
    fn followon_without_si_is_malformed() {
        let wb = wb_one_sheet(vec![((1, 0), followon(None))]);
        let r = attribute_workbook_pure(&wb);
        assert_eq!(r.malformed_no_si, 1);
        assert_eq!(r.orphan_missing, 0);
    }

    #[test]
    fn tally_counts_distinct_workbooks_per_text() {
        let mk = || {
            wb_one_sheet(vec![
                ((0, 0), master("SUM(", 0)),
                ((1, 0), followon(Some(0))),
            ])
        };
        let mut tally = SharedResidualTally::default();
        tally.fold(&attribute_workbook_pure(&mk()));
        tally.fold(&attribute_workbook_pure(&mk()));
        let ranked = tally.ranked_failures();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].followon_cells, 2, "1 per workbook × 2");
        assert_eq!(ranked[0].workbooks, 2, "seen in 2 distinct workbooks");
    }
}
