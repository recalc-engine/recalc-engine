//! Tier-0-only fidelity cut — the M1 gate measurement.
//!
//! The overall corpus fidelity number (`recalc verify-dir`) is measured on the
//! *full* corpus, which is heavily poisoned by out-of-scope VBA UDFs and
//! `_XLL.*` add-in cells. The M1 milestone gate, however, is stated against a
//! **Tier-0-only** slice: *"≥99.9% cell agreement on Tier-0-only workbooks"*
//! (`implementation-plan.md` §5). This module computes that slice.
//!
//! # Provenance / scope caption — READ THIS
//! Per `project decision (Tier-0 number scope)`, 2026-07-12, the number this
//! module produces is **`[INTERNAL/T0]`**: it is computed on the *private*
//! Enron oracle corpus with a methodology (the [`T0_SET_V1`] membership and the
//! cell-/workbook-level predicate) that is **not frozen**. It is an engineering
//! steering signal for the M1 gate, **not a publishable fidelity claim**.
//! Wherever it is quoted it must carry the `[INTERNAL/T0]` caption and name the
//! predicate + T0-set version. It must never leave repo-internal surfaces.
//!
//! # What "Tier-0" means here (T0-set v1)
//! [`T0_SET_V1`] is an explicit, versioned allow-list derived from the plan's
//! Tier-0 enumeration ("~40 fns: SUM, IF, VLOOKUP, INDEX/MATCH, SUMIF/COUNTIF
//! families, IFERROR, text/date workhorses, OFFSET, INDIRECT"), expanded to the
//! standard family/workhorse members. It is deliberately **inclusive**: a
//! rarely-used member barely moves the cell-level number, whereas wrongly
//! *excluding* a genuinely-core function would understate the gap. The set is
//! versioned precisely because membership is a judgement call that may be
//! refined; do not treat it as frozen.
//!
//! # Two predicates, three cuts
//! A formula cell is **spec-eligible** if it parses cleanly, contains no
//! `Unsupported` construct, and every function it calls is in [`T0_SET_V1`]
//! (a cell with *no* function calls — pure arithmetic/refs — is trivially
//! eligible; that is core-supported behaviour). It is additionally
//! **supported-eligible** if every call is also currently *registered* in
//! `xl-fn` (i.e. not in [`T0_UNSUPPORTED_V1`]).
//!
//! - **Cell-level, spec** — every spec-eligible cell in the corpus. The primary
//!   M1-gate reading of "how faithful are we on Tier-0 vocabulary", *including*
//!   coverage gaps (an unregistered Tier-0 function scores as a failure).
//! - **Cell-level, supported** — the subset whose calls are all registered. A
//!   *correctness ceiling*: how faithful our existing Tier-0 implementations are,
//!   with coverage gaps factored out.
//! - **Workbook-level, spec** — only cells in workbooks where *every* formula
//!   cell is spec-eligible. The most literal reading of "Tier-0-only workbooks";
//!   expected to have a small sample on a UDF-poisoned corpus.
//!
//! Shared/array follow-on cells (stored with no formula body) have unknowable
//! vocabulary and are conservatively classified [`VocabClass::NonTier0`].
//!
//! Buckets and the strict/lenient formulas mirror
//! [`crate::report::WorkbookSummary`] exactly, so the Tier-0 number is directly
//! comparable to the corpus number.

use std::collections::{BTreeMap, BTreeSet};

use xl_ast::{Expr, ExprKind, RefKind, parse};

use crate::diff::CellStatus;
use crate::report::CellRecord;

/// The pinned Tier-0 function allow-list, **version 1** (2026-07-12). Grouped
/// by role; see the module docs for the inclusion rationale. Names are the
/// canonical (uppercased, prefix-stripped) forms the parser produces.
pub const T0_SET_V1: &[&str] = &[
    // Aggregation / math workhorses.
    "SUM",
    "AVERAGE",
    "MIN",
    "MAX",
    "COUNT",
    "COUNTA",
    "ROUND",
    "ROUNDUP",
    "ROUNDDOWN",
    "INT",
    "ABS",
    "MOD",
    "PRODUCT",
    // Conditional (SUMIF / COUNTIF) families.
    "SUMIF",
    "SUMIFS",
    "COUNTIF",
    "COUNTIFS",
    "AVERAGEIF",
    "AVERAGEIFS",
    // Logical + error handling.
    "IF",
    "IFERROR",
    "IFNA",
    "AND",
    "OR",
    "NOT",
    // IS* / information.
    "ISBLANK",
    "ISERROR",
    "ISNUMBER",
    "ISTEXT",
    "ISNA",
    "NA",
    // Lookup / reference.
    "VLOOKUP",
    "HLOOKUP",
    "INDEX",
    "MATCH",
    "OFFSET",
    "INDIRECT",
    "CHOOSE",
    "ROW",
    "COLUMN",
    // Text workhorses.
    "LEFT",
    "RIGHT",
    "MID",
    "LEN",
    "TRIM",
    "UPPER",
    "LOWER",
    "CONCATENATE",
    "TEXT",
    "FIND",
    "SUBSTITUTE",
    "VALUE",
    // Date workhorses.
    "DATE",
    "YEAR",
    "MONTH",
    "DAY",
    "TODAY",
    "NOW",
    "WEEKDAY",
    "DATEVALUE",
    "EDATE",
    "EOMONTH",
];

/// Members of [`T0_SET_V1`] that are **not computed** by `xl-fn`/`xl-engine`
/// (they evaluate to `#UNSUPPORTED!`). Kept as an explicit constant — rather
/// than derived from the registry — so this harness stays free of any `xl-fn`
/// dependency. If a function here is later implemented, move it out; CI does
/// not police this list, so a stale entry only mislabels the spec-vs-supported
/// split, never the raw buckets.
///
/// **v2 (2026-07-12, T0-set v2):** dropped **OFFSET** and **INDIRECT** — the
/// M1-backlog investigation (and `xl-engine/src/refx.rs`) confirmed both are
/// implemented as RFC-0003 reference-transformers (not registry functions), so
/// listing them here was a false positive. Dropped **AVERAGEIF/AVERAGEIFS/IFNA**
/// — implemented in the M1 batch-1 wave. Only the genuinely-uncomputed
/// volatile anti-targets remain (TODAY/NOW: their cached oracle value is the
/// file's unreproducible save-timestamp, so they are deliberately left
/// uncomputed — implementing them would turn explicit `#UNSUPPORTED!` into silent
/// mismatch).
pub const T0_UNSUPPORTED_V1: &[&str] = &["TODAY", "NOW"];

/// A formula cell's Tier-0 classification (see module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VocabClass {
    /// Every call is in [`T0_SET_V1`] and registered in `xl-fn`.
    Tier0Supported,
    /// Every call is in [`T0_SET_V1`], but at least one is in
    /// [`T0_UNSUPPORTED_V1`] (a coverage gap).
    Tier0SpecOnly,
    /// Not Tier-0: a parse failure, an `Unsupported` construct, or a call
    /// outside [`T0_SET_V1`].
    NonTier0,
}

/// Recursively collect every function-call name in `expr`, and note whether the
/// tree contains any `Unsupported` construct (which disqualifies the cell).
fn collect_calls(expr: &Expr, calls: &mut BTreeSet<String>, has_unsupported: &mut bool) {
    match &expr.kind {
        ExprKind::Number(_)
        | ExprKind::Text(_)
        | ExprKind::Bool(_)
        | ExprKind::Error(_)
        | ExprKind::Missing
        | ExprKind::Ref(_)
        | ExprKind::Name(_) => {}
        ExprKind::Unary { expr, .. }
        | ExprKind::Postfix { expr, .. }
        | ExprKind::Paren(expr)
        | ExprKind::ImplicitIntersection(expr) => collect_calls(expr, calls, has_unsupported),
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_calls(lhs, calls, has_unsupported);
            collect_calls(rhs, calls, has_unsupported);
        }
        ExprKind::Call { name, args } => {
            calls.insert(name.canonical.to_ascii_uppercase());
            for a in args {
                collect_calls(a, calls, has_unsupported);
            }
        }
        ExprKind::Array(rows) => {
            for row in rows {
                for e in row {
                    collect_calls(e, calls, has_unsupported);
                }
            }
        }
        ExprKind::Unsupported { .. } => *has_unsupported = true,
    }
}

/// The construct classes an `#UNSUPPORTED!` spec-eligible cell can be
/// attributed to — the "corpus attribution probe" the M1 backlog asked for.
/// A spec-eligible cell has no `ExprKind::Unsupported` node by construction
/// (those are classified [`VocabClass::NonTier0`]), so **structured/table
/// references never appear here** — the residual unsupported must come from a
/// reference-form construct or from a precedent cascade (a Tier-0 formula whose
/// input cell is itself `#UNSUPPORTED!`, e.g. a UDF), the latter showing up as
/// a cell with *none* of these constructs.
fn detect_constructs(expr: &Expr, out: &mut BTreeSet<&'static str>) {
    match &expr.kind {
        ExprKind::Ref(r) => {
            match r.kind {
                RefKind::Col(_) => {
                    out.insert("whole-column");
                }
                RefKind::Row(_) => {
                    out.insert("whole-row");
                }
                RefKind::R1C1(_) => {
                    out.insert("r1c1");
                }
                RefKind::Cell(_, _) => {}
            }
            if let Some(s) = &r.sheet
                && s.last.is_some()
            {
                out.insert("3d-ref");
            }
        }
        ExprKind::Array(rows) => {
            out.insert("array-literal");
            for row in rows {
                for e in row {
                    detect_constructs(e, out);
                }
            }
        }
        ExprKind::ImplicitIntersection(inner) => {
            out.insert("implicit-intersection");
            detect_constructs(inner, out);
        }
        ExprKind::Unary { expr, .. } | ExprKind::Postfix { expr, .. } | ExprKind::Paren(expr) => {
            detect_constructs(expr, out)
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            detect_constructs(lhs, out);
            detect_constructs(rhs, out);
        }
        ExprKind::Call { args, .. } => {
            for a in args {
                detect_constructs(a, out);
            }
        }
        _ => {}
    }
}

/// Classify one formula string (leading `=` optional — [`parse`] strips it).
#[must_use]
pub fn classify_formula(
    formula: &str,
    spec: &BTreeSet<&str>,
    unsupported: &BTreeSet<&str>,
) -> VocabClass {
    let Ok(expr) = parse(formula) else {
        return VocabClass::NonTier0;
    };
    let mut calls = BTreeSet::new();
    let mut has_unsupported = false;
    collect_calls(&expr, &mut calls, &mut has_unsupported);
    if has_unsupported {
        return VocabClass::NonTier0;
    }
    if !calls.iter().all(|c| spec.contains(c.as_str())) {
        return VocabClass::NonTier0;
    }
    // Spec-eligible. Supported iff no call is in the unsupported set.
    if calls.iter().any(|c| unsupported.contains(c.as_str())) {
        VocabClass::Tier0SpecOnly
    } else {
        VocabClass::Tier0Supported
    }
}

/// The five scoring buckets for one cut, mirroring [`crate::report::WorkbookSummary`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Buckets {
    pub total: usize,
    pub exact: usize,
    pub ulp_diff: usize,
    pub mismatch: usize,
    pub engine_unsupported: usize,
    pub no_oracle: usize,
}

impl Buckets {
    fn add(&mut self, status: &CellStatus) {
        self.total += 1;
        match status {
            CellStatus::Exact => self.exact += 1,
            CellStatus::UlpDiff { .. } => self.ulp_diff += 1,
            CellStatus::Mismatch { .. } => self.mismatch += 1,
            CellStatus::EngineUnsupported => self.engine_unsupported += 1,
            CellStatus::NoOracle => self.no_oracle += 1,
        }
    }

    /// Strict fidelity %: `(exact + ulp_diff) / (total − no_oracle)`.
    #[must_use]
    pub fn strict_pct(&self) -> f64 {
        let denom = self.total.saturating_sub(self.no_oracle);
        if denom == 0 {
            return 0.0;
        }
        (self.exact + self.ulp_diff) as f64 / denom as f64 * 100.0
    }

    /// Lenient fidelity %: `(exact + ulp_diff) / (total − unsupported − no_oracle)`.
    #[must_use]
    pub fn lenient_pct(&self) -> f64 {
        let denom = self
            .total
            .saturating_sub(self.engine_unsupported)
            .saturating_sub(self.no_oracle);
        if denom == 0 {
            return 0.0;
        }
        (self.exact + self.ulp_diff) as f64 / denom as f64 * 100.0
    }
}

/// Per-function attribution over spec-eligible cells: how a function's cells
/// scored. Powers the M1 gap ranking (the backlog generator).
#[derive(Clone, Copy, Debug, Default)]
pub struct FnGap {
    /// Spec-eligible cells whose vocabulary includes this function.
    pub appearances: usize,
    /// …of which scored Mismatch.
    pub mismatch: usize,
    /// …of which scored EngineUnsupported.
    pub engine_unsupported: usize,
    /// …of which scored Exact.
    pub exact: usize,
}

impl FnGap {
    /// Cells this function is implicated in that are *not* correct and *not*
    /// unscored — the size of the gap it contributes to.
    #[must_use]
    pub fn bad(&self) -> usize {
        self.mismatch + self.engine_unsupported
    }
}

/// The full Tier-0 cut over a corpus.
#[derive(Clone, Debug, Default)]
pub struct Tier0Cut {
    /// All formula cells seen across the corpus (sanity check vs `verify-dir`).
    pub corpus_total: usize,
    /// Cell-level, spec-eligible cut.
    pub cell_spec: Buckets,
    /// Cell-level, supported-eligible cut.
    pub cell_supported: Buckets,
    /// Workbook-level, spec-eligible cut (cells in all-Tier-0 workbooks).
    pub wb_spec: Buckets,
    /// Number of workbooks that are entirely spec-eligible.
    pub wb_qualifying: usize,
    /// Number of workbooks examined (that loaded and had ≥1 formula cell).
    pub wb_examined: usize,
    /// Per-function gap attribution over spec-eligible cells.
    pub fn_gaps: BTreeMap<String, FnGap>,
    /// Over the cell-level SPEC `#UNSUPPORTED!` cells: how many contain each
    /// construct class ([`detect_constructs`]). A cell may contain several, so
    /// these overlap and need not sum to the unsupported total.
    pub unsup_constructs: BTreeMap<&'static str, usize>,
    /// Cell-level SPEC `#UNSUPPORTED!` cells that contain **no** flagged
    /// construct — the residual is a precedent cascade (the formula's own
    /// vocabulary is fine, but an input cell is itself `#UNSUPPORTED!`).
    pub unsup_cascade: usize,
    /// Cell-level SPEC `#UNSUPPORTED!` cells whose own formula names a volatile
    /// anti-target (TODAY/NOW, e.g. `WEEKDAY(TODAY())`). A direct lower bound on
    /// the volatile ceiling (cached value = unreproducible save-date).
    pub unsup_volatile_in_formula: usize,
    /// Workbook-level (clean, no-UDF) analogue of [`Self::unsup_constructs`]:
    /// construct classes over the `#UNSUPPORTED!` cells of entirely-Tier-0
    /// workbooks. This is the M1-gate-relevant split (no UDF cascade possible).
    pub wb_unsup_constructs: BTreeMap<&'static str, usize>,
    /// Workbook-level (clean) precedent-cascade `#UNSUPPORTED!` count.
    pub wb_unsup_cascade: usize,
    /// Diagnostic sampling config: collect up to [`Self::sample_cap`] example
    /// `Mismatch` cells whose formula names one of these functions. Empty = off.
    pub sample_fns: BTreeSet<String>,
    /// Cap on collected mismatch samples.
    pub sample_cap: usize,
    /// Collected `(sheet!ref, formula, actual → expected)` mismatch samples —
    /// the never-guess evidence needed to author a targeted farm probe.
    pub mismatch_samples: Vec<String>,
    /// Functions to sample `#UNSUPPORTED!` cells for (e.g. OFFSET/INDIRECT — to
    /// see which unhandled forms drive the cascade). Empty = off.
    pub unsup_sample_fns: BTreeSet<String>,
    /// Collected `(sheet!ref, formula)` samples of `#UNSUPPORTED!` cells whose
    /// formula names an [`Self::unsup_sample_fns`] function.
    pub unsupported_samples: Vec<String>,
}

impl Tier0Cut {
    /// Fold one workbook's per-cell records into the running cut. Call once per
    /// workbook, then drop the records — this keeps memory bounded over a
    /// multi-million-cell corpus.
    pub fn fold_workbook(&mut self, records: &[CellRecord], wb_path: &str) {
        let spec: BTreeSet<&str> = T0_SET_V1.iter().copied().collect();
        let unsupported: BTreeSet<&str> = T0_UNSUPPORTED_V1.iter().copied().collect();

        let mut wb_has_formula = false;
        let mut wb_all_tier0 = true;
        // Buffer this workbook's spec-eligible records so we can commit them to
        // the workbook-level cut only if the whole workbook qualifies.
        let mut wb_spec_statuses: Vec<CellStatus> = Vec::new();
        // Per unsupported spec cell in this workbook: its construct classes
        // (empty = precedent cascade). Committed to the wb-level breakdown only
        // if the workbook qualifies as entirely-Tier-0.
        let mut wb_unsup_buf: Vec<Vec<&'static str>> = Vec::new();

        for rec in records {
            wb_has_formula = true;
            let class = classify_formula(&rec.formula, &spec, &unsupported);
            self.corpus_total += 1;

            match class {
                VocabClass::NonTier0 => {
                    wb_all_tier0 = false;
                }
                VocabClass::Tier0SpecOnly | VocabClass::Tier0Supported => {
                    self.cell_spec.add(&rec.status);
                    if class == VocabClass::Tier0Supported {
                        self.cell_supported.add(&rec.status);
                    }
                    wb_spec_statuses.push(rec.status.clone());
                    self.attribute_fns(&rec.formula, &rec.status, &spec, &unsupported);
                    // Diagnostic: sample Mismatch cells naming a target function
                    // (never-guess evidence for authoring a farm probe).
                    if !self.sample_fns.is_empty()
                        && self.mismatch_samples.len() < self.sample_cap
                        && let CellStatus::Mismatch { expected, actual } = &rec.status
                        && let Ok(expr) = parse(&rec.formula)
                    {
                        let mut calls = BTreeSet::new();
                        let mut dummy = false;
                        collect_calls(&expr, &mut calls, &mut dummy);
                        if calls.iter().any(|c| self.sample_fns.contains(c)) {
                            self.mismatch_samples.push(format!(
                                "[{}] {}!{}  {}  =>  actual={:?}  expected={:?}",
                                wb_path, rec.sheet, rec.cell_ref, rec.formula, actual, expected
                            ));
                        }
                    }
                    // Diagnostic: sample #UNSUPPORTED! cells naming a target
                    // function (which unhandled forms drive the cascade).
                    if !self.unsup_sample_fns.is_empty()
                        && self.unsupported_samples.len() < self.sample_cap
                        && matches!(rec.status, CellStatus::EngineUnsupported)
                        && let Ok(expr) = parse(&rec.formula)
                    {
                        let mut calls = BTreeSet::new();
                        let mut dummy = false;
                        collect_calls(&expr, &mut calls, &mut dummy);
                        if calls.iter().any(|c| self.unsup_sample_fns.contains(c)) {
                            self.unsupported_samples.push(format!(
                                "[{}] {}!{}  {}",
                                wb_path, rec.sheet, rec.cell_ref, rec.formula
                            ));
                        }
                    }
                    // Attribute each #UNSUPPORTED! cell to a construct class (or
                    // to precedent-cascade if none) — the M1 "which lever" probe
                    // — and flag whether its own formula names a volatile
                    // anti-target (TODAY/NOW, incl. `WEEKDAY(TODAY())`), which
                    // bounds the volatile ceiling directly.
                    if matches!(rec.status, CellStatus::EngineUnsupported) {
                        let mut cell_cons: Vec<&'static str> = Vec::new();
                        if let Ok(expr) = parse(&rec.formula) {
                            let mut cons = BTreeSet::new();
                            detect_constructs(&expr, &mut cons);
                            let mut calls = BTreeSet::new();
                            let mut dummy = false;
                            collect_calls(&expr, &mut calls, &mut dummy);
                            if calls.contains("TODAY") || calls.contains("NOW") {
                                self.unsup_volatile_in_formula += 1;
                            }
                            if cons.is_empty() {
                                self.unsup_cascade += 1;
                            } else {
                                for c in cons {
                                    *self.unsup_constructs.entry(c).or_default() += 1;
                                    cell_cons.push(c);
                                }
                            }
                        } else {
                            self.unsup_cascade += 1;
                        }
                        wb_unsup_buf.push(cell_cons);
                    }
                }
            }
        }

        if wb_has_formula {
            self.wb_examined += 1;
            if wb_all_tier0 {
                self.wb_qualifying += 1;
                for st in &wb_spec_statuses {
                    self.wb_spec.add(st);
                }
                // Commit this clean (no-UDF) workbook's unsupported attribution.
                for cons in &wb_unsup_buf {
                    if cons.is_empty() {
                        self.wb_unsup_cascade += 1;
                    } else {
                        for c in cons {
                            *self.wb_unsup_constructs.entry(c).or_default() += 1;
                        }
                    }
                }
            }
        }
    }

    /// Attribute a spec-eligible cell's outcome to each function it calls.
    fn attribute_fns(
        &mut self,
        formula: &str,
        status: &CellStatus,
        spec: &BTreeSet<&str>,
        _unsupported: &BTreeSet<&str>,
    ) {
        let Ok(expr) = parse(formula) else { return };
        let mut calls = BTreeSet::new();
        let mut dummy = false;
        collect_calls(&expr, &mut calls, &mut dummy);
        for c in &calls {
            if !spec.contains(c.as_str()) {
                continue;
            }
            let g = self.fn_gaps.entry(c.clone()).or_default();
            g.appearances += 1;
            match status {
                CellStatus::Mismatch { .. } => g.mismatch += 1,
                CellStatus::EngineUnsupported => g.engine_unsupported += 1,
                CellStatus::Exact => g.exact += 1,
                _ => {}
            }
        }
    }

    /// The M1 gap ranking: spec-eligible functions ordered by the number of
    /// bad (Mismatch + EngineUnsupported) cells they appear in, descending.
    /// This is the data-driven M1 correctness backlog.
    #[must_use]
    pub fn gap_ranking(&self) -> Vec<(String, FnGap)> {
        let mut v: Vec<(String, FnGap)> = self
            .fn_gaps
            .iter()
            .filter(|(_, g)| g.bad() > 0)
            .map(|(k, g)| (k.clone(), *g))
            .collect();
        v.sort_by(|a, b| b.1.bad().cmp(&a.1.bad()).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// Render a human-readable report. `top_n` caps the gap ranking.
    #[must_use]
    pub fn render(&self, top_n: usize) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(s, "== Tier-0 fidelity cut [INTERNAL/T0] ==");
        let _ = writeln!(
            s,
            "T0-set v2 ({} fns, {} uncomputed anti-targets: {}); predicates: cell-level + workbook-level.",
            T0_SET_V1.len(),
            T0_UNSUPPORTED_V1.len(),
            T0_UNSUPPORTED_V1.join("/")
        );
        let _ = writeln!(
            s,
            "NOT a publishable claim: private Enron corpus, methodology unfrozen (project decision 2026-07-12).\n"
        );
        let _ = writeln!(s, "corpus formula cells examined: {}", self.corpus_total);
        let _ = writeln!(
            s,
            "workbooks: {} examined, {} entirely-Tier-0 (spec)\n",
            self.wb_examined, self.wb_qualifying
        );

        let row = |s: &mut String, label: &str, b: &Buckets| {
            let _ = writeln!(
                s,
                "{label}\n  cells={} exact={} ulp={} mismatch={} unsupported={} no_oracle={}\n  strict={:.4}%  lenient={:.4}%",
                b.total,
                b.exact,
                b.ulp_diff,
                b.mismatch,
                b.engine_unsupported,
                b.no_oracle,
                b.strict_pct(),
                b.lenient_pct()
            );
        };
        row(
            &mut s,
            "[cell-level, SPEC set] (primary M1-gate reading)",
            &self.cell_spec,
        );
        row(
            &mut s,
            "[cell-level, SUPPORTED set] (correctness ceiling)",
            &self.cell_supported,
        );
        row(
            &mut s,
            "[workbook-level, SPEC set] (literal 'Tier-0-only workbooks')",
            &self.wb_spec,
        );

        // The M1 "which lever" attribution: split the cell-level SPEC
        // #UNSUPPORTED! mass by construct class vs precedent cascade. This is the
        // corpus attribution probe that gates the big reference-form RFC (a cell
        // may hit several constructs, so these overlap; cascade is exclusive).
        let unsup_total = self.cell_spec.engine_unsupported;
        let _ = writeln!(
            s,
            "\n== UNSUPPORTED attribution (cell-level SPEC, {unsup_total} cells) — the M1 lever probe =="
        );
        let pct = |n: usize| -> f64 {
            if unsup_total == 0 {
                0.0
            } else {
                n as f64 / unsup_total as f64 * 100.0
            }
        };
        let mut cons: Vec<(&&str, &usize)> = self.unsup_constructs.iter().collect();
        cons.sort_by(|a, b| b.1.cmp(a.1));
        for (name, n) in cons {
            let _ = writeln!(
                s,
                "  construct {name:<22} {n:>10} ({:.2}% of unsupported)",
                pct(*n)
            );
        }
        let _ = writeln!(
            s,
            "  {:<32} {:>10} ({:.2}%)  <- no reference-form construct; input cell is itself #UNSUPPORTED!",
            "precedent-cascade (no-construct)",
            self.unsup_cascade,
            pct(self.unsup_cascade)
        );
        let _ = writeln!(
            s,
            "  {:<32} {:>10} ({:.2}%)  <- own formula names TODAY/NOW (volatile ceiling lower bound)",
            "volatile-in-formula",
            self.unsup_volatile_in_formula,
            pct(self.unsup_volatile_in_formula)
        );

        // The M1-gate-relevant split: the SAME attribution over the CLEAN
        // (entirely-Tier-0, no-UDF) workbook-level cut. Here a cascade cannot be
        // UDF poison — it must root in a construct, a volatile, or a chained
        // Tier-0 #UNSUPPORTED!. This decides whether the strict Tier-0 gate is
        // engine-achievable or volatile/construct-ceiling-limited.
        let wb_unsup_total = self.wb_spec.engine_unsupported;
        let _ = writeln!(
            s,
            "\n== UNSUPPORTED attribution (workbook-level CLEAN cut, {wb_unsup_total} cells) — the M1-gate split =="
        );
        let wbpct = |n: usize| -> f64 {
            if wb_unsup_total == 0 {
                0.0
            } else {
                n as f64 / wb_unsup_total as f64 * 100.0
            }
        };
        let mut wbcons: Vec<(&&str, &usize)> = self.wb_unsup_constructs.iter().collect();
        wbcons.sort_by(|a, b| b.1.cmp(a.1));
        for (name, n) in wbcons {
            let _ = writeln!(
                s,
                "  construct {name:<22} {n:>10} ({:.2}% of clean-unsupported)",
                wbpct(*n)
            );
        }
        let _ = writeln!(
            s,
            "  {:<32} {:>10} ({:.2}%)  <- clean cascade: chained Tier-0 #UNSUPPORTED! / volatile (NOT UDF)",
            "precedent-cascade (no-construct)",
            self.wb_unsup_cascade,
            wbpct(self.wb_unsup_cascade)
        );

        let ranking = self.gap_ranking();
        let _ = writeln!(
            s,
            "\n== M1 gap ranking (spec-eligible fns by bad-cell count) — top {} ==",
            top_n
        );
        if ranking.is_empty() {
            let _ = writeln!(
                s,
                "  (no gaps: every spec-eligible cell is Exact/UlpDiff/NoOracle)"
            );
        }
        for (name, g) in ranking.iter().take(top_n) {
            let unreg = if T0_UNSUPPORTED_V1.contains(&name.as_str()) {
                " [UNREGISTERED]"
            } else {
                ""
            };
            let _ = writeln!(
                s,
                "  {name:<12}{unreg}  bad={} (mismatch={} unsupported={}) of {} appearances; exact={}",
                g.bad(),
                g.mismatch,
                g.engine_unsupported,
                g.appearances,
                g.exact
            );
        }

        if !self.sample_fns.is_empty() {
            let mut fns: Vec<&String> = self.sample_fns.iter().collect();
            fns.sort();
            let _ = writeln!(
                s,
                "\n== MISMATCH samples for {:?} (up to {}) — never-guess probe evidence ==",
                fns, self.sample_cap
            );
            if self.mismatch_samples.is_empty() {
                let _ = writeln!(s, "  (none found)");
            }
            for line in &self.mismatch_samples {
                let _ = writeln!(s, "  {line}");
            }
        }

        if !self.unsup_sample_fns.is_empty() {
            let mut fns: Vec<&String> = self.unsup_sample_fns.iter().collect();
            fns.sort();
            let _ = writeln!(
                s,
                "\n== UNSUPPORTED samples for {:?} (up to {}) — which forms drive the cascade ==",
                fns, self.sample_cap
            );
            if self.unsupported_samples.is_empty() {
                let _ = writeln!(s, "  (none found)");
            }
            for line in &self.unsupported_samples {
                let _ = writeln!(s, "  {line}");
            }
        }
        s
    }

    /// A single machine-parseable summary line for status-log capture.
    #[must_use]
    pub fn summary_line(&self) -> String {
        format!(
            "[INTERNAL/T0] T0-set-v2 | cell-spec strict={:.4}% lenient={:.4}% (n={}) | cell-supported strict={:.4}% lenient={:.4}% (n={}) | wb-spec strict={:.4}% lenient={:.4}% (n={}, {}/{} wbs) | corpus_cells={}",
            self.cell_spec.strict_pct(),
            self.cell_spec.lenient_pct(),
            self.cell_spec.total,
            self.cell_supported.strict_pct(),
            self.cell_supported.lenient_pct(),
            self.cell_supported.total,
            self.wb_spec.strict_pct(),
            self.wb_spec.lenient_pct(),
            self.wb_spec.total,
            self.wb_qualifying,
            self.wb_examined,
            self.corpus_total
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> BTreeSet<&'static str> {
        T0_SET_V1.iter().copied().collect()
    }
    fn unsup() -> BTreeSet<&'static str> {
        T0_UNSUPPORTED_V1.iter().copied().collect()
    }

    #[test]
    fn pure_arithmetic_is_supported() {
        assert_eq!(
            classify_formula("=A1+B1*2", &spec(), &unsup()),
            VocabClass::Tier0Supported
        );
    }

    #[test]
    fn all_supported_calls() {
        assert_eq!(
            classify_formula("=SUM(A1:A10)+IF(B1>0,LEFT(C1,2),\"\")", &spec(), &unsup()),
            VocabClass::Tier0Supported
        );
    }

    #[test]
    fn spec_but_uncomputed_call() {
        // NOW is Tier-0 spec but a volatile anti-target (uncomputed) → SpecOnly,
        // not Supported. (OFFSET/INDIRECT were dropped from the uncomputed list
        // in v2 — they are implemented reference-transformers.)
        assert_eq!(
            classify_formula("=NOW()+1", &spec(), &unsup()),
            VocabClass::Tier0SpecOnly
        );
        // OFFSET is now a *supported* reference-transformer.
        assert_eq!(
            classify_formula("=SUM(OFFSET(A1,0,0,10,1))", &spec(), &unsup()),
            VocabClass::Tier0Supported
        );
    }

    #[test]
    fn non_tier0_call() {
        assert_eq!(
            classify_formula("=XLOOKUP(A1,B:B,C:C)", &spec(), &unsup()),
            VocabClass::NonTier0
        );
    }

    #[test]
    fn nested_non_tier0_disqualifies() {
        // A Tier-0 outer call wrapping a non-Tier-0 inner call is NonTier0.
        assert_eq!(
            classify_formula("=IFERROR(TEXTJOIN(\",\",1,A1:A3),0)", &spec(), &unsup()),
            VocabClass::NonTier0
        );
    }

    #[test]
    fn unparseable_is_non_tier0() {
        assert_eq!(
            classify_formula("=SUM(", &spec(), &unsup()),
            VocabClass::NonTier0
        );
    }

    fn constructs(formula: &str) -> BTreeSet<&'static str> {
        let expr = parse(formula).expect("parses");
        let mut out = BTreeSet::new();
        detect_constructs(&expr, &mut out);
        out
    }

    #[test]
    fn detect_constructs_classes() {
        assert!(constructs("=SUM(A:A)").contains("whole-column"));
        assert!(constructs("=SUM(1:1)").contains("whole-row"));
        assert!(constructs("=SUM(Sheet1:Sheet3!A1)").contains("3d-ref"));
        assert!(constructs("=SUM({1,2;3,4})").contains("array-literal"));
        // A plain cell/range formula flags no construct (→ cascade bucket if it
        // still scores unsupported).
        assert!(constructs("=SUM(A1:A10)+B2").is_empty());
    }

    #[test]
    fn strict_lenient_formulas() {
        let b = Buckets {
            total: 100,
            exact: 80,
            ulp_diff: 0,
            mismatch: 5,
            engine_unsupported: 10,
            no_oracle: 5,
        };
        // strict: 80 / (100 - 5) = 84.2105%
        assert!((b.strict_pct() - 84.210_526).abs() < 1e-4);
        // lenient: 80 / (100 - 10 - 5) = 94.1176%
        assert!((b.lenient_pct() - 94.117_647).abs() < 1e-4);
    }
}
