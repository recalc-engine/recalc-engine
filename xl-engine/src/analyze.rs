//! Static analysis of a parsed formula: reference resolution, precedent
//! extraction (for `xl-graph`), defined-name resolution, and volatility
//! detection.
//!
//! # Provenance
//! Reference geometry (A1 addressing, whole-column/row ranges, sheet-qualified
//! and 3-D prefixes) follows the AST shapes `xl-ast` produces (ECMA-376 §18.17).
//! Defined-name scope resolution follows ECMA-376 §18.2.6
//! (`definedName@localSheetId`): a name with `localSheetId="N"` is scoped to
//! the sheet at 0-based position N of the `<sheets>` collection (name strings
//! are unique per scope, not per workbook); resolved from that sheet it
//! shadows a workbook-global name of the same string, while every other sheet
//! sees the global. See `docs/l2-refusal-decomposition.md` lane L2-D (the
//! `rate`/`X` corpus shape). What this module deliberately does **not**
//! resolve — and returns as `#UNSUPPORTED!` / no-precedent rather than
//! guessing — is documented per function: R1C1 (the engine parses in A1
//! mode), 3-D spans, per-endpoint sheet-qualified ranges, non-simple
//! defined-name bodies, and **sheet-qualified name references**
//! (`Sheet2!name`): §18.2.6 pins storage scoping only, not which scope a
//! qualified reference selects — that confirmation probe rides the next farm
//! batch (lane doc, review condition 3, 2026-08-25).
//!
//! # Recursion is bounded
//! Every walk here recurses over a *single formula's* AST, which `xl-ast` caps
//! at [`xl_ast::MAX_NESTING_DEPTH`] (200) — far below a stack-overflow risk.
//! This is unlike `xl-graph`'s workbook-wide traversals (which must be
//! iterative); a per-formula tree walk is safe to write recursively.

use std::collections::{BTreeMap, BTreeSet};

use xl_ast::{BinaryOp, Expr, ExprKind, NameRef, RefKind, Reference, SheetRef};
use xl_fn::is_volatile;
use xl_graph::{CellId, Precedent, SheetRange};
use xl_value::{RectRange, SheetId};

// `RectRange` is used both for reference geometry (above) and, via the
// `WbIndex::spills` field below, for the M2 lane-4 spill anchor→region map.

/// Largest 0-based row index (Excel's 1,048,576 rows).
pub(crate) const MAX_ROW0: u32 = 1_048_575;
/// Largest 0-based column index (Excel's 16,384 columns, `XFD`).
pub(crate) const MAX_COL0: u32 = 16_383;

/// The scope a defined name applies to, translated into engine sheet ids at
/// load (ECMA-376 §18.2.6 `definedName@localSheetId`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NameScope {
    /// No `localSheetId`: visible from every sheet unless shadowed by a
    /// same-named [`NameScope::Local`] on the resolving sheet.
    Global,
    /// `localSheetId` mapped to a loaded sheet: visible from that sheet only,
    /// where it shadows a same-named global.
    Local(SheetId),
    /// `localSheetId` pointed at a `<sheets>` entry the loader skipped
    /// (chartsheet/dialogsheet/macrosheet/veryHidden no-part sheet) or out of
    /// range. The scope sheet hosts no loaded formulas, so this name matches
    /// **nowhere** — it must neither shadow nor stand in for a global.
    LocalUnmapped,
}

/// A defined name after load-time scope translation: the raw
/// [`xl_io::DefinedName`] with its `localSheetId` (an index into the full
/// `<sheets>` collection, [`xl_io::Sheet::sheets_index`]) rewritten into a
/// [`NameScope`] over engine [`SheetId`]s — the two index spaces diverge
/// whenever the loader skipped a `<sheet>` entry.
#[derive(Clone, Debug)]
pub(crate) struct ScopedName {
    /// The name as declared (matching is ASCII-case-insensitive).
    pub name: String,
    /// The raw body text (no leading `=`), parsed lazily at resolution.
    pub formula: String,
    /// Which sheet(s) may resolve this name.
    pub scope: NameScope,
}

/// Workbook-level lookup tables the analyzer and evaluator share: sheet name →
/// id (ASCII-lowercased keys) and the defined-name list.
pub(crate) struct WbIndex<'a> {
    /// ASCII-lowercased sheet name → its [`SheetId`] (the 0-based tab index).
    pub sheet_ids: &'a BTreeMap<String, SheetId>,
    /// Defined names with load-translated scopes (ECMA-376 §18.2.6): resolution
    /// prefers a [`NameScope::Local`] match on the resolving sheet, then falls
    /// back to [`NameScope::Global`]. See [`resolve_name`].
    pub defined_names: &'a [ScopedName],
    /// Cells whose own formula head is a `SUBTOTAL` call (RFC 0002). Consulted
    /// only by the evaluator's provenance-tagged cell walk
    /// (`EngineArgs::for_each_cell_tagged`) so `SUBTOTAL` can exclude nested
    /// SUBTOTALs; reference resolution and precedent extraction ignore it. The
    /// precedent-collection call sites pass an empty set — nothing there reads
    /// it — while the eval path passes the workbook's live tag set.
    pub subtotals: &'a BTreeSet<CellId>,
    /// The `(sheet, 0-based row)` pairs Excel is not displaying — the OOXML
    /// `<row hidden="1">` rows `xl-io` parsed (OXP-121). Like `subtotals`, this
    /// is read only by the provenance-tagged cell walk
    /// (`EngineArgs::for_each_cell_tagged`), which sets `CellFlags::is_hidden_row`
    /// for a streamed cell whose `(sheet, row)` is in this set, so `SUBTOTAL`'s
    /// `101`–`111` forms can drop hidden-row cells. Reference resolution and
    /// precedent extraction ignore it; the precedent call sites pass an empty
    /// set, the eval path passes the workbook's live set.
    pub hidden_rows: &'a BTreeSet<(SheetId, u32)>,
    /// **M2 lane 4 (dynamic-array spill).** The live anchor → spilled-rectangle
    /// map ([`crate::Engine::spills`]), so the evaluator can resolve a spill
    /// reference (`A1#` / `_xlfn.ANCHORARRAY(A1)`) to the anchor's current
    /// materialized region and stream it from the store — the read-side analogue
    /// of the RFC-0003 `OFFSET`/`INDIRECT` reference seam (see
    /// [`crate::refx::resolve_ref_expr`]). Each rectangle is on its anchor cell's
    /// own sheet and always contains the anchor as its top-left. This is
    /// eval-derived materialized state (like the value store), **not** a static
    /// graph fact: precedent extraction never reads it (those call sites pass an
    /// empty map — the `#` operand's anchor cell is already a plain `Ref`
    /// precedent, RFC-0012 finding 4), and only the run-cell eval path passes the
    /// engine's live map. Provenance: RFC-0012 (spine), OXP-204 (1×1 anchor
    /// registration), `spike/spill-sequence`.
    pub spills: &'a BTreeMap<CellId, RectRange>,
    /// Whether the **currently-evaluating cell's** formula was array-entered
    /// (legacy CSE `<f t="array">`; see [`xl_io::RawFormula::is_array_entered`]).
    ///
    /// Unlike the other fields this is not a workbook-wide table but a per-cell
    /// evaluation flag — `Engine::run_cell` rebuilds this `WbIndex` for each
    /// cell, stamping it with that cell's array-entry status. It is consulted at
    /// exactly one seam: the `Binary { op: Range }` scalar-context arm in
    /// [`crate::eval`]. A **non**-array formula (`false`) applies Excel's legacy
    /// implicit intersection to a range reaching scalar context; an array
    /// formula (`true`) does **not** intersect (OXP-004/163,
    /// RUN-2026-07-11-oracle01). Precedent extraction never reads it — those
    /// call sites pass `false`.
    pub is_array_formula: bool,
}

/// Whether a parsed formula's **head** is a `SUBTOTAL(...)` call — the RFC 0002
/// nested-exclusion tag predicate.
///
/// Matches the top-level call only (the AST head), case-insensitively via the
/// canonicalized `xl-ast` function name (`_xlfn.`/`_xlws.` prefixes already
/// stripped and uppercased). A `SUBTOTAL` buried inside a larger expression
/// (e.g. `=SUBTOTAL(9,A1:A5)+1`) is deliberately **not** tagged: Excel's Data ▸
/// Subtotal feature and the documented nested-exclusion behavior are about cells
/// that *are* a SUBTOTAL, and tagging arbitrary sub-expressions would need
/// oracle confirmation this task does not have.
pub(crate) fn head_is_subtotal(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::Call { name, .. } if name.canonical == "SUBTOTAL")
}

/// The outcome of resolving a single (non-range) [`Reference`].
pub(crate) enum RefResolution {
    /// A resolved single cell.
    Cell(CellId),
    /// A construct the engine does not resolve in v0 (3-D, R1C1,
    /// whole-column/row on its own, bad sheet).
    Unsupported,
}

/// Resolve a reference's sheet qualifier to a [`SheetId`].
///
/// `None` qualifier → the current sheet. A single named sheet resolves through
/// `index`. A 3-D span (`last` set) or workbook-global (`!Name`) prefix is
/// `None` (unsupported here — handled as `#UNSUPPORTED!` upstream).
fn resolve_sheet(sheet: &Option<SheetRef>, cur: SheetId, env: &WbIndex) -> Option<SheetId> {
    match sheet {
        None => Some(cur),
        Some(sr) => {
            if sr.workbook_global || sr.last.is_some() {
                return None;
            }
            env.sheet_ids.get(&sr.first.to_ascii_lowercase()).copied()
        }
    }
}

/// Resolve a non-range reference to a single [`CellId`], or [`RefResolution::Unsupported`].
pub(crate) fn resolve_reference(r: &Reference, cur: SheetId, env: &WbIndex) -> RefResolution {
    let Some(sheet) = resolve_sheet(&r.sheet, cur, env) else {
        return RefResolution::Unsupported;
    };
    match r.kind {
        RefKind::Cell(col, row) => match (row.index.checked_sub(1), col.index.checked_sub(1)) {
            (Some(r0), Some(c0)) => RefResolution::Cell(CellId::new(sheet, r0, c0)),
            _ => RefResolution::Unsupported,
        },
        // Whole column/row are only meaningful as a range endpoint; R1C1 is not
        // produced in A1 parse mode.
        RefKind::Col(_) | RefKind::Row(_) | RefKind::R1C1(_) => RefResolution::Unsupported,
    }
}

/// Resolve a `lhs:rhs` range binary to a [`SheetRange`], or `None` if it is not
/// a supported cell/whole-column/whole-row range on a single resolvable sheet.
pub(crate) fn resolve_range(
    lhs: &Expr,
    rhs: &Expr,
    cur: SheetId,
    env: &WbIndex,
) -> Option<SheetRange> {
    let (ExprKind::Ref(l), ExprKind::Ref(r)) = (&lhs.kind, &rhs.kind) else {
        return None;
    };
    let sheet = resolve_sheet(&l.sheet, cur, env)?;
    // The right endpoint must not carry a divergent sheet qualifier. `xl-ast`
    // rejects per-endpoint sheet qualifiers (OXP-053), so `r.sheet` is normally
    // `None`; guard anyway.
    if r.sheet.is_some() && resolve_sheet(&r.sheet, cur, env) != Some(sheet) {
        return None;
    }
    let rect = match (l.kind, r.kind) {
        (RefKind::Cell(lc, lr), RefKind::Cell(rc, rr)) => {
            let (r0, r1) = (lr.index.checked_sub(1)?, rr.index.checked_sub(1)?);
            let (c0, c1) = (lc.index.checked_sub(1)?, rc.index.checked_sub(1)?);
            RectRange::new(r0.min(r1), r0.max(r1), c0.min(c1), c0.max(c1))
        }
        (RefKind::Col(la), RefKind::Col(ra)) => {
            let (c0, c1) = (la.index.checked_sub(1)?, ra.index.checked_sub(1)?);
            RectRange::new(0, MAX_ROW0, c0.min(c1), c0.max(c1))
        }
        (RefKind::Row(la), RefKind::Row(ra)) => {
            let (r0, r1) = (la.index.checked_sub(1)?, ra.index.checked_sub(1)?);
            RectRange::new(r0.min(r1), r0.max(r1), 0, MAX_COL0)
        }
        _ => return None,
    };
    Some(SheetRange::new(sheet, rect))
}

/// Whether a rectangle covers exactly one cell.
pub(crate) fn rect_is_single(rect: &RectRange) -> bool {
    rect.row_start == rect.row_end && rect.col_start == rect.col_end
}

/// Resolve a defined name to its underlying reference expression, if it is a
/// **simple** reference or range.
///
/// # Scope (ECMA-376 §18.2.6, lane L2-D)
/// An **unqualified** name resolved from sheet `cur` prefers a name scoped
/// locally to `cur` ([`NameScope::Local`]), then falls back to a
/// workbook-global one — the sheet-local name shadows the global within its
/// sheet, and every other sheet sees the global. Within one scope the first
/// declaration in document order wins (§18.2.6 requires per-scope uniqueness,
/// so a duplicate only occurs in a malformed file; taking the first is
/// deterministic). A [`NameScope::LocalUnmapped`] name (scoped to a skipped
/// `<sheets>` entry) matches nowhere.
///
/// A **sheet-qualified** name reference (`Sheet2!name`) yields `None`
/// unconditionally: §18.2.6 pins how names are *stored and scoped*, not which
/// scope a qualified reference selects, so the engine refuses loudly instead
/// of guessing (before L2-D the qualifier was silently ignored and the global
/// served — a silent-wrong risk). The confirmation probe rides the next farm
/// batch (`docs/l2-refusal-decomposition.md`, review condition 3).
///
/// # Body (unchanged from v0)
/// Only a body that parses to a lone reference or a `ref:ref` range
/// (parentheses stripped) is served. Anything else — a computed name, a
/// constant, a name-of-a-name — yields `None`, so the caller produces
/// `#UNSUPPORTED!` rather than guessing. This also prevents unbounded
/// name→name recursion (only a bare ref is accepted).
pub(crate) fn resolve_name(name: &NameRef, cur: SheetId, env: &WbIndex) -> Option<Expr> {
    if name.sheet.is_some() {
        return None;
    }
    let dn = env
        .defined_names
        .iter()
        .find(|d| d.scope == NameScope::Local(cur) && d.name.eq_ignore_ascii_case(&name.name))
        .or_else(|| {
            env.defined_names
                .iter()
                .find(|d| d.scope == NameScope::Global && d.name.eq_ignore_ascii_case(&name.name))
        })?;
    let mut expr = xl_ast::parse(&dn.formula).ok()?;
    // Strip a single layer of grouping parentheses.
    while let ExprKind::Paren(inner) = expr.kind {
        expr = *inner;
    }
    match &expr.kind {
        ExprKind::Ref(_) => Some(expr),
        ExprKind::Binary {
            op: BinaryOp::Range,
            ..
        } => Some(expr),
        _ => None,
    }
}

/// Collect the graph precedents (single cells and ranges) a formula reads.
///
/// Over-approximation is safe and intended: precedents from an *unselected* `IF`
/// branch are still collected (the value evaluator is lazy, but the dependency
/// graph is static). Unresolvable references contribute no precedent — the cell
/// that reads them evaluates to `#UNSUPPORTED!` regardless.
pub(crate) fn collect_precedents(
    expr: &Expr,
    cur: SheetId,
    env: &WbIndex,
    out: &mut Vec<Precedent>,
) {
    match &expr.kind {
        ExprKind::Ref(r) => {
            if let RefResolution::Cell(c) = resolve_reference(r, cur, env) {
                out.push(Precedent::Cell(c));
            }
        }
        ExprKind::Binary {
            op: BinaryOp::Range,
            lhs,
            rhs,
        } => {
            if let Some(sr) = resolve_range(lhs, rhs, cur, env) {
                out.push(Precedent::Range(sr));
            }
        }
        ExprKind::Name(n) => {
            if let Some(resolved) = resolve_name(n, cur, env) {
                collect_precedents(&resolved, cur, env, out);
            }
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::Postfix { expr, .. }
        | ExprKind::Paren(expr)
        | ExprKind::ImplicitIntersection(expr) => collect_precedents(expr, cur, env, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_precedents(lhs, cur, env, out);
            collect_precedents(rhs, cur, env, out);
        }
        ExprKind::Call { args, .. } => {
            for a in args {
                collect_precedents(a, cur, env, out);
            }
        }
        ExprKind::Array(rows) => {
            for row in rows {
                for e in row {
                    collect_precedents(e, cur, env, out);
                }
            }
        }
        ExprKind::Number(_)
        | ExprKind::Text(_)
        | ExprKind::Bool(_)
        | ExprKind::Error(_)
        | ExprKind::Missing
        | ExprKind::Unsupported { .. } => {}
    }
}

/// Whether any function call anywhere in the formula is volatile (per
/// [`xl_fn::is_volatile`]). Volatile-*listed* but unimplemented functions
/// (`NOW`, `OFFSET`, …) still count, so the graph reschedules the cell every
/// recalc even though evaluating the call yields `#UNSUPPORTED!`.
pub(crate) fn contains_volatile(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Call { name, args } => {
            is_volatile(&name.canonical) || args.iter().any(contains_volatile)
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::Postfix { expr, .. }
        | ExprKind::Paren(expr)
        | ExprKind::ImplicitIntersection(expr) => contains_volatile(expr),
        ExprKind::Binary { lhs, rhs, .. } => contains_volatile(lhs) || contains_volatile(rhs),
        ExprKind::Array(rows) => rows.iter().flatten().any(contains_volatile),
        _ => false,
    }
}
