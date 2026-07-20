//! Reference-returning function resolution (RFC 0003, Phase 1): `OFFSET` and
//! `INDIRECT`.
//!
//! # Why this module exists (RFC 0003 Seam A — approach **A2**)
//! `OFFSET(reference, rows, cols, [height], [width])` and `INDIRECT(ref_text,
//! [a1])` do not return a scalar or array — they return a **reference** (a cell
//! or a rectangular range) that a consuming function then streams as a range or
//! dereferences to a scalar. Rather than widen the frozen `xl-value` `Value`
//! contract with a new computed-reference variant (approach A1), this module
//! makes the engine's own reference resolver understand the two functions as
//! *reference transformers*: [`resolve_ref_expr`] turns an `OFFSET`/`INDIRECT`
//! call expression into a concrete [`SheetRange`] (or an Excel error), so the
//! evaluator's scalar-dereference path and `EngineArgs`'
//! `for_each_cell`/`for_each_row`/`dims` serve them exactly as they already
//! serve a literal `A1:D10`. The `xl-fn` value contract is untouched; `OFFSET`
//! and `INDIRECT` are **not** registry functions.
//!
//! # Seam C — dynamic dependencies (batch full-recalc; RFC choice **(b)**)
//! `OFFSET`/`INDIRECT` targets are not visible in the static dependency graph
//! (they are computed at eval time, and for `INDIRECT` parsed from a runtime
//! string), which is why both are volatile. This Phase-1 implementation reads
//! the target cells' **current value from the store** (`values`), which is
//! seeded with the file's cached values at load and refreshed in topological
//! order during recalc. It does **not** attempt on-demand recursive evaluation
//! of an un-computed target (approach (a)) — that would require threading
//! mutable engine state (programs, graph, cycle detection) through the whole
//! otherwise-immutable eval/`CallArgs` path, a large and ordering-risky change.
//! The residual hazard — a dynamic target whose fresh recompute differs from
//! its cached value **and** is scheduled after the referencing cell — is
//! documented as **OXP-144** rather than silently returning a stale value; for
//! a faithfully-reproduced cached-value corpus cell (computed == cached) the
//! two agree, so the common static cases score.
//!
//! # Oracle-resolved corners (RUN-2026-07-11-oracle01)
//! - **OXP-140** — a non-integer `rows`/`cols`/`height`/`width` argument to
//!   `OFFSET` is **truncated toward zero** (`1.5`→`1`, `2.9`→`2`; the sole
//!   negative probe `-1.5` lands off the top edge either way → `#REF!`).
//! - **OXP-141** — a zero or negative `height`/`width` to `OFFSET` → `#REF!`
//!   (`0`, `-2`, and width `0` all observed as `#REF!`).
//! - **OXP-142** — a *relative* or partial R1C1 target in `INDIRECT(...,FALSE)`
//!   (`R[1]C`, `RC[1]`, whole-row `R5`) resolves against the **anchor cell**
//!   (the cell containing the formula, now threaded in): an absolute axis is
//!   1-based, a relative offset adds to the anchor's index, an absent axis
//!   fills from the anchor's own coordinate (implicit intersection). An R1C1
//!   *range* endpoint remains deferred (unprobed).
//! - **OXP-143** — an `INDIRECT` target on another workbook (`[Book2]Sheet1!A1`)
//!   or a 3-D span (`Sheet1:Sheet3!A1`) → `#REF!` (v1 single-workbook), the
//!   same result Excel gives, distinct from a valid single sheet.

use xl_ast::{
    BinaryOp, Expr, ExprKind, ParseMode, PostfixOp, R1C1Axis, R1C1Ref, RefKind, Reference,
    SheetRef, parse_with_mode,
};
use xl_fn::EvalContext;
use xl_graph::{CellId, SheetRange};
use xl_value::{ErrorKind, RectRange, SheetId, to_bool, to_number, to_text};

use crate::analyze::{
    MAX_COL0, MAX_ROW0, RefResolution, WbIndex, resolve_name, resolve_range, resolve_reference,
};
use crate::eval::{ValueStore, eval_expr};
use crate::{DiagnosticKind, DiagnosticSink};

/// Bound on nested reference-returning calls (`SUM(OFFSET(INDIRECT(...),...))`),
/// so a pathological formula cannot recurse without limit (RFC 0003 open
/// question on bounded recursion depth). Far above any realistic nesting.
const MAX_REF_DEPTH: u32 = 16;

/// The outcome of resolving an expression used in **reference** position (a
/// range argument, an `OFFSET` base, or an `INDIRECT` target).
pub(crate) enum RefExpr {
    /// Resolved to a concrete rectangle on one sheet.
    Range(SheetRange),
    /// A hard Excel error the reference computation itself produced (e.g.
    /// `#REF!` for an off-sheet `OFFSET` or an unparseable `INDIRECT` string).
    Error(ErrorKind),
    /// Not resolvable as a reference in Phase 1 → `#UNSUPPORTED!`. A diagnostic
    /// is pushed at the point of refusal (never a silent guess).
    Unsupported,
}

/// Whether `canonical` (an `xl-ast`-canonicalized function name) is one of the
/// reference-returning functions the engine resolves here instead of dispatching
/// through the `xl-fn` registry.
///
/// - `OFFSET`/`INDIRECT` — RFC-0003 Phase 1.
/// - `ANCHORARRAY` — M2 lane 4: Excel serializes the spilled-range operator
///   `A1#` in OOXML as `_xlfn.ANCHORARRAY(A1)` (OXP-204). Routing it here means
///   `SUM`/`ROWS`/`INDEX`/… serve `A1#` through the exact same
///   `for_each_cell`/`dims`/`arg_ref_extent` paths that already serve
///   `OFFSET`/`INDIRECT` — no new streaming plumbing. The user-typed spelling
///   `A1#` (a `PostfixOp::SpillRange`, see [`is_ref_expr`]) resolves the same
///   way.
pub(crate) fn is_ref_returning(canonical: &str) -> bool {
    canonical == "OFFSET" || canonical == "INDIRECT" || canonical == "ANCHORARRAY"
}

/// Whether `e` appears in **reference position** and is resolved by
/// [`resolve_ref_expr`] rather than evaluated as a value: a reference-returning
/// call ([`is_ref_returning`]) **or** the spilled-range postfix `A1#`
/// (`PostfixOp::SpillRange`). This is the predicate the evaluator's
/// argument-streaming seams (`for_each_cell`/`dims`/…) gate on, so a spill
/// reference streams from the store exactly like a literal `A1:A3` (M2 lane 4).
///
/// The two OOXML spellings of the spill operator — the postfix `A1#` (what a
/// user types / the parser produces) and the serialized `_xlfn.ANCHORARRAY(A1)`
/// (what Excel writes) — both resolve through here (OXP-204).
pub(crate) fn is_ref_expr(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Call { name, .. } => is_ref_returning(&name.canonical),
        ExprKind::Postfix {
            op: PostfixOp::SpillRange,
            ..
        } => true,
        _ => false,
    }
}

/// Resolve an expression that appears in reference position to a concrete
/// [`SheetRange`], an Excel error, or `#UNSUPPORTED!`.
///
/// Handles the ordinary reference shapes (a bare cell ref, a `ref:ref` range, a
/// simple defined name) as well as the two reference-returning calls, so an
/// `OFFSET` whose base is itself an `INDIRECT` (or a defined name) resolves by
/// recursion, bounded by [`MAX_REF_DEPTH`].
///
/// `anchor` is the cell containing the formula being evaluated (its sheet equals
/// `cur`); it is the origin for relative/partial R1C1 targets in `INDIRECT`
/// (OXP-142) and is threaded unchanged through every recursion.
#[allow(clippy::too_many_arguments)] // the immutable eval seam threaded to reference resolution
pub(crate) fn resolve_ref_expr(
    expr: &Expr,
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    depth: u32,
) -> RefExpr {
    if depth > MAX_REF_DEPTH {
        diags.push((
            DiagnosticKind::UnsupportedConstruct,
            "reference-returning call nested beyond the supported depth".to_string(),
        ));
        return RefExpr::Unsupported;
    }
    match &expr.kind {
        ExprKind::Paren(inner) => {
            resolve_ref_expr(inner, anchor, cur, values, env, ctx, diags, depth)
        }
        ExprKind::Ref(r) => match resolve_reference(r, cur, env) {
            RefResolution::Cell(c) => RefExpr::Range(SheetRange::new(
                c.sheet,
                RectRange::new(c.row, c.row, c.col, c.col),
            )),
            RefResolution::Unsupported => RefExpr::Unsupported,
        },
        ExprKind::Binary {
            op: BinaryOp::Range,
            lhs,
            rhs,
        } => match resolve_range(lhs, rhs, cur, env) {
            Some(sr) => RefExpr::Range(sr),
            None => RefExpr::Unsupported,
        },
        ExprKind::Name(n) => match resolve_name(n, env) {
            Some(resolved) => {
                resolve_ref_expr(&resolved, anchor, cur, values, env, ctx, diags, depth)
            }
            None => RefExpr::Unsupported,
        },
        ExprKind::Call { name, args } if name.canonical == "OFFSET" => {
            resolve_offset(args, anchor, cur, values, env, ctx, diags, depth)
        }
        ExprKind::Call { name, args } if name.canonical == "INDIRECT" => {
            resolve_indirect(args, anchor, cur, values, env, ctx, diags, depth)
        }
        // M2 lane 4: `A1#` (spilled-range postfix) and its serialized twin
        // `_xlfn.ANCHORARRAY(A1)` both resolve to the anchor's current spilled
        // rectangle, read live from the engine's anchor→region map (`env.spills`).
        // This is the read-side of the spill machinery: it materializes the
        // region from run-time state exactly as `OFFSET` materializes its target,
        // so a consumer (`SUM(A1#)`) streams it from the store as if it were
        // `A1:A3`. Provenance: RFC-0012 (finding 3), OXP-204, `spike/spill-sequence`.
        ExprKind::Postfix {
            op: PostfixOp::SpillRange,
            expr: operand,
        } => resolve_spill(operand, anchor, cur, values, env, ctx, diags, depth),
        ExprKind::Call { name, args } if name.canonical == "ANCHORARRAY" => {
            if args.len() != 1 {
                diags.push((
                    DiagnosticKind::UnsupportedConstruct,
                    "ANCHORARRAY expects exactly 1 argument".to_string(),
                ));
                return RefExpr::Unsupported;
            }
            resolve_spill(&args[0], anchor, cur, values, env, ctx, diags, depth)
        }
        // Anything else is not a reference; the caller decides the error.
        _ => RefExpr::Unsupported,
    }
}

/// Resolve a spilled-range reference (`A1#` / `ANCHORARRAY(A1)`) to the anchor's
/// current spilled rectangle.
///
/// The single operand resolves (recursively, so `OFFSET(...)#` would work) to a
/// reference; its top-left cell is the anchor. The anchor's live region is looked
/// up in `env.spills`. A currently-spilling anchor (including a **1×1** dynamic
/// array — OXP-204/BC-10 registers those) yields its full rectangle; a cell with
/// no registered spill degrades to the single anchor cell (Excel's `A1#` on a
/// non-spilling scalar `A1` is just `A1`). This reads the materialized region
/// from run-time state — never a static guess.
#[allow(clippy::too_many_arguments)]
fn resolve_spill(
    operand: &Expr,
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    depth: u32,
) -> RefExpr {
    let anchor_cell =
        match resolve_ref_expr(operand, anchor, cur, values, env, ctx, diags, depth + 1) {
            RefExpr::Range(sr) => CellId::new(sr.sheet, sr.range.row_start, sr.range.col_start),
            RefExpr::Error(k) => return RefExpr::Error(k),
            RefExpr::Unsupported => return RefExpr::Unsupported,
        };
    match env.spills.get(&anchor_cell) {
        Some(rect) => RefExpr::Range(SheetRange::new(anchor_cell.sheet, *rect)),
        None => RefExpr::Range(SheetRange::new(
            anchor_cell.sheet,
            RectRange::new(
                anchor_cell.row,
                anchor_cell.row,
                anchor_cell.col,
                anchor_cell.col,
            ),
        )),
    }
}

/// An integer argument coercion outcome.
enum IntArg {
    /// A whole-number value (a fractional input truncated toward zero).
    Int(i64),
    /// An error propagated from the argument (e.g. a `#DIV/0!` in `rows`).
    Error(ErrorKind),
    /// A value the engine refuses to interpret (a non-finite number) →
    /// `#UNSUPPORTED!`.
    Defer,
}

/// Coerce an argument expression to an **integer** via `to_number`, truncating
/// a fractional value **toward zero**.
///
/// OXP-140 (RUN-2026-07-11-oracle01) pinned truncation-toward-zero for `OFFSET`
/// numeric arguments: `OFFSET(A1,1.5,0)`→row+1 and `SUM(OFFSET(A1,0,0,2.9,1))`
/// summed two rows (`2.9`→`2`); the sole negative probe `OFFSET(A1,-1.5,0)`
/// lands off the top edge under either rounding direction → `#REF!`. Excel's
/// documented "must be an integer" convention drops the fraction; `n.trunc()`
/// reproduces every probed value. A huge magnitude (`1E19`) saturates the
/// `as i64` cast (Rust's saturating float→int), which the caller's i128 bounds
/// math then rejects as off-sheet without overflowing.
fn int_arg(
    expr: &Expr,
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
) -> IntArg {
    // M2 lane 2: reference-resolution (OFFSET/INDIRECT argument evaluation) is
    // NOT threaded with a lexical environment — a LET/LAMBDA parameter is not
    // visible inside a reference function's argument, so `lex = None`. If one is
    // referenced there it resolves loudly (`#UNSUPPORTED!` via the defined-name
    // path), never silently — this is a documented scope limitation, not a drop.
    let v = eval_expr(expr, anchor, cur, values, env, ctx, diags, false, None);
    match to_number(&v) {
        Ok(n) => {
            if n.is_finite() {
                IntArg::Int(n.trunc() as i64)
            } else {
                diags.push((
                    DiagnosticKind::UnsupportedConstruct,
                    "OFFSET non-finite numeric argument".to_string(),
                ));
                IntArg::Defer
            }
        }
        Err(k) => IntArg::Error(k),
    }
}

/// Resolve an `OFFSET(reference, rows, cols, [height], [width])` call.
#[allow(clippy::too_many_arguments)] // the immutable eval seam threaded to reference resolution
fn resolve_offset(
    args: &[Expr],
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    depth: u32,
) -> RefExpr {
    if !(3..=5).contains(&args.len()) {
        diags.push((
            DiagnosticKind::UnsupportedConstruct,
            "OFFSET expects 3 to 5 arguments".to_string(),
        ));
        return RefExpr::Unsupported;
    }
    // The base reference — itself possibly a range, a name, or a nested
    // reference-returning call.
    let base = match resolve_ref_expr(&args[0], anchor, cur, values, env, ctx, diags, depth + 1) {
        RefExpr::Range(sr) => sr,
        RefExpr::Error(k) => return RefExpr::Error(k),
        RefExpr::Unsupported => {
            diags.push((
                DiagnosticKind::UnsupportedConstruct,
                "OFFSET reference argument does not resolve to a reference".to_string(),
            ));
            return RefExpr::Unsupported;
        }
    };

    let rows = match int_arg(&args[1], anchor, cur, values, env, ctx, diags) {
        IntArg::Int(n) => n,
        IntArg::Error(k) => return RefExpr::Error(k),
        IntArg::Defer => return RefExpr::Unsupported,
    };
    let cols = match int_arg(&args[2], anchor, cur, values, env, ctx, diags) {
        IntArg::Int(n) => n,
        IntArg::Error(k) => return RefExpr::Error(k),
        IntArg::Defer => return RefExpr::Unsupported,
    };

    let base_rows = i64::from(base.range.row_end - base.range.row_start) + 1;
    let base_cols = i64::from(base.range.col_end - base.range.col_start) + 1;

    let height = match opt_size_arg(args.get(3), anchor, cur, values, env, ctx, diags, base_rows) {
        SizeArg::Size(n) => n,
        SizeArg::Error(k) => return RefExpr::Error(k),
        SizeArg::Defer => return RefExpr::Unsupported,
    };
    let width = match opt_size_arg(args.get(4), anchor, cur, values, env, ctx, diags, base_cols) {
        SizeArg::Size(n) => n,
        SizeArg::Error(k) => return RefExpr::Error(k),
        SizeArg::Defer => return RefExpr::Unsupported,
    };

    // Bounds math in i128: `rows`/`cols` come from a *saturating* `n as i64`
    // cast, so a huge OFFSET offset (e.g. `OFFSET(A1,1E19,0)`) yields i64::MAX and
    // an i64 `+ height - 1` would OVERFLOW (a panic under overflow-checks / fuzz).
    // i128 cannot overflow for any i64 inputs, and the result is only narrowed
    // back to u32 after the in-range check below passes.
    let new_r0 = i128::from(base.range.row_start) + i128::from(rows);
    let new_c0 = i128::from(base.range.col_start) + i128::from(cols);
    let new_r1 = new_r0 + i128::from(height) - 1;
    let new_c1 = new_c0 + i128::from(width) - 1;

    // Any part of the result off the sheet edge → #REF! (OFFSET.md §Error).
    if new_r0 < 0 || new_c0 < 0 || new_r1 > i128::from(MAX_ROW0) || new_c1 > i128::from(MAX_COL0) {
        return RefExpr::Error(ErrorKind::Ref);
    }

    RefExpr::Range(SheetRange::new(
        base.sheet,
        RectRange::new(new_r0 as u32, new_r1 as u32, new_c0 as u32, new_c1 as u32),
    ))
}

/// An optional `height`/`width` coercion outcome.
enum SizeArg {
    /// A positive whole-number size (or the default when omitted).
    Size(i64),
    /// An error propagated from the argument, or a zero/negative size (`#REF!`,
    /// OXP-141).
    Error(ErrorKind),
    /// A non-finite size argument → deferred.
    Defer,
}

/// Coerce an optional `height`/`width` argument. Absent or an elided slot →
/// `default`. A fractional value is truncated toward zero (OXP-140, via
/// [`int_arg`]); a resulting zero or negative size → `#REF!` (OXP-141:
/// RUN-2026-07-11-oracle01 observed `0`, `-2`, and width `0` all as `#REF!`).
#[allow(clippy::too_many_arguments)] // the immutable eval seam threaded to reference resolution
fn opt_size_arg(
    slot: Option<&Expr>,
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    default: i64,
) -> SizeArg {
    let Some(expr) = slot else {
        return SizeArg::Size(default);
    };
    if matches!(expr.kind, ExprKind::Missing) {
        return SizeArg::Size(default);
    }
    match int_arg(expr, anchor, cur, values, env, ctx, diags) {
        IntArg::Int(n) => {
            if n > 0 {
                SizeArg::Size(n)
            } else {
                SizeArg::Error(ErrorKind::Ref)
            }
        }
        IntArg::Error(k) => SizeArg::Error(k),
        IntArg::Defer => SizeArg::Defer,
    }
}

/// Resolve an `INDIRECT(ref_text, [a1])` call.
#[allow(clippy::too_many_arguments)] // the immutable eval seam threaded to reference resolution
fn resolve_indirect(
    args: &[Expr],
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    depth: u32,
) -> RefExpr {
    if !(1..=2).contains(&args.len()) {
        diags.push((
            DiagnosticKind::UnsupportedConstruct,
            "INDIRECT expects 1 or 2 arguments".to_string(),
        ));
        return RefExpr::Unsupported;
    }

    let text_val = eval_expr(&args[0], anchor, cur, values, env, ctx, diags, false, None);
    let text = match to_text(&text_val) {
        Ok(t) => t,
        Err(k) => return RefExpr::Error(k),
    };

    // `a1` defaults to TRUE (A1 style); an elided slot also defaults.
    let a1 = match args.get(1) {
        Some(a) if !matches!(a.kind, ExprKind::Missing) => {
            let bv = eval_expr(a, anchor, cur, values, env, ctx, diags, false, None);
            match to_bool(&bv) {
                Ok(b) => b,
                Err(k) => return RefExpr::Error(k),
            }
        }
        _ => true,
    };

    let mode = if a1 { ParseMode::A1 } else { ParseMode::R1C1 };
    // Parse the runtime string with the EXISTING reference parser — no new
    // parser (RFC 0003 INDIRECT specifics). Any parse failure → #REF!.
    let mut parsed = match parse_with_mode(text.as_str(), mode) {
        Ok(e) => e,
        Err(_) => return RefExpr::Error(ErrorKind::Ref),
    };
    while let ExprKind::Paren(inner) = parsed.kind {
        parsed = *inner;
    }

    match &parsed.kind {
        ExprKind::Ref(r) => resolve_indirect_ref(r, anchor, cur, env, diags),
        ExprKind::Binary {
            op: BinaryOp::Range,
            lhs,
            rhs,
        } => resolve_indirect_range(lhs, rhs, cur, env, diags),
        // A reference intersection/union (`"A1:A5 B1:B5"`, `"A1,B1"`) IS a
        // reference expression Excel would compute — but the engine does not yet
        // resolve intersection/union anywhere, so defer rather than guess `#REF!`.
        ExprKind::Binary {
            op: BinaryOp::Intersect | BinaryOp::Union,
            ..
        } => {
            diags.push((
                DiagnosticKind::UnsupportedConstruct,
                "INDIRECT to a reference intersection/union is unsupported".to_string(),
            ));
            RefExpr::Unsupported
        }
        // A bareword parses to a defined Name. Route it through the name table
        // (RFC 0003 §INDIRECT specifics: "defined-name resolution route through
        // the existing name table") exactly as the OFFSET-base path does — a
        // defined name resolving to a simple ref/range is served; a name holding
        // something non-referential falls through resolve_ref_expr to
        // #UNSUPPORTED!; a genuinely *undefined* name → #REF! (Excel's result for
        // INDIRECT of an unresolvable name). This replaces a blanket #REF! that
        // masqueraded as deliberate Excel behavior for every named-range INDIRECT.
        ExprKind::Name(n) => match resolve_name(n, env) {
            Some(resolved) => {
                resolve_ref_expr(&resolved, anchor, cur, values, env, ctx, diags, depth)
            }
            None => RefExpr::Error(ErrorKind::Ref),
        },
        // Parses, but not a reference at all (e.g. INDIRECT("1+1")) → #REF!.
        _ => RefExpr::Error(ErrorKind::Ref),
    }
}

/// Resolve an `INDIRECT` sheet qualifier, mapping failures to Excel errors: a
/// nonexistent single sheet → `#REF!`; a 3-D span or another workbook → `#REF!`
/// as well (OXP-143, RUN-2026-07-11-oracle01: `Sheet1:Sheet3!A1` and
/// `[Book2]Sheet1!A1` both observed `#REF!` under v1's single-workbook scope,
/// distinct from a valid single sheet).
fn resolve_indirect_sheet(
    sheet: &Option<SheetRef>,
    cur: SheetId,
    env: &WbIndex,
    _diags: &mut DiagnosticSink,
) -> Result<SheetId, RefExpr> {
    match sheet {
        None => Ok(cur),
        Some(sr) => {
            if sr.workbook_global || sr.last.is_some() {
                Err(RefExpr::Error(ErrorKind::Ref))
            } else {
                match env.sheet_ids.get(&sr.first.to_ascii_lowercase()).copied() {
                    Some(id) => Ok(id),
                    None => Err(RefExpr::Error(ErrorKind::Ref)),
                }
            }
        }
    }
}

/// Resolve a single (non-range) parsed `INDIRECT` reference (A1 cell or any R1C1
/// cell, resolved against the `anchor`).
fn resolve_indirect_ref(
    r: &Reference,
    anchor: CellId,
    cur: SheetId,
    env: &WbIndex,
    diags: &mut DiagnosticSink,
) -> RefExpr {
    let sheet = match resolve_indirect_sheet(&r.sheet, cur, env, diags) {
        Ok(s) => s,
        Err(re) => return re,
    };
    match &r.kind {
        RefKind::Cell(col, row) => match (row.index.checked_sub(1), col.index.checked_sub(1)) {
            (Some(r0), Some(c0)) => {
                RefExpr::Range(SheetRange::new(sheet, RectRange::new(r0, r0, c0, c0)))
            }
            _ => RefExpr::Error(ErrorKind::Ref),
        },
        RefKind::R1C1(a) => resolve_r1c1_cell(sheet, a, anchor),
        // A lone whole-column/row token as an INDIRECT target is rare; defer.
        RefKind::Col(_) | RefKind::Row(_) => {
            diags.push((
                DiagnosticKind::UnsupportedConstruct,
                "INDIRECT to a bare whole-column/row reference is unsupported".to_string(),
            ));
            RefExpr::Unsupported
        }
    }
}

/// Resolve an R1C1 single cell against the `anchor` (the formula's own cell).
///
/// OXP-142 (RUN-2026-07-11-oracle01) pinned relative/partial R1C1 targets in
/// `INDIRECT(...,FALSE)` against the containing cell: `INDIRECT("R[1]C",FALSE)`
/// in H10 → H11, `INDIRECT("RC[1]",FALSE)` in H13 → I13, and the whole-row form
/// `INDIRECT("R5",FALSE)` in column H → H5 (implicit intersection with the
/// anchor's column). Each axis resolves independently via [`resolve_r1c1_axis`];
/// an axis landing off the sheet edge → `#REF!`.
fn resolve_r1c1_cell(sheet: SheetId, a: &R1C1Ref, anchor: CellId) -> RefExpr {
    let Some(r0) = resolve_r1c1_axis(a.row, anchor.row, MAX_ROW0) else {
        return RefExpr::Error(ErrorKind::Ref);
    };
    let Some(c0) = resolve_r1c1_axis(a.col, anchor.col, MAX_COL0) else {
        return RefExpr::Error(ErrorKind::Ref);
    };
    RefExpr::Range(SheetRange::new(sheet, RectRange::new(r0, r0, c0, c0)))
}

/// Resolve one R1C1 axis to a 0-based grid index, or `None` if it falls off the
/// sheet (→ `#REF!`): an absolute axis is a 1-based index (`R5`→`4`), a relative
/// axis offsets the `anchor`'s own index (`R[1]` from row 9 → `10`, bare `R` =
/// `Relative(0)` → the anchor's row), and an absent axis fills from the anchor's
/// coordinate on that axis (whole-row `R5` in column H → column H). See OXP-142.
fn resolve_r1c1_axis(axis: Option<R1C1Axis>, anchor: u32, max: u32) -> Option<u32> {
    let idx = match axis {
        None => i64::from(anchor),
        Some(R1C1Axis::Absolute(n)) => i64::from(n) - 1,
        Some(R1C1Axis::Relative(d)) => i64::from(anchor) + i64::from(d),
    };
    if (0..=i64::from(max)).contains(&idx) {
        Some(idx as u32)
    } else {
        None
    }
}

/// Resolve a parsed `INDIRECT` range (`A1:B10`, `Sheet2!A1:A5`) in A1 mode. R1C1
/// range endpoints are deferred (mirrors [`resolve_r1c1_cell`]).
fn resolve_indirect_range(
    lhs: &Expr,
    rhs: &Expr,
    cur: SheetId,
    env: &WbIndex,
    diags: &mut DiagnosticSink,
) -> RefExpr {
    let (ExprKind::Ref(l), ExprKind::Ref(r)) = (&lhs.kind, &rhs.kind) else {
        return RefExpr::Error(ErrorKind::Ref);
    };
    let sheet = match resolve_indirect_sheet(&l.sheet, cur, env, diags) {
        Ok(s) => s,
        Err(re) => return re,
    };
    // A divergent right-endpoint sheet qualifier is not a single rectangle.
    if r.sheet.is_some() {
        match resolve_indirect_sheet(&r.sheet, cur, env, diags) {
            Ok(s) if s == sheet => {}
            Ok(_) => return RefExpr::Error(ErrorKind::Ref),
            Err(re) => return re,
        }
    }
    let rect = match (l.kind, r.kind) {
        (RefKind::Cell(lc, lr), RefKind::Cell(rc, rr)) => {
            let (Some(r0), Some(r1)) = (lr.index.checked_sub(1), rr.index.checked_sub(1)) else {
                return RefExpr::Error(ErrorKind::Ref);
            };
            let (Some(c0), Some(c1)) = (lc.index.checked_sub(1), rc.index.checked_sub(1)) else {
                return RefExpr::Error(ErrorKind::Ref);
            };
            RectRange::new(r0.min(r1), r0.max(r1), c0.min(c1), c0.max(c1))
        }
        (RefKind::Col(la), RefKind::Col(ra)) => {
            let (Some(c0), Some(c1)) = (la.index.checked_sub(1), ra.index.checked_sub(1)) else {
                return RefExpr::Error(ErrorKind::Ref);
            };
            RectRange::new(0, MAX_ROW0, c0.min(c1), c0.max(c1))
        }
        (RefKind::Row(la), RefKind::Row(ra)) => {
            let (Some(r0), Some(r1)) = (la.index.checked_sub(1), ra.index.checked_sub(1)) else {
                return RefExpr::Error(ErrorKind::Ref);
            };
            RectRange::new(r0.min(r1), r0.max(r1), 0, MAX_COL0)
        }
        // R1C1 range endpoints (or a mixed shape) → deferred.
        _ => {
            diags.push((
                DiagnosticKind::UnsupportedConstruct,
                "INDIRECT R1C1 range or mixed-shape endpoints are unsupported (OXP-142)"
                    .to_string(),
            ));
            return RefExpr::Unsupported;
        }
    };
    RefExpr::Range(SheetRange::new(sheet, rect))
}
