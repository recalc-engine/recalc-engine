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

use xl_ast::ParseErrorKind;
use xl_io::FormulaKind;

use crate::decline::has_external_bracket;
use crate::report::RunError;

/// Coarse syntactic **feature bucket** for one unparseable master formula —
/// the W-B triage axis (*why* does this master fail to parse?). Derived
/// deterministically from the [`ParseErrorKind`] plus a raw-text scan of the
/// master's formula (see [`classify_master`]); purely diagnostic, never fed
/// back into any engine decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FeatureBucket {
    /// The lexer rejected a **non-ASCII character** (`UnexpectedChar` outside
    /// ASCII) — the OXP-211 defined-name family (`º`/`í`, …). After the
    /// OXP-211 letter widening (`58f0b86`) this fires only for non-letter
    /// non-ASCII chars still awaiting the deferred COM fill-in probe.
    NonAsciiChar,
    /// The raw text carries an **external workbook index** `[n]` (the stored
    /// form of a cross-workbook reference, `[1]Sheet1!A1`). Not a grammar gap:
    /// the referenced workbook is not shipped, so the correct outcome is a
    /// refusal either way.
    ExternalRef,
    /// A **structured/table reference** shape (`Table1[Col]`, `[[#Data],…]`,
    /// `[@Col]`, or an unterminated bracket group).
    StructuredRef,
    /// An **array literal** `{…}` the parser rejected (non-constant element or
    /// ragged rows per ECMA-376 §18.17.2).
    ArrayLiteral,
    /// **R1C1-style** reference text (`R[1]C[1]`, `RC[2]`, …) inside a part
    /// stored in A1 mode — an authoring/converter artifact.
    R1C1Artifact,
    /// Anything else — reported under its [`ParseErrorKind`] string, never
    /// silently folded into a named bucket.
    OtherParse,
}

impl FeatureBucket {
    /// Short, stable machine-readable tag.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            FeatureBucket::NonAsciiChar => "non_ascii_char",
            FeatureBucket::ExternalRef => "external_ref",
            FeatureBucket::StructuredRef => "structured_ref",
            FeatureBucket::ArrayLiteral => "array_literal",
            FeatureBucket::R1C1Artifact => "r1c1_artifact",
            FeatureBucket::OtherParse => "other_parse",
        }
    }
}

/// Identifier-character predicate shared by the raw-text scans.
#[must_use]
fn is_ident_byte(p: u8) -> bool {
    p.is_ascii_alphanumeric() || p == b'_' || p == b'.'
}

/// Whether the bytes ending at `end` (exclusive) form a standalone R1C1 axis
/// prefix — a lone `R`/`C` (`R[1]`, `C[-2]`) or the relative pair `RC`
/// (`RC[3]`) — with no identifier character before it (so `VARC[…]` or
/// `Name.R[…]` do not qualify).
#[must_use]
fn is_r1c1_axis_prefix(b: &[u8], end: usize) -> bool {
    if end == 0 {
        return false;
    }
    let last = b[end - 1];
    if matches!(last, b'R' | b'r') {
        return end < 2 || !is_ident_byte(b[end - 2]);
    }
    if matches!(last, b'C' | b'c') {
        // Lone `C[…]`, or the `RC[…]` pair.
        if end < 2 || !is_ident_byte(b[end - 2]) {
            return true;
        }
        if matches!(b[end - 2], b'R' | b'r') {
            return end < 3 || !is_ident_byte(b[end - 3]);
        }
    }
    false
}

/// Whether `text` contains a structured-reference marker: `[#`, `[[`, `[@`,
/// or a `[` immediately preceded by an identifier character (`Table1[Col]`,
/// `Name[2024]`). Skips double-quoted string literals, mirroring
/// [`has_external_bracket`]'s string handling. An R1C1 offset (`R[1]`,
/// `C[-2]`) also puts an ident char before `[`; a lone `R`/`C` axis letter is
/// excluded so those shapes stay with [`has_r1c1_artifact`].
#[must_use]
fn has_structured_ref_marker(text: &str) -> bool {
    let b = text.as_bytes();
    let mut in_str = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'"' {
            in_str = !in_str;
            i += 1;
            continue;
        }
        if !in_str && c == b'[' {
            if i + 1 < b.len() && matches!(b[i + 1], b'#' | b'[' | b'@') {
                return true;
            }
            if i > 0 && is_ident_byte(b[i - 1]) && !is_r1c1_axis_prefix(b, i) {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Whether `text` contains an R1C1-style reference shape outside string
/// literals: an `R` or `C` axis letter (not part of a longer identifier)
/// immediately followed by `[` with a signed-integer offset (`R[1]`, `C[-2]`).
#[must_use]
fn has_r1c1_artifact(text: &str) -> bool {
    let b = text.as_bytes();
    let mut in_str = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'"' {
            in_str = !in_str;
            i += 1;
            continue;
        }
        if !in_str && c == b'[' && is_r1c1_axis_prefix(b, i) {
            let mut j = i + 1;
            if j < b.len() && b[j] == b'-' {
                j += 1;
            }
            let d0 = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > d0 && j < b.len() && b[j] == b']' {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Classify one unparseable master into its [`FeatureBucket`].
///
/// Deterministic precedence (first match wins), sharpest signal first:
/// 1. a non-ASCII `UnexpectedChar` is unambiguous lexer evidence;
/// 2. an array-literal `ParseErrorKind` is unambiguous parser evidence;
/// 3. an R1C1 offset shape (`R[1]`/`C[-2]`) beats the broader bracket scans
///    (its `[signed-int]` shape is sharper than "some bracket");
/// 4. an external `[n]` workbook index;
/// 5. structured-ref bracket shapes;
/// 6. everything else stays [`FeatureBucket::OtherParse`] under its
///    `ParseErrorKind` — never guessed into a named bucket.
#[must_use]
pub fn classify_master(text: &str, kind: &ParseErrorKind) -> FeatureBucket {
    if let ParseErrorKind::UnexpectedChar(c) = kind
        && !c.is_ascii()
    {
        return FeatureBucket::NonAsciiChar;
    }
    if matches!(
        kind,
        ParseErrorKind::NonConstantInArray | ParseErrorKind::RaggedArray
    ) {
        return FeatureBucket::ArrayLiteral;
    }
    if has_r1c1_artifact(text) {
        return FeatureBucket::R1C1Artifact;
    }
    if has_external_bracket(text) {
        return FeatureBucket::ExternalRef;
    }
    if matches!(kind, ParseErrorKind::UnterminatedBracket) || has_structured_ref_marker(text) {
        return FeatureBucket::StructuredRef;
    }
    FeatureBucket::OtherParse
}

/// A master cell's text and its cached parse outcome (`None` = parsed OK, `Some`
/// = the `ParseErrorKind`'s display string). Parsing once per master keeps the
/// per-workbook cost linear in distinct masters, not in follow-ons.
struct MasterInfo {
    text: String,
    /// `None` if the body parses; else the parse-error *kind* string (span
    /// dropped, so the same grammatical failure deduplicates cleanly) plus the
    /// coarse [`FeatureBucket`].
    parse_err: Option<(String, FeatureBucket)>,
}

/// One workbook's record for a single unparseable master text.
#[derive(Clone, Debug)]
pub struct MasterFailure {
    /// The `ParseErrorKind` display string.
    pub parse_error: String,
    /// The coarse feature bucket ([`classify_master`]).
    pub bucket: FeatureBucket,
    /// Follow-on cells this master blocks within the workbook.
    pub followons: usize,
}

/// One deduplicated unparseable-master record, in the corpus-wide ranking.
#[derive(Clone, Debug)]
pub struct FailingMaster {
    /// The master's exact formula text (no leading `=`), as stored in `<f>`.
    pub text: String,
    /// The `ParseErrorKind` display string (the failure class).
    pub parse_error: String,
    /// The coarse feature bucket ([`classify_master`]).
    pub bucket: FeatureBucket,
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
    /// Per unparseable-master **text** → its [`MasterFailure`] record within
    /// this one workbook. Folded into the corpus tally with distinct-workbook
    /// counting.
    pub fails: BTreeMap<String, MasterFailure>,
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
            let parse_err = xl_ast::parse(text)
                .err()
                .map(|e| (e.kind.to_string(), classify_master(text, &e.kind)));
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
                    Some((err, bucket)) => {
                        out.parse_fail_followons += 1;
                        let entry =
                            out.fails
                                .entry(mi.text.clone())
                                .or_insert_with(|| MasterFailure {
                                    parse_error: err.clone(),
                                    bucket: *bucket,
                                    followons: 0,
                                });
                        entry.followons += 1;
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
    /// text -> (parse-error string, bucket, total follow-on cells, distinct
    /// workbooks).
    fails: BTreeMap<String, (String, FeatureBucket, usize, usize)>,
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
        for (text, mf) in &r.fails {
            let e = self
                .fails
                .entry(text.clone())
                .or_insert_with(|| (mf.parse_error.clone(), mf.bucket, 0, 0));
            e.2 += mf.followons;
            e.3 += 1;
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
            .map(|(text, (err, bucket, cnt, wbs))| FailingMaster {
                text: text.clone(),
                parse_error: err.clone(),
                bucket: *bucket,
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
        for (err, _bucket, cnt, _wbs) in self.fails.values() {
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

        // Feature-bucket rollup: the W-B coarse-cause ranking.
        let mut by_bucket: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();
        for (_err, bucket, cnt, _wbs) in self.fails.values() {
            let e = by_bucket.entry(bucket.tag()).or_insert((0, 0));
            e.0 += *cnt; // follow-on cells
            e.1 += 1; // distinct master texts
        }
        let mut bucket_rank: Vec<(&'static str, usize, usize)> =
            by_bucket.into_iter().map(|(k, (c, m))| (k, c, m)).collect();
        bucket_rank.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let _ = writeln!(
            s,
            "\n== feature-bucket rollup (follow-on cells / distinct masters) =="
        );
        if bucket_rank.is_empty() {
            let _ = writeln!(s, "  (none)");
        }
        for (tag, cells, masters) in &bucket_rank {
            let _ = writeln!(s, "  {cells:>10} cells / {masters:>6} masters   {tag}");
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
                "  [{:>7} cells / {:>5} wbs] [{}] {} :: ={}",
                fm.followon_cells,
                fm.workbooks,
                fm.bucket.tag(),
                fm.parse_error,
                shown
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
            sheets_index: 0,
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
        let mf = r.fails.get("SUM(").expect("master text keyed");
        assert_eq!(mf.followons, 2);
        assert_eq!(mf.bucket, FeatureBucket::OtherParse);
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

    /// Parse `text`, expecting failure, and classify the failing master.
    fn bucket_of(text: &str) -> FeatureBucket {
        let err = xl_ast::parse(text).expect_err("fixture must fail to parse");
        classify_master(text, &err.kind)
    }

    #[test]
    fn classify_non_ascii_unexpected_char() {
        // `§` (U+00A7, category So — not a letter) is still rejected after the
        // OXP-211 letter widening.
        assert_eq!(bucket_of("Total§x+1"), FeatureBucket::NonAsciiChar);
    }

    #[test]
    fn classify_external_ref() {
        // Unquoted `[1]…` external index: lexes to a bracket atom, then trips
        // a trailing-token parse error — the classic unparsed external form.
        assert_eq!(bucket_of("[1]Sheet1!A1+B2"), FeatureBucket::ExternalRef);
    }

    #[test]
    fn classify_structured_ref() {
        assert_eq!(
            bucket_of("SUM(Table1[[#Data],[Amount"),
            FeatureBucket::StructuredRef
        );
    }

    #[test]
    fn classify_array_literal() {
        // Ragged rows → RaggedArray.
        assert_eq!(bucket_of("{1,2;3}"), FeatureBucket::ArrayLiteral);
    }

    #[test]
    fn classify_r1c1_artifact() {
        // `RC[-1]` in an A1-mode part; the `[signed-int]` offset shape is the
        // R1C1 signal even though the parse error itself is generic.
        assert_eq!(bucket_of("SUM(RC[-1]:RC[3]"), FeatureBucket::R1C1Artifact);
    }

    #[test]
    fn classify_other_parse() {
        assert_eq!(bucket_of("SUM("), FeatureBucket::OtherParse);
    }

    #[test]
    fn classify_string_literal_brackets_do_not_mislead() {
        // `[1]` inside a string literal must not classify as external.
        assert_eq!(bucket_of("\"[1]x\"&SUM("), FeatureBucket::OtherParse);
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
