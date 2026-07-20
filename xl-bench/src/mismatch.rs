//! `recalc mismatch-mine`: corpus-wide decomposition of the genuine-fidelity-
//! failure set — every [`crate::diff::CellStatus::Mismatch`] cell — by
//! **function vocabulary**, **type transition** (expected-kind → actual-kind),
//! and a small set of mutually-exclusive **named patterns**.
//!
//! # Why a dedicated cut (vs. `tier0 --dump-mismatch`)
//! [`crate::tier0`]'s sampler collects a *capped* set of example mismatch cells
//! for a **named** function list, over the **Tier-0 spec slice only**. That is a
//! chicken-and-egg tool: you must already know which function to look at. The
//! `mismatch-mine` cut is the inverse — it walks the **whole** corpus, needs no
//! function named up front, and produces the *ranking itself* (which function /
//! pattern owns the most mismatch cells), so it can drive a data-first fix
//! backlog. It also streams a per-cell forensic dump (`--dump`) so an ambiguous
//! bucket can be inspected without re-running the corpus.
//!
//! # Scoring tolerance
//! The classification runs against whatever [`DiffConfig`] the caller passes.
//! For the headline mismatch set (the 48,632 cells that survive at Excel's own
//! storage resolution) run with `--tol=15sig`; the default bit-exact floor is a
//! superset (adds sub-15-sig IEEE noise) and is useful only as a drift
//! diagnostic. The tool never changes a tolerance itself — that is human-gated
//! (`TOLERANCES.md`).
//!
//! # Never-guess candor
//! This is a *measurement*, not an oracle. It classifies the disagreement
//! Recalc *already* produced against Excel's cached `<v>`; it does not decide
//! who is right. A bucket's size is a lead for a farm probe, not a verdict.

use std::collections::BTreeMap;
use std::path::Path;

use xl_ast::{Expr, ExprKind, parse};
use xl_value::Value;

use crate::corpus::discover_workbooks;
use crate::diff::{CellStatus, DiffConfig};
use crate::report::{CellRecord, run_workbook};

/// The placeholder [`CellRecord::formula`] carries for a shared/array follow-on
/// cell with no formula body of its own (mirrors `report::run_workbook`). Such
/// a cell's function vocabulary is unknowable from the cell alone.
const NO_BODY_PLACEHOLDER: &str = "<shared/array follow-on, no formula body>";

/// The lookup family, for the `lookup-*` named patterns (a `#N/A` on either
/// side of a mismatch in one of these is the classic match/no-match divergence).
const LOOKUP_FNS: &[&str] = &[
    "VLOOKUP", "HLOOKUP", "LOOKUP", "MATCH", "INDEX", "XLOOKUP", "XMATCH",
];

/// A single formula cell's mismatch pattern — mutually exclusive, assigned in
/// the priority order of [`classify_pattern`]. The `&'static str` label is the
/// stable key used in every aggregate and in the `--dump` stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pattern {
    /// A lookup-family cell where the **oracle** is `#N/A` but Recalc returned a
    /// value (Recalc matched where Excel did not — over-match / wrong default).
    LookupExpectedNa,
    /// A lookup-family cell where **Recalc** returned `#N/A` but the oracle has
    /// a value (Recalc missed a match Excel found).
    LookupActualNa,
    /// Both sides are genuine (non-sentinel) Excel errors of a *different* kind
    /// (e.g. oracle `#DIV/0!`, Recalc `#VALUE!`).
    ErrorKindMismatch,
    /// Oracle is a genuine Excel error; Recalc returned a non-error value
    /// (Recalc computed where Excel errored).
    ExpectedErrorGotValue,
    /// Oracle is a value; Recalc returned a genuine Excel error (Recalc errored
    /// where Excel computed) — excludes the lookup `#N/A` case above.
    ExpectedValueGotError,
    /// Both numbers, disagreeing beyond tolerance, relative error ≥ 1e-2.
    NumericGross,
    /// Both numbers, relative error in `[1e-4, 1e-2)`.
    NumericMid,
    /// Both numbers, relative error `< 1e-4` (accumulation / precision-as-
    /// displayed candidates — small but above 15-sig storage resolution).
    NumericFine,
    /// Both text, equal ignoring ASCII case only (a capitalization bug).
    TextCaseOnly,
    /// Both text, equal after trimming ASCII whitespace only.
    TextWhitespaceOnly,
    /// Both text, and both parse as numbers (number-formatting / `TEXT()` gap).
    TextNumericFormat,
    /// Both text, differing in some other way.
    TextOther,
    /// Oracle number vs Recalc text, or the reverse (a coercion divergence).
    NumberTextCoercion,
    /// Exactly one side is `Blank` (blank-vs-value / empty-string handling).
    BlankInvolved,
    /// Both booleans (necessarily differing).
    BoolDiff,
    /// Anything else — Array/Ref/Lambda on either side, or a residual type pair.
    TypeOther,
}

impl Pattern {
    /// The stable snake-case label used as the aggregation key and dump column.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Pattern::LookupExpectedNa => "lookup_expected_na",
            Pattern::LookupActualNa => "lookup_actual_na",
            Pattern::ErrorKindMismatch => "error_kind_mismatch",
            Pattern::ExpectedErrorGotValue => "expected_error_got_value",
            Pattern::ExpectedValueGotError => "expected_value_got_error",
            Pattern::NumericGross => "numeric_gross",
            Pattern::NumericMid => "numeric_mid",
            Pattern::NumericFine => "numeric_fine",
            Pattern::TextCaseOnly => "text_case_only",
            Pattern::TextWhitespaceOnly => "text_whitespace_only",
            Pattern::TextNumericFormat => "text_numeric_format",
            Pattern::TextOther => "text_other",
            Pattern::NumberTextCoercion => "number_text_coercion",
            Pattern::BlankInvolved => "blank_involved",
            Pattern::BoolDiff => "bool_diff",
            Pattern::TypeOther => "type_other",
        }
    }

    /// Every pattern, for stable report ordering / zero-fill.
    #[must_use]
    pub fn all() -> [Pattern; 16] {
        [
            Pattern::LookupExpectedNa,
            Pattern::LookupActualNa,
            Pattern::ErrorKindMismatch,
            Pattern::ExpectedErrorGotValue,
            Pattern::ExpectedValueGotError,
            Pattern::NumericGross,
            Pattern::NumericMid,
            Pattern::NumericFine,
            Pattern::TextCaseOnly,
            Pattern::TextWhitespaceOnly,
            Pattern::TextNumericFormat,
            Pattern::TextOther,
            Pattern::NumberTextCoercion,
            Pattern::BlankInvolved,
            Pattern::BoolDiff,
            Pattern::TypeOther,
        ]
    }
}

/// The coarse "kind" tag of a value, used to build the expected→actual
/// transition cross-tab. Genuine Excel errors report their exact spelling
/// (`#N/A`, `#VALUE!`, …) so an error-kind divergence is visible in the tab.
#[must_use]
pub fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Number(_) => "num",
        Value::Text(_) => "text",
        Value::Bool(_) => "bool",
        Value::Error(e) => e.as_str(),
        Value::Blank => "blank",
        Value::Array(_) => "array",
        Value::Ref(_) => "ref",
        Value::Lambda(_) => "lambda",
    }
}

/// A short, dump-safe repr of a value (tabs/newlines stripped, text truncated).
#[must_use]
pub fn value_repr(v: &Value, max_text: usize) -> String {
    let raw = match v {
        Value::Number(n) => format!("{n}"),
        Value::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).to_string(),
        Value::Error(e) => e.as_str().to_string(),
        Value::Blank => "<blank>".to_string(),
        Value::Text(t) => {
            let s = t.as_str();
            let shown: String = s.chars().take(max_text).collect();
            let ell = if s.chars().count() > max_text {
                "…"
            } else {
                ""
            };
            format!("\"{shown}{ell}\"")
        }
        Value::Array(_) => "<array>".to_string(),
        Value::Ref(_) => "<ref>".to_string(),
        Value::Lambda(_) => "<lambda>".to_string(),
    };
    sanitize(&raw)
}

/// Strip tab/newline/CR so a value or formula is safe as one TSV field.
fn sanitize(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// Recursively collect every function-call name (canonical, uppercased) in
/// `expr`. Mirrors `tier0::collect_calls` but is local so this module stands
/// alone.
fn collect_calls(expr: &Expr, calls: &mut Vec<String>) {
    match &expr.kind {
        ExprKind::Number(_)
        | ExprKind::Text(_)
        | ExprKind::Bool(_)
        | ExprKind::Error(_)
        | ExprKind::Missing
        | ExprKind::Ref(_)
        | ExprKind::Name(_)
        | ExprKind::Unsupported { .. } => {}
        ExprKind::Unary { expr, .. }
        | ExprKind::Postfix { expr, .. }
        | ExprKind::Paren(expr)
        | ExprKind::ImplicitIntersection(expr) => collect_calls(expr, calls),
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_calls(lhs, calls);
            collect_calls(rhs, calls);
        }
        ExprKind::Call { name, args } => {
            calls.push(name.canonical.to_ascii_uppercase());
            for a in args {
                collect_calls(a, calls);
            }
        }
        ExprKind::Array(rows) => {
            for row in rows {
                for e in row {
                    collect_calls(e, calls);
                }
            }
        }
    }
}

/// Extract a cell's function vocabulary: the **sorted, de-duplicated** set of
/// function names it calls. Returns a distinguishable single-element vocabulary
/// for the two un-parseable cases so they rank as their own bucket rather than
/// silently vanishing:
/// - `<no-body>` — a shared/array follow-on cell with no formula body.
/// - `<parse-fail>` — a formula body the parser rejects.
/// - `<no-call>` — parses, but is pure arithmetic/refs with no function call.
#[must_use]
pub fn vocabulary(formula: &str) -> Vec<String> {
    if formula == NO_BODY_PLACEHOLDER {
        return vec!["<no-body>".to_string()];
    }
    let Ok(expr) = parse(formula) else {
        return vec!["<parse-fail>".to_string()];
    };
    let mut calls = Vec::new();
    collect_calls(&expr, &mut calls);
    if calls.is_empty() {
        return vec!["<no-call>".to_string()];
    }
    calls.sort();
    calls.dedup();
    calls
}

/// `true` if the vocabulary names any lookup-family function.
fn is_lookup(vocab: &[String]) -> bool {
    vocab.iter().any(|c| LOOKUP_FNS.contains(&c.as_str()))
}

/// Relative error between two finite numbers, guarding a zero denominator.
fn rel_error(a: f64, b: f64) -> f64 {
    let denom = a.abs().max(b.abs());
    if denom == 0.0 {
        // Both zero would be Exact upstream; a genuine 0-vs-x here means x≠0,
        // so treat as gross.
        return f64::INFINITY;
    }
    (a - b).abs() / denom
}

/// `true` if both strings parse as f64 (a numeric-formatting text divergence).
fn both_numeric_text(a: &str, b: &str) -> bool {
    a.trim().parse::<f64>().is_ok() && b.trim().parse::<f64>().is_ok()
}

/// Classify one mismatch's `(expected, actual)` pair into exactly one
/// [`Pattern`]. `vocab` is the cell's function vocabulary (for the lookup
/// patterns). See [`Pattern`] for the meaning of each bucket; the order here is
/// the tie-break priority.
#[must_use]
pub fn classify_pattern(expected: &Value, actual: &Value, vocab: &[String]) -> Pattern {
    let exp_na = matches!(expected, Value::Error(e) if e.as_str() == "#N/A");
    let act_na = matches!(actual, Value::Error(e) if e.as_str() == "#N/A");

    // Lookup #N/A divergences first — the highest-signal named cluster.
    if is_lookup(vocab) {
        if exp_na && !act_na {
            return Pattern::LookupExpectedNa;
        }
        if act_na && !exp_na {
            return Pattern::LookupActualNa;
        }
    }

    match (expected, actual) {
        (Value::Error(_), Value::Error(_)) => Pattern::ErrorKindMismatch,
        (Value::Error(_), _) => Pattern::ExpectedErrorGotValue,
        (_, Value::Error(_)) => Pattern::ExpectedValueGotError,
        (Value::Number(a), Value::Number(b)) => {
            let r = rel_error(*a, *b);
            if r >= 1e-2 {
                Pattern::NumericGross
            } else if r >= 1e-4 {
                Pattern::NumericMid
            } else {
                Pattern::NumericFine
            }
        }
        (Value::Text(a), Value::Text(b)) => {
            let (a, b) = (a.as_str(), b.as_str());
            if a.eq_ignore_ascii_case(b) {
                Pattern::TextCaseOnly
            } else if a.trim() == b.trim() {
                Pattern::TextWhitespaceOnly
            } else if both_numeric_text(a, b) {
                Pattern::TextNumericFormat
            } else {
                Pattern::TextOther
            }
        }
        (Value::Number(_), Value::Text(_)) | (Value::Text(_), Value::Number(_)) => {
            Pattern::NumberTextCoercion
        }
        (Value::Bool(_), Value::Bool(_)) => Pattern::BoolDiff,
        (Value::Blank, _) | (_, Value::Blank) => Pattern::BlankInvolved,
        _ => Pattern::TypeOther,
    }
}

/// The running corpus-wide mismatch decomposition.
#[derive(Clone, Debug, Default)]
pub struct MismatchTally {
    /// Formula cells examined across the corpus (every `<f>` cell).
    pub total_cells: usize,
    /// Cells classified [`CellStatus::Mismatch`] (the fidelity-failure set).
    pub total_mismatch: usize,
    /// Workbooks that loaded and were scored.
    pub books_ok: usize,
    /// Workbooks that failed to load (excluded from all counts).
    pub load_failures: usize,
    /// Mismatch cells per [`Pattern`] (label-keyed).
    pub by_pattern: BTreeMap<&'static str, usize>,
    /// Mismatch cells per expected→actual kind transition (e.g. `num→#N/A`).
    pub by_transition: BTreeMap<String, usize>,
    /// Mismatch cells each function is implicated in (vocabulary membership; a
    /// cell calling N functions increments N entries).
    pub by_function: BTreeMap<String, usize>,
    /// Mismatch cells per full function-vocabulary signature (the sorted vocab
    /// joined by `+`) — surfaces dominant idioms (e.g. the array-eval
    /// `IF+ISBLANK+NOT+SUM` shape) that per-function membership blurs.
    pub by_vocab_sig: BTreeMap<String, usize>,
    /// Cross-tab: `(function, pattern-label)` → count, for the per-function
    /// pattern breakdown of the top functions.
    pub by_fn_pattern: BTreeMap<(String, &'static str), usize>,
    /// Up to [`Self::sample_cap`] representative dump lines per pattern.
    pub samples: BTreeMap<&'static str, Vec<String>>,
    /// Cap on samples retained per pattern.
    pub sample_cap: usize,
    /// Text truncation width for sampled/dumped values.
    pub max_text: usize,
}

impl MismatchTally {
    /// A tally configured for a run: `sample_cap` examples per pattern, values
    /// truncated to `max_text` chars.
    #[must_use]
    pub fn new(sample_cap: usize, max_text: usize) -> MismatchTally {
        MismatchTally {
            sample_cap,
            max_text,
            ..Default::default()
        }
    }

    /// Fold one workbook's per-cell records into the tally. `dump` (if `Some`)
    /// receives one TSV line per mismatch cell — the full forensic stream.
    /// Returns an IO error only if a dump write fails.
    pub fn fold_workbook<W: std::io::Write>(
        &mut self,
        records: &[CellRecord],
        wb_path: &str,
        dump: &mut Option<W>,
    ) -> std::io::Result<()> {
        for rec in records {
            self.total_cells += 1;
            let CellStatus::Mismatch { expected, actual } = &rec.status else {
                continue;
            };
            self.total_mismatch += 1;

            let vocab = vocabulary(&rec.formula);
            let pattern = classify_pattern(expected, actual, &vocab);
            let label = pattern.label();
            let exp_kind = value_kind(expected);
            let act_kind = value_kind(actual);

            *self.by_pattern.entry(label).or_default() += 1;
            *self
                .by_transition
                .entry(format!("{exp_kind}→{act_kind}"))
                .or_default() += 1;
            for f in &vocab {
                *self.by_function.entry(f.clone()).or_default() += 1;
                *self.by_fn_pattern.entry((f.clone(), label)).or_default() += 1;
            }
            *self.by_vocab_sig.entry(vocab.join("+")).or_default() += 1;

            let exp_repr = value_repr(expected, self.max_text);
            let act_repr = value_repr(actual, self.max_text);
            let line = format!(
                "{wb}\t{sheet}\t{cell}\t{label}\t{exp_kind}\t{act_kind}\t{formula}\t{exp}\t{act}",
                wb = wb_path,
                sheet = sanitize(&rec.sheet),
                cell = rec.cell_ref,
                formula = sanitize(&rec.formula),
                exp = exp_repr,
                act = act_repr,
            );

            let bucket = self.samples.entry(label).or_default();
            if bucket.len() < self.sample_cap {
                bucket.push(line.clone());
            }
            if let Some(w) = dump.as_mut() {
                writeln!(w, "{line}")?;
            }
        }
        Ok(())
    }

    /// The pattern ranking, descending by count.
    #[must_use]
    pub fn pattern_ranking(&self) -> Vec<(&'static str, usize)> {
        let mut v: Vec<(&'static str, usize)> =
            self.by_pattern.iter().map(|(k, n)| (*k, *n)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        v
    }

    /// The function ranking, descending by implicated-mismatch count.
    #[must_use]
    pub fn function_ranking(&self) -> Vec<(String, usize)> {
        let mut v: Vec<(String, usize)> = self
            .by_function
            .iter()
            .map(|(k, n)| (k.clone(), *n))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// The expected→actual transition ranking, descending.
    #[must_use]
    pub fn transition_ranking(&self) -> Vec<(String, usize)> {
        let mut v: Vec<(String, usize)> = self
            .by_transition
            .iter()
            .map(|(k, n)| (k.clone(), *n))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// The vocabulary-signature ranking, descending.
    #[must_use]
    pub fn vocab_sig_ranking(&self) -> Vec<(String, usize)> {
        let mut v: Vec<(String, usize)> = self
            .by_vocab_sig
            .iter()
            .map(|(k, n)| (k.clone(), *n))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// Per-pattern breakdown for one function (descending).
    #[must_use]
    fn fn_pattern_breakdown(&self, func: &str) -> Vec<(&'static str, usize)> {
        let mut v: Vec<(&'static str, usize)> = Pattern::all()
            .iter()
            .filter_map(|p| {
                self.by_fn_pattern
                    .get(&(func.to_string(), p.label()))
                    .map(|n| (p.label(), *n))
            })
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        v
    }

    /// Render the human-readable decomposition report. `top_n` caps each
    /// ranking; the per-function pattern breakdown covers the top `fn_detail`
    /// functions.
    #[must_use]
    pub fn render(&self, top_n: usize, fn_detail: usize) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let pct = |n: usize| -> f64 {
            if self.total_mismatch == 0 {
                0.0
            } else {
                n as f64 / self.total_mismatch as f64 * 100.0
            }
        };

        let _ = writeln!(s, "== mismatch decomposition ==");
        let _ = writeln!(
            s,
            "workbooks: {} scored, {} load failures",
            self.books_ok, self.load_failures
        );
        let _ = writeln!(
            s,
            "cells: {} formula cells examined, {} mismatch ({:.4}%)\n",
            self.total_cells,
            self.total_mismatch,
            if self.total_cells == 0 {
                0.0
            } else {
                self.total_mismatch as f64 / self.total_cells as f64 * 100.0
            }
        );

        let _ = writeln!(s, "== by pattern (mutually exclusive; sums to total) ==");
        for (label, n) in self.pattern_ranking() {
            let _ = writeln!(s, "  {label:<26} {n:>8}  ({:.2}%)", pct(n));
        }

        let _ = writeln!(
            s,
            "\n== by expected→actual kind transition — top {top_n} =="
        );
        for (t, n) in self.transition_ranking().into_iter().take(top_n) {
            let _ = writeln!(s, "  {t:<22} {n:>8}  ({:.2}%)", pct(n));
        }

        let _ = writeln!(
            s,
            "\n== by function (vocabulary membership; overlaps) — top {top_n} =="
        );
        for (f, n) in self.function_ranking().into_iter().take(top_n) {
            let _ = writeln!(s, "  {f:<20} {n:>8}  ({:.2}% of mismatch)", pct(n));
        }

        let _ = writeln!(
            s,
            "\n== by full function-vocabulary signature (dominant idioms) — top {top_n} =="
        );
        for (sig, n) in self.vocab_sig_ranking().into_iter().take(top_n) {
            let _ = writeln!(s, "  {n:>8}  ({:.2}%)  {sig}", pct(n));
        }

        let _ = writeln!(
            s,
            "\n== per-function pattern breakdown — top {fn_detail} functions =="
        );
        for (f, total) in self.function_ranking().into_iter().take(fn_detail) {
            let parts: Vec<String> = self
                .fn_pattern_breakdown(&f)
                .into_iter()
                .map(|(p, n)| format!("{p}={n}"))
                .collect();
            let _ = writeln!(s, "  {f:<20} ({total}): {}", parts.join("  "));
        }

        let _ = writeln!(
            s,
            "\n== representative samples per pattern (up to {}) ==",
            self.sample_cap
        );
        for p in Pattern::all() {
            let label = p.label();
            let Some(lines) = self.samples.get(label) else {
                continue;
            };
            if lines.is_empty() {
                continue;
            }
            let _ = writeln!(s, "-- {label} --");
            for line in lines {
                // Sample columns: drop the leading wb/sheet/cell/label for a
                // tighter view; keep formula/exp/act (last three tab fields).
                let cols: Vec<&str> = line.split('\t').collect();
                if cols.len() >= 9 {
                    let _ = writeln!(
                        s,
                        "   {}!{}  {}  =>  actual={}  expected={}",
                        cols[1], cols[2], cols[6], cols[8], cols[7]
                    );
                } else {
                    let _ = writeln!(s, "   {line}");
                }
            }
        }
        s
    }

    /// A single machine-parseable summary line for status-log capture.
    #[must_use]
    pub fn summary_line(&self) -> String {
        let top_pattern = self
            .pattern_ranking()
            .first()
            .map(|(l, n)| format!("{l}={n}"))
            .unwrap_or_else(|| "none".to_string());
        format!(
            "[MISMATCH-MINE] total_mismatch={} of {} cells ({} wbs, {} load_fail) | top_pattern {}",
            self.total_mismatch, self.total_cells, self.books_ok, self.load_failures, top_pattern
        )
    }
}

/// Walk every workbook under `dir`, recalc + diff at `cfg`, and fold every
/// mismatch cell into a [`MismatchTally`]. `dump` (if given) receives the full
/// per-cell TSV stream. `progress` is called after each workbook with
/// `(done, total)`.
pub fn mine_dir<W: std::io::Write>(
    dir: &Path,
    cfg: DiffConfig,
    sample_cap: usize,
    max_text: usize,
    dump: &mut Option<W>,
    mut progress: impl FnMut(usize, usize, &str),
) -> MismatchTally {
    let books = discover_workbooks(dir);
    let total = books.len();
    let mut tally = MismatchTally::new(sample_cap, max_text);
    for (idx, path) in books.iter().enumerate() {
        let rel = path.display().to_string();
        match run_workbook(path, cfg) {
            Ok(report) => {
                tally.books_ok += 1;
                // Best-effort dump: an IO error aborts the dump but not the run.
                let _ = tally.fold_workbook(&report.cells, &rel, dump);
            }
            Err(_) => tally.load_failures += 1,
        }
        progress(idx + 1, total, &rel);
    }
    tally
}

#[cfg(test)]
mod tests {
    use super::*;
    use xl_value::ErrorKind;

    fn num(x: f64) -> Value {
        Value::number(x)
    }
    fn txt(s: &str) -> Value {
        Value::text(s)
    }
    fn err(k: ErrorKind) -> Value {
        Value::Error(k)
    }

    #[test]
    fn vocabulary_extracts_and_sorts() {
        assert_eq!(
            vocabulary("=SUM(IF(A1>0,B1,C1))"),
            vec!["IF".to_string(), "SUM".to_string()]
        );
        assert_eq!(vocabulary("=A1+B1"), vec!["<no-call>".to_string()]);
        assert_eq!(vocabulary("=SUM("), vec!["<parse-fail>".to_string()]);
        assert_eq!(
            vocabulary(NO_BODY_PLACEHOLDER),
            vec!["<no-body>".to_string()]
        );
    }

    #[test]
    fn lookup_na_patterns() {
        let vocab = vocabulary("=VLOOKUP(A1,B:C,2,0)");
        // Oracle #N/A, Recalc value → over-match.
        assert_eq!(
            classify_pattern(&err(ErrorKind::Na), &num(0.0), &vocab),
            Pattern::LookupExpectedNa
        );
        // Recalc #N/A, oracle value → missed match.
        assert_eq!(
            classify_pattern(&num(5.0), &err(ErrorKind::Na), &vocab),
            Pattern::LookupActualNa
        );
        // A non-lookup formula with the same shape is NOT a lookup pattern.
        let plain = vocabulary("=A1");
        assert_eq!(
            classify_pattern(&err(ErrorKind::Na), &num(0.0), &plain),
            Pattern::ExpectedErrorGotValue
        );
    }

    #[test]
    fn error_patterns() {
        assert_eq!(
            classify_pattern(&err(ErrorKind::Div0), &err(ErrorKind::Value), &[]),
            Pattern::ErrorKindMismatch
        );
        assert_eq!(
            classify_pattern(&err(ErrorKind::Value), &num(1.0), &[]),
            Pattern::ExpectedErrorGotValue
        );
        assert_eq!(
            classify_pattern(&num(1.0), &err(ErrorKind::Value), &[]),
            Pattern::ExpectedValueGotError
        );
    }

    #[test]
    fn numeric_buckets_by_relative_error() {
        assert_eq!(
            classify_pattern(&num(100.0), &num(50.0), &[]),
            Pattern::NumericGross
        );
        assert_eq!(
            classify_pattern(&num(1.0), &num(1.0005), &[]),
            Pattern::NumericMid
        );
        assert_eq!(
            classify_pattern(&num(1.0), &num(1.000001), &[]),
            Pattern::NumericFine
        );
    }

    #[test]
    fn text_buckets() {
        assert_eq!(
            classify_pattern(&txt("Total"), &txt("TOTAL"), &[]),
            Pattern::TextCaseOnly
        );
        assert_eq!(
            classify_pattern(&txt("hi "), &txt("hi"), &[]),
            Pattern::TextWhitespaceOnly
        );
        assert_eq!(
            classify_pattern(&txt("1.50"), &txt("1.5"), &[]),
            Pattern::TextNumericFormat
        );
        assert_eq!(
            classify_pattern(&txt("cat"), &txt("dog"), &[]),
            Pattern::TextOther
        );
    }

    #[test]
    fn coercion_and_blank_and_bool() {
        assert_eq!(
            classify_pattern(&num(1.0), &txt("1"), &[]),
            Pattern::NumberTextCoercion
        );
        assert_eq!(
            classify_pattern(&Value::Blank, &num(0.0), &[]),
            Pattern::BlankInvolved
        );
        assert_eq!(
            classify_pattern(&Value::bool(true), &Value::bool(false), &[]),
            Pattern::BoolDiff
        );
    }

    #[test]
    fn value_kind_reports_error_spelling() {
        assert_eq!(value_kind(&num(1.0)), "num");
        assert_eq!(value_kind(&err(ErrorKind::Na)), "#N/A");
        assert_eq!(value_kind(&Value::Blank), "blank");
    }

    #[test]
    fn fold_counts_only_mismatch_and_streams_dump() {
        let records = vec![
            CellRecord {
                sheet: "S".into(),
                cell_ref: "A1".into(),
                row: 0,
                col: 0,
                formula: "=VLOOKUP(A1,B:C,2,0)".into(),
                status: CellStatus::Mismatch {
                    expected: err(ErrorKind::Na),
                    actual: num(0.0),
                },
            },
            CellRecord {
                sheet: "S".into(),
                cell_ref: "A2".into(),
                row: 1,
                col: 0,
                formula: "=1+1".into(),
                status: CellStatus::Exact,
            },
        ];
        let mut tally = MismatchTally::new(10, 40);
        let mut dump: Option<Vec<u8>> = Some(Vec::new());
        tally.fold_workbook(&records, "wb.xlsx", &mut dump).unwrap();
        assert_eq!(tally.total_cells, 2);
        assert_eq!(tally.total_mismatch, 1);
        assert_eq!(tally.by_pattern.get("lookup_expected_na"), Some(&1));
        assert_eq!(tally.by_function.get("VLOOKUP"), Some(&1));
        // Exactly one dump line (the mismatch), 9 tab-separated columns.
        let dump = String::from_utf8(dump.unwrap()).unwrap();
        assert_eq!(dump.lines().count(), 1);
        assert_eq!(dump.trim_end().split('\t').count(), 9);
    }
}
