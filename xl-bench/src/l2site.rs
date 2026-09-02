//! L2 refusal-site decomposition — *where* does each shared-expanded runtime
//! refusal actually refuse?
//!
//! # The population this decomposes
//! [`crate::decline`]'s `other_shared_expanded` sub-bucket (the W-B doc's
//! **L2 lane**, `docs/shared-residual-analysis.md`): bodyless
//! `<f t="shared"/>` follow-on cells whose group master **parses** and whose
//! expansion (master translated to the follow-on's position, ECMA-376
//! §18.17.2) succeeded — yet the expanded formula still evaluates to a recalc
//! sentinel (`#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`) with **no statically
//! attributable cause** (no external ref, no unimplemented function, no
//! declined-cascade root). These are *runtime* refusals; the static
//! attributor explicitly parks them, and this module answers the next
//! question: which function or engine seam produced the sentinel?
//!
//! # Method — diagnostics first, sentinel-flow trace second
//! The engine records a [`xl_engine::Diagnostic`] at every *engine-seam*
//! refusal (unknown function, arity, unsupported reference form,
//! scalar-context array/range refusals, spill-engine refusals, …), so:
//!
//! 1. **Own diagnostics win.** If the L2 cell itself carries diagnostics,
//!    its refusal site is read straight from them ([`site_key`] normalizes
//!    each message to a short stable key).
//! 2. **Otherwise trace the sentinel flow.** A sentinel with no diagnostic
//!    at the cell arrived by ordinary error propagation from a precedent, or
//!    was returned by a registered function's *internal* refusal (an
//!    `xl-fn` eval returning `#UNSUPPORTED!` — e.g. an OXP-held coercion or
//!    collation seam — which by design emits no engine diagnostic). The
//!    tracer BFS-walks precedents **whose computed value is itself a recalc
//!    sentinel** (crucially including `NoOracle` cells the declined-only
//!    cascade of [`crate::decline`] cannot see). A reached cell *with*
//!    diagnostics is a root: its normalized site is taken. A reached
//!    sentinel cell with **no** diagnostics and **no** sentinel precedents
//!    is a *silent* root: the refusal happened inside evaluating that cell's
//!    own formula, and the site is inferred from its AST —
//!    `fn_runtime:<NAME>` when exactly one supported callable is named,
//!    `fn_runtime_multi:<A+B+…>` when several are (never guessed apart),
//!    `op_or_coercion_runtime` when only operators could have refused, and
//!    `root_unclassified` otherwise.
//! 3. **Ties are deterministic.** A cell reaching several distinct sites
//!    takes the lexicographically first (`BTreeSet` order) and is counted in
//!    `multi_site_cells` — the same fixed-tiebreak-plus-explicit-count pattern
//!    as [`crate::decline`]'s `CauseSet::pick` / `multi_cause_cells`.
//!
//! Nothing here feeds back into any engine decision or published fidelity
//! number; it is a triage instrument. Reconciliation is by construction: the
//! L2 cell list comes from the *same* [`crate::decline::attribute_cells`]
//! call ([`crate::decline::WorkbookDeclineResult::shared_expanded_cells`]),
//! so `total_l2` equals `other_shared_expanded` exactly, and every cell
//! receives exactly one site (the per-site counts sum to `total_l2`,
//! gate-checked in the CLI).
//!
//! # Provenance
//! - Site messages: `xl-engine`'s diagnostic push sites (`eval.rs`,
//!   `refx.rs`, `lib.rs`); the normalization table in [`site_key`] mirrors
//!   those message templates and falls back to `construct_other` (with the
//!   raw message retained in examples) rather than guessing.
//! - Sentinel predicate: [`xl_value::ErrorKind::is_recalc_sentinel`] — the
//!   same predicate the benchmark funnel layers under `EngineUnsupported`.
//! - Shared-formula translation: [`crate::decline::translate`], the
//!   clean-room mirror of `xl-engine/src/shared.rs` (ECMA-376 §18.17.2).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use xl_ast::{Expr, ExprKind, parse};
use xl_engine::{CellId, DiagnosticKind, Engine};
use xl_io::FormulaKind;
use xl_value::{ErrorKind, SheetId, Value};

use crate::addr::a1_ref;
use crate::decline::{
    BODYLESS_PLACEHOLDER, CellInfo, REF_TRANSFORMERS, SPECIAL_FORMS, SharedMaster, attribute_cells,
    resolve_declined_precedents, translate,
};
use crate::diff::{CellStatus, DiffConfig, classify};
use crate::report::RunError;
use crate::sidecar::{CachedValueSource, SidecarSource};

/// Cap on the number of function names spelled out in a `fn_runtime_multi:`
/// site key before the remainder collapses to `+more` (keeps the key space
/// bounded and the ranking readable; the full formula is in the examples).
const MULTI_FN_NAME_CAP: usize = 4;

// ── site-key normalization ─────────────────────────────────────────────────

/// Classify the rendered reference text of an `unsupported reference: {r}`
/// diagnostic into a coarse reference-form key. Heuristics over
/// `xl-ast`'s `Display` output (sheet prefix `Name!`/`'Q'!`/`A:B!`, then an
/// A1 cell, a bare column (`A`), a bare row (`12`), or R1C1 text).
#[must_use]
fn ref_form_key(rest: &str) -> &'static str {
    // Split at the LAST '!' — the sheet qualifier ends there.
    let (sheet_part, addr) = match rest.rfind('!') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => ("", rest),
    };
    if !sheet_part.is_empty() {
        // A 3-D span renders as `First:Last!` (quoted or not).
        if sheet_part.contains(':') {
            return "ref_3d_span";
        }
    }
    let b = addr.as_bytes();
    if addr.is_empty() {
        return "ref_other";
    }
    // R1C1 text: `R[1]C[-2]`, `R1C1`, `RC[3]` … — requires a digit or a
    // bracket so single column letters `R`/`RC` (valid A1 columns) don't
    // misfire.
    let starts_rc = matches!(b[0], b'R' | b'r' | b'C' | b'c');
    if starts_rc
        && (addr.contains('[')
            || (addr.chars().any(|c| c.is_ascii_digit())
                && addr.to_ascii_uppercase().contains('C')
                && addr.to_ascii_uppercase().contains('R')))
    {
        return "ref_r1c1";
    }
    if b.iter().all(|&c| c.is_ascii_alphabetic() || c == b'$') {
        return "ref_whole_col";
    }
    if b.iter().all(|&c| c.is_ascii_digit() || c == b'$') {
        return "ref_whole_row";
    }
    if !sheet_part.is_empty() {
        // A plain A1 cell behind a sheet qualifier only reaches the
        // unsupported-reference seam when the sheet failed to resolve
        // (unknown name / workbook-global scope).
        return "ref_unresolved_sheet";
    }
    "ref_other"
}

/// Normalize one engine diagnostic `(kind, message)` into a short, stable
/// refusal-site key. Prefix/substring matching against the engine's message
/// templates; anything unrecognized lands in `construct_other` (reported
/// as-is, never folded into a named site — the raw message survives in
/// the examples).
#[must_use]
pub fn site_key(kind: DiagnosticKind, message: &str) -> String {
    match kind {
        DiagnosticKind::ParseError => "parse_error".to_string(),
        DiagnosticKind::CircularReference => "circular_reference".to_string(),
        DiagnosticKind::UnknownFunction => {
            let name = message.rsplit(": ").next().unwrap_or("?").trim();
            format!("unknown_fn:{name}")
        }
        DiagnosticKind::ArityError => {
            let name = message
                .strip_prefix("function ")
                .and_then(|m| m.split(' ').next())
                .unwrap_or("?");
            format!("arity:{name}")
        }
        DiagnosticKind::UnsupportedConstruct => {
            if let Some(rest) = message.strip_prefix("unsupported reference: ") {
                return ref_form_key(rest).to_string();
            }
            if message.starts_with("unsupported defined name: ") {
                return "defined_name".to_string();
            }
            let table: &[(&str, &str)] = &[
                (
                    "multi-cell range in scalar context",
                    "scalar_ctx_multicell_range",
                ),
                (
                    "unresolvable range in scalar context",
                    "scalar_ctx_unresolvable_range",
                ),
                ("reference intersection/union", "union_intersection"),
                (
                    "multi-cell reference (OFFSET/INDIRECT)",
                    "scalar_ctx_refx_range",
                ),
                ("multi-element array literal", "scalar_ctx_array_literal"),
                ("unsupported construct: ", "ast_construct"),
                (
                    "multi-cell spilled-range reference",
                    "scalar_ctx_spilled_range",
                ),
                (
                    "whole-column/row range in an array-context",
                    "array_ctx_whole_colrow",
                ),
                (
                    "consumed range exceeds the array-materialization",
                    "array_ctx_elem_cap",
                ),
                ("2-D range in scalar context", "scalar_ctx_2d_range"),
                ("duplicate LET parameter", "let_duplicate_param"),
                ("duplicate LAMBDA parameter", "lambda_duplicate_param"),
                ("ISOMITTED", "isomitted_unpinned"),
                ("reference-returning call nested", "refx_nesting_depth"),
                ("ANCHORARRAY expects", "refx_anchorarray_arity"),
                ("OFFSET", "refx_offset"),
                ("INDIRECT", "refx_indirect"),
                ("spill planning did not converge", "spill_nonconvergence"),
                ("#SPILL!: dynamic array spill blocked", "spill_blocked"),
                ("#SPILL!: dynamic array at", "spill_blocked"),
                ("#SPILL!", "spill_overflow"),
                ("#VALUE!: a lambda-valued element", "spill_lambda_element"),
                ("shared follow-on", "shared_followon_orphan"),
                ("array follow-on", "array_followon"),
                ("dataTable follow-on", "datatable_followon"),
                ("formula cell with no body", "bodyless_malformed"),
            ];
            for (needle, key) in table {
                if message.contains(needle) {
                    return (*key).to_string();
                }
            }
            "construct_other".to_string()
        }
    }
}

// ── silent-root inference ──────────────────────────────────────────────────

/// Collect every *supported* callable named in `expr` — registry hits,
/// engine special forms (`LET`/`LAMBDA`/`MAP`/…), and the reference
/// transformers — plus whether any operator (unary/binary/postfix, excluding
/// the range operator, which builds a reference rather than computing) is
/// present. `_xlpm.`-prefixed lambda-parameter applications are skipped
/// (they are bindings, not refusal sites). An *unknown* name never appears
/// here: it would have pushed an `UnknownFunction` diagnostic, so the cell
/// would not be a silent root.
fn collect_runtime_candidates(expr: &Expr, names: &mut BTreeSet<String>, has_op: &mut bool) {
    match &expr.kind {
        ExprKind::Call { name, args } => {
            let canon = name.canonical.as_str();
            if !canon.starts_with("_XLPM.")
                && (xl_fn::lookup(canon).is_some()
                    || SPECIAL_FORMS.contains(&canon)
                    || REF_TRANSFORMERS.contains(&canon))
            {
                names.insert(canon.to_string());
            }
            for a in args {
                collect_runtime_candidates(a, names, has_op);
            }
        }
        ExprKind::Unary { expr: inner, .. } | ExprKind::Postfix { expr: inner, .. } => {
            *has_op = true;
            collect_runtime_candidates(inner, names, has_op);
        }
        ExprKind::Binary { op, lhs, rhs } => {
            if !matches!(op, xl_ast::BinaryOp::Range) {
                *has_op = true;
            }
            collect_runtime_candidates(lhs, names, has_op);
            collect_runtime_candidates(rhs, names, has_op);
        }
        ExprKind::Paren(inner) | ExprKind::ImplicitIntersection(inner) => {
            collect_runtime_candidates(inner, names, has_op);
        }
        ExprKind::Array(rows) => {
            for row in rows {
                for e in row {
                    collect_runtime_candidates(e, names, has_op);
                }
            }
        }
        ExprKind::Ref(_)
        | ExprKind::Name(_)
        | ExprKind::Number(_)
        | ExprKind::Text(_)
        | ExprKind::Bool(_)
        | ExprKind::Error(_)
        | ExprKind::Missing
        | ExprKind::Unsupported { .. } => {}
    }
}

/// The site key for a **silent root** — a sentinel-valued cell with no
/// diagnostics and no sentinel precedents, whose refusal therefore happened
/// inside evaluating its own formula without an engine-seam diagnostic
/// (a registered function's internal refusal, or an operator/coercion seam).
#[must_use]
fn silent_root_site(ast: Option<&Expr>) -> String {
    let Some(expr) = ast else {
        return "root_unclassified".to_string();
    };
    let mut names = BTreeSet::new();
    let mut has_op = false;
    collect_runtime_candidates(expr, &mut names, &mut has_op);
    match names.len() {
        0 => {
            if has_op {
                "op_or_coercion_runtime".to_string()
            } else {
                "root_unclassified".to_string()
            }
        }
        1 => format!("fn_runtime:{}", names.iter().next().expect("len==1")),
        _ => {
            let shown: Vec<&str> = names
                .iter()
                .take(MULTI_FN_NAME_CAP)
                .map(String::as_str)
                .collect();
            let suffix = if names.len() > MULTI_FN_NAME_CAP {
                "+more"
            } else {
                ""
            };
            format!("fn_runtime_multi:{}{suffix}", shown.join("+"))
        }
    }
}

// ── the pure attribution core ──────────────────────────────────────────────

/// One L2 cell's attribution result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct L2CellSite {
    pub cell: CellId,
    /// The winning refusal-site key.
    pub site: String,
    /// The site was found on a *precedent* (sentinel-flow trace), not on the
    /// cell itself.
    pub cascade: bool,
    /// The trace reached ≥2 distinct sites; the lexicographically first won.
    pub multi_site: bool,
}

/// Attribute each L2 cell to its refusal site (pure — no I/O, no engine).
///
/// * `l2` — the `other_shared_expanded` cells (from
///   [`crate::decline::WorkbookDeclineResult::shared_expanded_cells`]).
/// * `asts` — effective AST per *sentinel-valued* cell (own body parsed, or
///   the translated master for a bodyless shared follow-on); `None` when no
///   AST could be built.
/// * `diags` — per-cell engine diagnostics, `(kind, message)` pairs.
/// * `sentinel_index` — per-sheet `(row, col)` sets of every formula cell
///   whose computed value is a recalc sentinel (declined *and* `NoOracle`).
/// * `lambda_cells` — formula cells whose computed value is an engine
///   lambda (declined as generic `#UNSUPPORTED!` but not error-propagating).
pub fn attribute_l2_sites(
    l2: &[CellId],
    asts: &BTreeMap<CellId, Option<Expr>>,
    diags: &BTreeMap<CellId, Vec<(DiagnosticKind, String)>>,
    sentinel_index: &BTreeMap<SheetId, BTreeSet<(u32, u32)>>,
    sheet_map: &BTreeMap<String, SheetId>,
    lambda_cells: &BTreeSet<CellId>,
) -> Vec<L2CellSite> {
    let own_sites = |cid: &CellId| -> BTreeSet<String> {
        diags
            .get(cid)
            .map(|v| v.iter().map(|(k, m)| site_key(*k, m)).collect())
            .unwrap_or_default()
    };
    let precs = |cid: &CellId| -> Vec<CellId> {
        asts.get(cid)
            .and_then(|a| a.as_ref())
            .map(|e| resolve_declined_precedents(e, cid.sheet, sheet_map, sentinel_index))
            .unwrap_or_default()
    };

    let mut out = Vec::with_capacity(l2.len());
    for &cid in l2 {
        if lambda_cells.contains(&cid) {
            out.push(L2CellSite {
                cell: cid,
                site: "lambda_valued_cell".to_string(),
                cascade: false,
                multi_site: false,
            });
            continue;
        }
        let own = own_sites(&cid);
        if !own.is_empty() {
            let multi = own.len() > 1;
            out.push(L2CellSite {
                cell: cid,
                site: own.into_iter().next().expect("non-empty"),
                cascade: false,
                multi_site: multi,
            });
            continue;
        }
        // No own diagnostics: BFS the sentinel flow.
        let start_precs = precs(&cid);
        if start_precs.is_empty() {
            // The cell is itself the silent root.
            out.push(L2CellSite {
                cell: cid,
                site: silent_root_site(asts.get(&cid).and_then(|a| a.as_ref())),
                cascade: false,
                multi_site: false,
            });
            continue;
        }
        let mut sites: BTreeSet<String> = BTreeSet::new();
        let mut seen: BTreeSet<CellId> = BTreeSet::new();
        let mut stack = start_precs;
        while let Some(c) = stack.pop() {
            if !seen.insert(c) {
                continue;
            }
            let s = own_sites(&c);
            if !s.is_empty() {
                sites.extend(s);
                continue; // diagnostic-bearing root; stop here
            }
            let p = precs(&c);
            if p.is_empty() {
                sites.insert(silent_root_site(asts.get(&c).and_then(|a| a.as_ref())));
                continue;
            }
            for q in p {
                if !seen.contains(&q) {
                    stack.push(q);
                }
            }
        }
        if sites.is_empty() {
            // Every reachable sentinel precedent cycled back without a root.
            out.push(L2CellSite {
                cell: cid,
                site: "propagation_unresolved".to_string(),
                cascade: true,
                multi_site: false,
            });
            continue;
        }
        let multi = sites.len() > 1;
        out.push(L2CellSite {
            cell: cid,
            site: sites.into_iter().next().expect("non-empty"),
            cascade: true,
            multi_site: multi,
        });
    }
    out
}

// ── per-workbook pipeline ──────────────────────────────────────────────────

/// One example L2 cell for the report: `sheet!A1` plus the *expanded*
/// formula the engine actually evaluated (the translated master, rendered).
#[derive(Clone, Debug)]
pub struct L2Example {
    pub sheet: String,
    pub a1: String,
    pub formula: String,
}

/// One workbook's L2 refusal-site decomposition.
#[derive(Clone, Debug, Default)]
pub struct WorkbookL2Sites {
    /// The L2 population (== this workbook's `other_shared_expanded`).
    pub total_l2: usize,
    /// This workbook's full declined total (context for reconciliation).
    pub total_declined: usize,
    /// Cells per refusal-site key.
    pub site_cells: BTreeMap<String, usize>,
    /// Site found on the cell itself.
    pub direct_cells: usize,
    /// Site found by the sentinel-flow trace on a precedent.
    pub cascade_cells: usize,
    /// The trace reached ≥2 distinct sites (tiebreak applied).
    pub multi_site_cells: usize,
    /// Up to a handful of examples per site (first in `CellId` order).
    pub examples: BTreeMap<String, Vec<L2Example>>,
}

/// How many examples each workbook retains per site key.
const EXAMPLES_PER_SITE_PER_WB: usize = 5;

/// Load, recalc, and decompose one workbook's L2 cells.
///
/// Mirrors [`crate::decline::attribute_workbook`]'s pipeline exactly (same
/// snapshot, same `CachedValueSource` oracle, same funnel, same clean-room
/// master map), then reuses [`crate::decline::attribute_cells`] itself so
/// the L2 population is *identical by construction* to what
/// `decline-attribution` reports at the same HEAD.
pub fn decompose_workbook(path: &Path) -> Result<WorkbookL2Sites, RunError> {
    let workbook = xl_io::open(path).map_err(RunError::Load)?;

    let defined_names_uc: BTreeSet<String> = workbook
        .defined_names
        .iter()
        .map(|d| d.name.to_ascii_uppercase())
        .collect();

    struct Snap {
        sheet: SheetId,
        name: String,
        row: u32,
        col: u32,
        formula: String,
        bodyless_kind: Option<FormulaKind>,
        shared_index: Option<u32>,
    }
    let mut sheet_map: BTreeMap<String, SheetId> = BTreeMap::new();
    let mut snaps: Vec<Snap> = Vec::new();
    let mut cached_map: BTreeMap<(String, u32, u32), Value> = BTreeMap::new();
    let mut shared_masters: BTreeMap<(SheetId, u32), SharedMaster> = BTreeMap::new();
    for (idx, sheet) in workbook.sheets.iter().enumerate() {
        let sid = SheetId(idx as u32);
        sheet_map.insert(sheet.name.to_ascii_lowercase(), sid);
        for (&(row, col), cell) in &sheet.cells {
            let Some(raw) = &cell.formula else { continue };
            let is_shared = raw.kind == FormulaKind::Shared;
            let (formula, bodyless_kind, shared_index) = match &raw.text {
                Some(text) => {
                    if is_shared && let Some(si) = raw.shared_index {
                        shared_masters.insert(
                            (sid, si),
                            SharedMaster {
                                row,
                                col,
                                expr: parse(text).ok(),
                            },
                        );
                    }
                    (format!("={text}"), None, None)
                }
                None => (
                    BODYLESS_PLACEHOLDER.to_string(),
                    Some(raw.kind),
                    is_shared.then_some(raw.shared_index).flatten(),
                ),
            };
            cached_map.insert((sheet.name.clone(), row, col), cell.value.clone());
            snaps.push(Snap {
                sheet: sid,
                name: sheet.name.clone(),
                row,
                col,
                formula,
                bodyless_kind,
                shared_index,
            });
        }
    }
    let oracle = CachedValueSource::new(cached_map);
    let sheet_names: Vec<String> = workbook.sheets.iter().map(|s| s.name.clone()).collect();

    let mut engine = Engine::load(workbook);
    engine.recalc();

    // Funnel classification (identical to decline.rs) + the sentinel/lambda
    // indices the tracer needs.
    let mut cells: Vec<CellInfo> = Vec::with_capacity(snaps.len());
    let mut sentinel_index: BTreeMap<SheetId, BTreeSet<(u32, u32)>> = BTreeMap::new();
    let mut lambda_cells: BTreeSet<CellId> = BTreeSet::new();
    for s in &snaps {
        let computed = engine
            .value(s.sheet, s.row, s.col)
            .cloned()
            .unwrap_or(Value::Blank);
        match &computed {
            Value::Error(k) if k.is_recalc_sentinel() => {
                sentinel_index
                    .entry(s.sheet)
                    .or_default()
                    .insert((s.row, s.col));
            }
            Value::Lambda(_) => {
                lambda_cells.insert(CellId::new(s.sheet, s.row, s.col));
            }
            _ => {}
        }
        let oracle_record = oracle.lookup(&s.name, s.row, s.col);
        let status = classify(&computed, &oracle_record, DiffConfig::default());
        let declined_kind =
            matches!(status, CellStatus::EngineUnsupported).then(|| match &computed {
                Value::Error(k) if k.is_recalc_sentinel() => *k,
                _ => ErrorKind::Unsupported,
            });
        cells.push(CellInfo {
            sheet: s.sheet,
            row: s.row,
            col: s.col,
            formula: s.formula.clone(),
            declined_kind,
            bodyless_kind: s.bodyless_kind,
            shared_index: s.shared_index,
        });
    }

    let decl = attribute_cells(&cells, &sheet_map, &defined_names_uc, &shared_masters);
    let l2: &[CellId] = &decl.shared_expanded_cells;

    // Effective ASTs for every sentinel-valued cell (the only cells the
    // tracer can visit) — own body parsed, or the translated master for a
    // bodyless shared follow-on.
    let mut asts: BTreeMap<CellId, Option<Expr>> = BTreeMap::new();
    for s in &snaps {
        let cid = CellId::new(s.sheet, s.row, s.col);
        let is_sentinel = sentinel_index
            .get(&s.sheet)
            .is_some_and(|set| set.contains(&(s.row, s.col)));
        if !is_sentinel && !lambda_cells.contains(&cid) {
            continue;
        }
        let ast = if s.formula == BODYLESS_PLACEHOLDER {
            if s.bodyless_kind == Some(FormulaKind::Shared) {
                s.shared_index
                    .and_then(|si| shared_masters.get(&(s.sheet, si)))
                    .and_then(|m| {
                        m.expr.as_ref().map(|e| {
                            translate(
                                e,
                                i64::from(s.row) - i64::from(m.row),
                                i64::from(s.col) - i64::from(m.col),
                            )
                        })
                    })
            } else {
                None
            }
        } else {
            parse(&s.formula).ok()
        };
        asts.insert(cid, ast);
    }

    // Per-cell diagnostics, converted to `(kind, message)` pairs.
    let mut diags: BTreeMap<CellId, Vec<(DiagnosticKind, String)>> = BTreeMap::new();
    for d in engine.diagnostics() {
        diags
            .entry(d.cell)
            .or_default()
            .push((d.kind, d.message.clone()));
    }

    let attributed = attribute_l2_sites(
        l2,
        &asts,
        &diags,
        &sentinel_index,
        &sheet_map,
        &lambda_cells,
    );

    let mut result = WorkbookL2Sites {
        total_l2: decl.other_shared_expanded,
        total_declined: decl.total_declined,
        ..Default::default()
    };
    for a in &attributed {
        *result.site_cells.entry(a.site.clone()).or_default() += 1;
        if a.cascade {
            result.cascade_cells += 1;
        } else {
            result.direct_cells += 1;
        }
        if a.multi_site {
            result.multi_site_cells += 1;
        }
        let ex = result.examples.entry(a.site.clone()).or_default();
        if ex.len() < EXAMPLES_PER_SITE_PER_WB {
            let sheet = sheet_names
                .get(a.cell.sheet.0 as usize)
                .cloned()
                .unwrap_or_else(|| "?".to_string());
            let formula = asts
                .get(&a.cell)
                .and_then(|o| o.as_ref())
                .map(|e| format!("={e}"))
                .unwrap_or_else(|| "<no AST>".to_string());
            ex.push(L2Example {
                sheet,
                a1: a1_ref(a.cell.row, a.cell.col),
                formula,
            });
        }
    }
    debug_assert_eq!(
        result.site_cells.values().sum::<usize>(),
        result.total_l2,
        "L2 site counts must partition the L2 cells"
    );
    Ok(result)
}

// ── corpus tally ───────────────────────────────────────────────────────────

/// Corpus-wide L2 site accumulator. Fold one [`WorkbookL2Sites`] per
/// workbook, then [`render`](L2SiteTally::render).
#[derive(Clone, Debug, Default)]
pub struct L2SiteTally {
    pub workbooks: usize,
    pub load_failures: usize,
    pub total_l2: usize,
    pub total_declined: usize,
    pub site_cells: BTreeMap<String, usize>,
    pub direct_cells: usize,
    pub cascade_cells: usize,
    pub multi_site_cells: usize,
    /// `(workbook file name, example)` per site, capped at
    /// [`EXAMPLES_PER_SITE`](Self::EXAMPLES_PER_SITE) (first encountered in
    /// corpus walk order — deterministic for a sorted walk).
    pub examples: BTreeMap<String, Vec<(String, L2Example)>>,
}

impl L2SiteTally {
    /// Global cap on retained examples per site key.
    pub const EXAMPLES_PER_SITE: usize = 5;

    /// Fold one workbook's result into the running totals.
    pub fn fold(&mut self, wb_name: &str, r: &WorkbookL2Sites) {
        self.workbooks += 1;
        self.total_l2 += r.total_l2;
        self.total_declined += r.total_declined;
        for (site, n) in &r.site_cells {
            *self.site_cells.entry(site.clone()).or_default() += n;
        }
        self.direct_cells += r.direct_cells;
        self.cascade_cells += r.cascade_cells;
        self.multi_site_cells += r.multi_site_cells;
        for (site, exs) in &r.examples {
            let slot = self.examples.entry(site.clone()).or_default();
            for ex in exs {
                if slot.len() >= Self::EXAMPLES_PER_SITE {
                    break;
                }
                slot.push((wb_name.to_string(), ex.clone()));
            }
        }
    }

    /// Record a workbook that failed to load (excluded from all counts).
    pub fn note_load_failure(&mut self) {
        self.load_failures += 1;
    }

    /// Sites ranked by cell count desc, then key asc (deterministic).
    #[must_use]
    pub fn ranked_sites(&self) -> Vec<(String, usize)> {
        let mut v: Vec<(String, usize)> = self
            .site_cells
            .iter()
            .map(|(k, n)| (k.clone(), *n))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// Render the human-readable report. `top_n` caps the ranked table;
    /// `example_sites` caps how many top sites get example blocks;
    /// `max_text` truncates each example formula (0 = full).
    #[must_use]
    pub fn render(&self, top_n: usize, example_sites: usize, max_text: usize) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(s, "== L2 refusal-site decomposition ==");
        let _ = writeln!(
            s,
            "Population: other_shared_expanded — would-expand shared follow-ons that"
        );
        let _ = writeln!(
            s,
            "refuse at runtime with no static cause. Each cell → exactly one site key"
        );
        let _ = writeln!(
            s,
            "(own diagnostics first; else sentinel-flow trace to the refusing root).\n"
        );
        let _ = writeln!(
            s,
            "workbooks: {} attributed, {} load failure(s) (excluded)",
            self.workbooks, self.load_failures
        );
        let _ = writeln!(s, "total declined cells (context): {}", self.total_declined);
        let _ = writeln!(
            s,
            "total L2 cells (other_shared_expanded): {}",
            self.total_l2
        );
        let _ = writeln!(
            s,
            "site found on: own cell {} / precedent trace {}; multi-site (tiebroken): {}\n",
            self.direct_cells, self.cascade_cells, self.multi_site_cells
        );

        let pct = |n: usize| -> f64 {
            if self.total_l2 == 0 {
                0.0
            } else {
                n as f64 / self.total_l2 as f64 * 100.0
            }
        };
        let ranked = self.ranked_sites();
        let _ = writeln!(s, "== ranked refusal sites (top {top_n}) ==");
        for (site, n) in ranked.iter().take(top_n) {
            let _ = writeln!(s, "  {site:<44} {n:>9}  ({:6.2}%)", pct(*n));
        }
        let sum: usize = self.site_cells.values().sum();
        let _ = writeln!(
            s,
            "  {:<44} {:>9}  ({})",
            "----- sum (all sites)",
            sum,
            if sum == self.total_l2 {
                "OK: equals L2 total"
            } else {
                "MISMATCH vs L2 total!"
            }
        );

        let _ = writeln!(s, "\n== examples (top {example_sites} sites) ==");
        for (site, _) in ranked.iter().take(example_sites) {
            let _ = writeln!(s, "  [{site}]");
            for (wb, ex) in self.examples.get(site).into_iter().flatten() {
                let mut f = ex.formula.clone();
                if max_text > 0 && f.len() > max_text {
                    let mut cut = max_text;
                    while !f.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    f.truncate(cut);
                    f.push('…');
                }
                let _ = writeln!(s, "    {wb} {}!{}  {f}", ex.sheet, ex.a1);
            }
        }
        s
    }

    /// A single machine-parseable summary line for status-log capture.
    #[must_use]
    pub fn summary_line(&self) -> String {
        let top: Vec<String> = self
            .ranked_sites()
            .into_iter()
            .take(8)
            .map(|(k, n)| format!("{k}={n}"))
            .collect();
        format!(
            "[L2-SITES] total_l2={} direct={} cascade={} multi_site={} top: {} (wbs={}, load_fail={})",
            self.total_l2,
            self.direct_cells,
            self.cascade_cells,
            self.multi_site_cells,
            top.join(" "),
            self.workbooks,
            self.load_failures,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── site_key normalization ─────────────────────────────────────────────

    #[test]
    fn site_key_unknown_fn_extracts_name() {
        assert_eq!(
            site_key(
                DiagnosticKind::UnknownFunction,
                "unsupported function: NORMDIST"
            ),
            "unknown_fn:NORMDIST"
        );
    }

    #[test]
    fn site_key_arity_extracts_name() {
        assert_eq!(
            site_key(
                DiagnosticKind::ArityError,
                "function SUM called with 300 argument(s), outside its arity"
            ),
            "arity:SUM"
        );
    }

    #[test]
    fn site_key_ref_forms() {
        let k = |m: &str| site_key(DiagnosticKind::UnsupportedConstruct, m);
        assert_eq!(k("unsupported reference: Sheet1:Sheet3!A1"), "ref_3d_span");
        assert_eq!(k("unsupported reference: R[1]C[-2]"), "ref_r1c1");
        assert_eq!(k("unsupported reference: A"), "ref_whole_col");
        // Column "RC" (valid A1 letters) must NOT read as R1C1.
        assert_eq!(k("unsupported reference: RC"), "ref_whole_col");
        assert_eq!(k("unsupported reference: $12"), "ref_whole_row");
        assert_eq!(
            k("unsupported reference: Missing!B2"),
            "ref_unresolved_sheet"
        );
    }

    #[test]
    fn site_key_construct_table_and_fallback() {
        let k = |m: &str| site_key(DiagnosticKind::UnsupportedConstruct, m);
        assert_eq!(
            k("2-D range in scalar context (implicit-intersection axis unprobed) is unsupported"),
            "scalar_ctx_2d_range"
        );
        assert_eq!(
            k(
                "whole-column/row range in an array-context aggregator argument needs array evaluation — unsupported in v1 (RFC-0011 / M2 lane 6)"
            ),
            "array_ctx_whole_colrow"
        );
        assert_eq!(k("unsupported defined name: MyName"), "defined_name");
        assert_eq!(k("something entirely new"), "construct_other");
    }

    #[test]
    fn site_key_parse_and_circular() {
        assert_eq!(
            site_key(DiagnosticKind::ParseError, "formula parse error: x"),
            "parse_error"
        );
        assert_eq!(
            site_key(DiagnosticKind::CircularReference, "cycle"),
            "circular_reference"
        );
    }

    // ── silent-root inference ──────────────────────────────────────────────

    fn ast(text: &str) -> Expr {
        parse(text).expect("test formula parses")
    }

    #[test]
    fn silent_root_single_fn() {
        let e = ast("=VLOOKUP(A1,B:C,2,0)");
        assert_eq!(silent_root_site(Some(&e)), "fn_runtime:VLOOKUP");
    }

    #[test]
    fn silent_root_multi_fn_sorted() {
        let e = ast("=IFERROR(VLOOKUP(A1,B:C,2,0),0)");
        assert_eq!(
            silent_root_site(Some(&e)),
            "fn_runtime_multi:IFERROR+VLOOKUP"
        );
    }

    #[test]
    fn silent_root_ops_only() {
        let e = ast("=A1<\"ß\"");
        assert_eq!(silent_root_site(Some(&e)), "op_or_coercion_runtime");
    }

    #[test]
    fn silent_root_no_ast() {
        assert_eq!(silent_root_site(None), "root_unclassified");
    }

    // ── attribution core ───────────────────────────────────────────────────

    fn sheet_map() -> BTreeMap<String, SheetId> {
        let mut m = BTreeMap::new();
        m.insert("sheet1".to_string(), SheetId(0));
        m
    }

    #[test]
    fn own_diagnostic_wins_direct() {
        let cid = CellId::new(SheetId(0), 5, 0);
        let mut diags = BTreeMap::new();
        diags.insert(
            cid,
            vec![(
                DiagnosticKind::UnsupportedConstruct,
                "2-D range in scalar context (implicit-intersection axis unprobed) is unsupported"
                    .to_string(),
            )],
        );
        let r = attribute_l2_sites(
            &[cid],
            &BTreeMap::new(),
            &diags,
            &BTreeMap::new(),
            &sheet_map(),
            &BTreeSet::new(),
        );
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].site, "scalar_ctx_2d_range");
        assert!(!r[0].cascade);
    }

    #[test]
    fn silent_cell_with_no_sentinel_precedents_is_its_own_root() {
        let cid = CellId::new(SheetId(0), 5, 0);
        let mut asts = BTreeMap::new();
        asts.insert(cid, Some(ast("=SUMIF(A1:A3,\"x\",B1:B3)")));
        let r = attribute_l2_sites(
            &[cid],
            &asts,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &sheet_map(),
            &BTreeSet::new(),
        );
        assert_eq!(r[0].site, "fn_runtime:SUMIF");
        assert!(!r[0].cascade);
    }

    #[test]
    fn sentinel_flow_traces_to_diagnostic_root() {
        // L2 cell at B6 (=A6+1); A6 is sentinel-valued with an UnknownFunction
        // diagnostic. The trace must land on unknown_fn, flagged cascade —
        // exactly the case the declined-only cascade cannot see when A6 has
        // no oracle value.
        let l2 = CellId::new(SheetId(0), 5, 1);
        let root = CellId::new(SheetId(0), 5, 0);
        let mut asts = BTreeMap::new();
        asts.insert(l2, Some(ast("=A6+1")));
        asts.insert(root, Some(ast("=NORMDIST(1,2,3,TRUE)")));
        let mut diags = BTreeMap::new();
        diags.insert(
            root,
            vec![(
                DiagnosticKind::UnknownFunction,
                "unsupported function: NORMDIST".to_string(),
            )],
        );
        let mut sentinel_index: BTreeMap<SheetId, BTreeSet<(u32, u32)>> = BTreeMap::new();
        sentinel_index.entry(SheetId(0)).or_default().insert((5, 0));
        sentinel_index.entry(SheetId(0)).or_default().insert((5, 1));
        let r = attribute_l2_sites(
            &[l2],
            &asts,
            &diags,
            &sentinel_index,
            &sheet_map(),
            &BTreeSet::new(),
        );
        assert_eq!(r[0].site, "unknown_fn:NORMDIST");
        assert!(r[0].cascade);
        assert!(!r[0].multi_site);
    }

    #[test]
    fn sentinel_flow_traces_to_silent_root() {
        // L2 cell → A6, whose own formula calls a registered function and has
        // no diagnostics and no sentinel precedents: fn_runtime root.
        let l2 = CellId::new(SheetId(0), 5, 1);
        let root = CellId::new(SheetId(0), 5, 0);
        let mut asts = BTreeMap::new();
        asts.insert(l2, Some(ast("=A6+1")));
        asts.insert(root, Some(ast("=COUNTIF(C1:C9,\"ä*\")")));
        let mut sentinel_index: BTreeMap<SheetId, BTreeSet<(u32, u32)>> = BTreeMap::new();
        sentinel_index.entry(SheetId(0)).or_default().insert((5, 0));
        sentinel_index.entry(SheetId(0)).or_default().insert((5, 1));
        let r = attribute_l2_sites(
            &[l2],
            &asts,
            &BTreeMap::new(),
            &sentinel_index,
            &sheet_map(),
            &BTreeSet::new(),
        );
        assert_eq!(r[0].site, "fn_runtime:COUNTIF");
        assert!(r[0].cascade);
    }

    #[test]
    fn cycle_without_root_is_unresolved() {
        // A6 ↔ B6, both sentinel, no diagnostics: the trace must terminate
        // and report propagation_unresolved, never loop.
        let a = CellId::new(SheetId(0), 5, 0);
        let b = CellId::new(SheetId(0), 5, 1);
        let mut asts = BTreeMap::new();
        asts.insert(a, Some(ast("=B6")));
        asts.insert(b, Some(ast("=A6")));
        let mut sentinel_index: BTreeMap<SheetId, BTreeSet<(u32, u32)>> = BTreeMap::new();
        sentinel_index.entry(SheetId(0)).or_default().insert((5, 0));
        sentinel_index.entry(SheetId(0)).or_default().insert((5, 1));
        let r = attribute_l2_sites(
            &[b],
            &asts,
            &BTreeMap::new(),
            &sentinel_index,
            &sheet_map(),
            &BTreeSet::new(),
        );
        assert_eq!(r[0].site, "propagation_unresolved");
    }

    #[test]
    fn lambda_valued_cell_short_circuits() {
        let cid = CellId::new(SheetId(0), 0, 0);
        let mut lambda_cells = BTreeSet::new();
        lambda_cells.insert(cid);
        let r = attribute_l2_sites(
            &[cid],
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &sheet_map(),
            &lambda_cells,
        );
        assert_eq!(r[0].site, "lambda_valued_cell");
    }

    #[test]
    fn multi_root_tiebreak_is_deterministic_and_counted() {
        // L2 cell reads two roots with different sites; the lexicographically
        // first key wins and multi_site is flagged.
        let l2 = CellId::new(SheetId(0), 5, 2);
        let r1 = CellId::new(SheetId(0), 5, 0);
        let r2 = CellId::new(SheetId(0), 5, 1);
        let mut asts = BTreeMap::new();
        asts.insert(l2, Some(ast("=A6+B6")));
        asts.insert(r1, Some(ast("=SUMIF(A1:A2,1,B1:B2)")));
        asts.insert(r2, Some(ast("=VLOOKUP(1,E:F,2,0)")));
        let mut sentinel_index: BTreeMap<SheetId, BTreeSet<(u32, u32)>> = BTreeMap::new();
        for c in [0u32, 1, 2] {
            sentinel_index.entry(SheetId(0)).or_default().insert((5, c));
        }
        let r = attribute_l2_sites(
            &[l2],
            &asts,
            &BTreeMap::new(),
            &sentinel_index,
            &sheet_map(),
            &BTreeSet::new(),
        );
        assert_eq!(r[0].site, "fn_runtime:SUMIF"); // "fn_runtime:SUMIF" < "fn_runtime:VLOOKUP"
        assert!(r[0].multi_site);
        assert!(r[0].cascade);
    }

    #[test]
    fn tally_render_partition_gate() {
        let mut wb = WorkbookL2Sites {
            total_l2: 3,
            total_declined: 10,
            ..Default::default()
        };
        wb.site_cells.insert("fn_runtime:SUMIF".to_string(), 2);
        wb.site_cells.insert("scalar_ctx_2d_range".to_string(), 1);
        wb.direct_cells = 1;
        wb.cascade_cells = 2;
        let mut tally = L2SiteTally::default();
        tally.fold("book.xlsx", &wb);
        let out = tally.render(10, 10, 0);
        assert!(out.contains("OK: equals L2 total"), "{out}");
        assert!(out.contains("fn_runtime:SUMIF"));
        let line = tally.summary_line();
        assert!(line.starts_with("[L2-SITES] total_l2=3"), "{line}");
    }
}
