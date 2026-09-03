//! The formula interpreter: evaluate a parsed [`Expr`] against the current cell
//! value store, dispatching function calls through `xl-fn`'s registry.
//!
//! # What evaluates vs `#UNSUPPORTED!`
//! Supported: literals; single-cell references; `SUM`/`IF` (via the registry)
//! and their arguments (including multi-cell ranges and array constants streamed
//! per element); the scalar operators; parentheses; a 1×1 range/array lifted to
//! its scalar. A multi-cell **range** reaching scalar context in a
//! **non-array-entered** formula undergoes Excel's *legacy implicit
//! intersection* — a single-row/single-column range reduces to the cell at the
//! formula's own row/column, or `#VALUE!` when that row/column falls outside the
//! range (OXP-004/163; see [`implicit_intersect`]). In an **array-entered**
//! (legacy CSE `<f t="array">`) formula the same range is **not** intersected —
//! only a 1×1 lifts; a larger range stays `#UNSUPPORTED!` (v1 has no
//! dynamic-array spill in a *static* range). **M2 lane 4** adds dynamic-array
//! spill: a top-level `Value::Array` result spills (see
//! [`crate::Engine::write_cell_result`]); the spilled-range postfix `A1#`
//! (`_xlfn.ANCHORARRAY`) resolves through the reference seam
//! ([`crate::refx::resolve_ref_expr`]); and `@X` implicit intersection over a
//! **computed** array yields its top-left element (OXP-201). Everything else is
//! refused rather than guessed and recorded as a diagnostic: unknown functions,
//! reference union/intersection, a static-range implicit-intersection `@A1:A5`, a
//! *2-D* range (both axes > 1) or a multi-element array literal in scalar
//! context, a multi-cell *computed* (`OFFSET`/`INDIRECT`) or bare multi-cell
//! spilled (`A1#`) reference in scalar context, 3-D and R1C1 references,
//! non-simple defined names, and any `xl-ast` [`ExprKind::Unsupported`] node.
//!
//! # No ambient authority
//! Evaluation reads only the passed-in value store and the shared
//! [`EvalContext`] (the clock/RNG seam, `#UNSUPPORTED!` in v0). It never touches
//! the wall clock, the filesystem, or the network.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use xl_ast::{BinaryOp, Expr, ExprKind, PostfixOp};
use xl_fn::{
    ArgShape, CallArgs, CellFlags, EvalContext, RefRect, apply_binary, apply_postfix, apply_unary,
    broadcast_binary, broadcast_postfix, broadcast_unary,
};
use xl_graph::{CellId, SheetRange};
use xl_value::{Array, ErrorKind, SheetId, Value};

use crate::analyze::{
    MAX_COL0, MAX_ROW0, RefResolution, WbIndex, rect_is_single, resolve_name, resolve_range,
    resolve_reference,
};
use crate::lambda::{
    LexEnv, LexFrame, as_xlpm_param, canon_name, downcast_closure, lex_lookup, make_lambda,
    snapshot,
};
use crate::refx::{RefExpr, is_ref_expr, is_ref_returning, resolve_ref_expr};
use crate::{DiagnosticKind, DiagnosticSink};

/// The read-only cell value store: 0-based `CellId` → its current value.
pub(crate) type ValueStore = BTreeMap<CellId, Value>;

/// Evaluate `expr` in scalar context, producing a single [`Value`].
///
/// `anchor` is the cell whose formula is being evaluated (its sheet equals
/// `cur`); it is threaded unchanged so a relative/partial R1C1 target inside
/// `INDIRECT(...,FALSE)` can resolve against the containing cell (OXP-142).
///
/// Diagnostics for refused constructs are pushed onto `diags` (only for paths
/// actually evaluated — laziness means an unselected `IF` branch adds none).
#[allow(clippy::too_many_arguments)] // the immutable eval seam + RFC-0011 array_arg_ctx
pub(crate) fn eval_expr(
    expr: &Expr,
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    // RFC-0011: `true` while evaluating an **array-context** aggregator argument
    // (set by `EngineArgs::eval_scalar_array_arg`, inherited through nested
    // calls/operators). When set, a multi-cell range reaching scalar context
    // returns loud `#UNSUPPORTED!` instead of silently implicit-intersecting —
    // Excel array-evaluates there (M2); recalc refuses loudly (M1).
    array_arg_ctx: bool,
    // M2 lane 2: the lexical binding environment for the LET/LAMBDA interpreter.
    // `None` for an ordinary formula; a chain of frames inside a `LET`/`LAMBDA`
    // body. Threaded read-only; see [`crate::lambda`].
    lex: LexEnv<'_>,
) -> Value {
    match &expr.kind {
        ExprKind::Number(n) => Value::number(*n),
        ExprKind::Text(s) => Value::text(s),
        ExprKind::Bool(b) => Value::Bool(*b),
        ExprKind::Error(k) => Value::Error(*k),
        // A bare missing slot outside a call reads as blank.
        ExprKind::Missing => Value::Blank,
        ExprKind::Ref(r) => match resolve_reference(r, cur, env) {
            RefResolution::Cell(c) => values.get(&c).cloned().unwrap_or(Value::Blank),
            RefResolution::Unsupported => {
                diags.push((
                    DiagnosticKind::UnsupportedConstruct,
                    format!("unsupported reference: {r}"),
                ));
                Value::Error(ErrorKind::Unsupported)
            }
        },
        ExprKind::Name(n) => {
            // M2 lane 2: a LET/LAMBDA-bound parameter is written `_xlpm.x` in the
            // body and resolves against the lexical env. ONLY an `_xlpm.`-prefixed
            // reference does so — a bare defined name is never captured by a
            // same-spelled binding (Excel's stored form distinguishes them).
            // Parameters are never sheet-qualified.
            if n.sheet.is_none()
                && let Some(key) = as_xlpm_param(&n.name)
                && let Some(v) = lex_lookup(lex, &key)
            {
                v.clone()
            } else {
                match resolve_name(n, cur, env) {
                    Some(resolved) => eval_expr(
                        &resolved,
                        anchor,
                        cur,
                        values,
                        env,
                        ctx,
                        diags,
                        array_arg_ctx,
                        lex,
                    ),
                    None => {
                        diags.push((
                            DiagnosticKind::UnsupportedConstruct,
                            format!("unsupported defined name: {n}"),
                        ));
                        Value::Error(ErrorKind::Unsupported)
                    }
                }
            }
        }
        ExprKind::Unary { op, expr } => {
            let v = eval_expr(
                expr,
                anchor,
                cur,
                values,
                env,
                ctx,
                diags,
                array_arg_ctx,
                lex,
            );
            // Consumed-array broadcast (M2 lane 6): gated on `array_arg_ctx`, NOT
            // on "operand is an array" — a bare `Value::Array` reaching an
            // operator at top level (e.g. `INDEX(A:A,0,1)`) must still refuse
            // (Principle 2). Under array context the array may be a materialized
            // consumed range OR any function-produced / literal array (e.g.
            // `INDEX(range,0,1)`, `{1;2;3}`); all fold through the OXP-201-pinned
            // broadcast composition, so the direction stays monotone and pinned.
            if array_arg_ctx && matches!(v, Value::Array(_)) {
                broadcast_unary(*op, &v)
            } else {
                apply_unary(*op, &v)
            }
        }
        // M2 lane 4: the spilled-range postfix `A1#` is a *reference* (like
        // `OFFSET`/`INDIRECT`), not a value-carrying postfix: resolve it through
        // the ref seam and dereference to a scalar. A 1×1 anchor (OXP-204) lifts
        // to its cell; a multi-cell region reaching scalar context is a bare-spill
        // re-spill situation that stays a loud `#UNSUPPORTED!` (unpinned by any
        // OXP — Principle 2). The streaming seams (`SUM(A1#)`, `ROWS(A1#)`, …)
        // handle multi-cell regions via [`is_ref_expr`].
        ExprKind::Postfix {
            op: PostfixOp::SpillRange,
            ..
        } => deref_ref_expr_scalar(expr, anchor, cur, values, env, ctx, diags),
        ExprKind::Postfix { op, expr } => {
            let v = eval_expr(
                expr,
                anchor,
                cur,
                values,
                env,
                ctx,
                diags,
                array_arg_ctx,
                lex,
            );
            if array_arg_ctx && matches!(v, Value::Array(_)) {
                broadcast_postfix(*op, &v)
            } else {
                apply_postfix(*op, &v)
            }
        }
        // A range in scalar context. In a **non-array** formula this is Excel's
        // *legacy implicit intersection* (OXP-004/163, RUN-2026-07-11-oracle01):
        // reduce the range to the single cell at the intersection of the range
        // and the FORMULA's own row/column (see [`implicit_intersect`]). In an
        // **array-entered** formula (`env.is_array_formula`) the range is NOT
        // intersected — only a 1×1 lifts to its cell; anything larger is a spill
        // situation (out of scope, v1) → `#UNSUPPORTED!`. This is the sole seam
        // where array vs scalar context changes a literal range's meaning.
        ExprKind::Binary {
            op: BinaryOp::Range,
            lhs,
            rhs,
        } => match resolve_range(lhs, rhs, cur, env) {
            Some(sr) if env.is_array_formula => {
                // Array context: no implicit intersection (pre-OXP-163 behavior).
                if rect_is_single(&sr.range) {
                    single_cell_value(&sr, values)
                } else {
                    diags.push((
                        DiagnosticKind::UnsupportedConstruct,
                        "multi-cell range in scalar context of an array formula is unsupported"
                            .to_string(),
                    ));
                    Value::Error(ErrorKind::Unsupported)
                }
            }
            // M2 lane 6 (RFC-0011 residual, OXP-201): inside an array-context
            // aggregator argument, a **bounded** MULTI-CELL range reaching scalar
            // context is where Excel array-evaluates (`SUM(IF(range,…))`).
            // Materialize it into a dense row-major `Value::Array` so the
            // consumed-array evaluator (broadcasting operators + IF/ISBLANK/NOT +
            // SUM fold) can reduce it. A **whole-column/row (unbounded)** range
            // (or an over-cap dense allocation) still refuses LOUDLY — see
            // `materialize_range_as_array`. A 1×1 range still lifts to its cell
            // (falls through to the intersect arm below).
            Some(sr) if array_arg_ctx && !rect_is_single(&sr.range) => {
                materialize_range_as_array(&sr, values, diags)
            }
            Some(sr) => implicit_intersect(&sr, anchor, values, diags),
            None => {
                diags.push((
                    DiagnosticKind::UnsupportedConstruct,
                    "unresolvable range in scalar context is unsupported".to_string(),
                ));
                Value::Error(ErrorKind::Unsupported)
            }
        },
        ExprKind::Binary {
            op: BinaryOp::Intersect | BinaryOp::Union,
            ..
        } => {
            diags.push((
                DiagnosticKind::UnsupportedConstruct,
                "reference intersection/union is unsupported".to_string(),
            ));
            Value::Error(ErrorKind::Unsupported)
        }
        ExprKind::Binary { op, lhs, rhs } => {
            let l = eval_expr(
                lhs,
                anchor,
                cur,
                values,
                env,
                ctx,
                diags,
                array_arg_ctx,
                lex,
            );
            let r = eval_expr(
                rhs,
                anchor,
                cur,
                values,
                env,
                ctx,
                diags,
                array_arg_ctx,
                lex,
            );
            // Consumed-array broadcast (M2 lane 6, OXP-201): gated on
            // `array_arg_ctx`, NOT on "an operand is an array". At top level a
            // bare `Value::Array` (e.g. `INDEX(A:A,0,1)+1`) must still refuse via
            // `apply_binary`'s `to_number(Array)` → `#UNSUPPORTED!` (Principle 2);
            // an array reaching here under array context may be a materialized
            // consumed range OR a function-produced / literal array (e.g.
            // `INDEX(range,0,1)+1`, `{1;2}*2`) — all fold through the
            // OXP-201-pinned composition (monotone, pinned direction).
            if array_arg_ctx && (matches!(l, Value::Array(_)) || matches!(r, Value::Array(_))) {
                broadcast_binary(*op, &l, &r)
            } else {
                apply_binary(*op, &l, &r)
            }
        }
        // OFFSET/INDIRECT return a *reference* (RFC 0003, approach A2): in scalar
        // context resolve the reference, then dereference it to the single cell's
        // value. A multi-cell reference in scalar context is a spill/implicit-
        // intersection situation (out of scope) — mirror the `Binary::Range` arm.
        ExprKind::Call { name, .. } if is_ref_returning(&name.canonical) => {
            match resolve_ref_expr(expr, anchor, cur, values, env, ctx, diags, 0) {
                RefExpr::Range(sr) if rect_is_single(&sr.range) => single_cell_value(&sr, values),
                RefExpr::Range(_) => {
                    diags.push((
                        DiagnosticKind::UnsupportedConstruct,
                        "multi-cell reference (OFFSET/INDIRECT) in scalar context is unsupported"
                            .to_string(),
                    ));
                    Value::Error(ErrorKind::Unsupported)
                }
                RefExpr::Error(k) => Value::Error(k),
                // `resolve_ref_expr` already pushed the refusal diagnostic.
                RefExpr::Unsupported => Value::Error(ErrorKind::Unsupported),
            }
        }
        ExprKind::Call { name, args } => eval_call(
            name,
            args,
            anchor,
            cur,
            values,
            env,
            ctx,
            diags,
            array_arg_ctx,
            lex,
        ),
        ExprKind::Array(rows) => {
            // 1×1 array lifts to its element; larger arrays are a spill
            // situation (out of scope) in scalar context.
            if let [row] = rows.as_slice()
                && let [only] = row.as_slice()
            {
                return eval_expr(
                    only,
                    anchor,
                    cur,
                    values,
                    env,
                    ctx,
                    diags,
                    array_arg_ctx,
                    lex,
                );
            }
            diags.push((
                DiagnosticKind::UnsupportedConstruct,
                "multi-element array literal in scalar context is unsupported".to_string(),
            ));
            Value::Error(ErrorKind::Unsupported)
        }
        ExprKind::Paren(inner) => eval_expr(
            inner,
            anchor,
            cur,
            values,
            env,
            ctx,
            diags,
            array_arg_ctx,
            lex,
        ),
        // `@X` implicit intersection. Two cases, both pinned, routed by evaluating
        // the operand in **scalar** context (`array_arg_ctx = false`) — critically
        // NOT the enclosing context (review B3): otherwise inside an array-arg
        // consumer like `SUM(@A1:A3)` the inner static range would materialize as a
        // `Value::Array` and the `get(0,0)` arm below would silently return the
        // top-left (`A1`) instead of the row/column intersection.
        //   * A **static range** `@A1:A5`: scalar context routes it through the
        //     legacy `implicit_intersect` table, which returns the element on the
        //     formula's own row/column (the geometry pinned by OXP-163/169/195) as
        //     a scalar → the `other => other` arm. This is Excel's documented `@`
        //     semantics, not a refusal (a farm probe pinning the `@`-spelling
        //     explicitly is a recommended follow-up, but the geometry it lands on
        //     is already pinned).
        //   * A **computed array** (`MAP`/`MAKEARRAY`/… or a 1×1 anchor, OXP-201:
        //     `@SEQUENCE(3)` = 1) is produced anchored at the formula's own cell,
        //     so its intersection with the formula's row/column is always the
        //     top-left element (array origin coincides with the anchor) → the
        //     `Value::Array` arm. A scalar operand is returned unchanged (`@x` =
        //     `x`); an error propagates.
        ExprKind::ImplicitIntersection(inner) => {
            let v = eval_expr(inner, anchor, cur, values, env, ctx, diags, false, lex);
            match v {
                Value::Array(a) => a.get(0, 0).cloned().unwrap_or(Value::Blank),
                other => other,
            }
        }
        ExprKind::Unsupported { reason, .. } => {
            diags.push((
                DiagnosticKind::UnsupportedConstruct,
                format!("unsupported construct: {reason}"),
            ));
            Value::Error(ErrorKind::Unsupported)
        }
    }
}

/// Read the single cell a 1×1 [`SheetRange`] covers.
fn single_cell_value(sr: &SheetRange, values: &ValueStore) -> Value {
    let c = CellId::new(sr.sheet, sr.range.row_start, sr.range.col_start);
    values.get(&c).cloned().unwrap_or(Value::Blank)
}

/// Resolve a **reference-position** expression in scalar context and dereference
/// it: a 1×1 result lifts to its cell's value; a multi-cell result reaching
/// scalar context is refused loudly (never a silent implicit intersection).
///
/// Used for the M2 lane-4 spilled-range postfix `A1#`: a 1×1 dynamic-array
/// anchor (OXP-204) dereferences to its single value, while a bare multi-cell
/// `=A1#` in scalar position (which would *re-spill* in Excel — a behavior no OXP
/// pins yet) stays `#UNSUPPORTED!` rather than guess. The streaming aggregators
/// (`SUM(A1#)`) never route here; they consume the whole region.
fn deref_ref_expr_scalar(
    expr: &Expr,
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
) -> Value {
    match resolve_ref_expr(expr, anchor, cur, values, env, ctx, diags, 0) {
        RefExpr::Range(sr) if rect_is_single(&sr.range) => single_cell_value(&sr, values),
        RefExpr::Range(_) => {
            diags.push((
                DiagnosticKind::UnsupportedConstruct,
                "multi-cell spilled-range reference (A1#) in scalar context re-spills \
                 in Excel — unpinned by any OXP, so refused loudly (M2 lane 4)"
                    .to_string(),
            ));
            Value::Error(ErrorKind::Unsupported)
        }
        RefExpr::Error(k) => Value::Error(k),
        RefExpr::Unsupported => Value::Error(ErrorKind::Unsupported),
    }
}

/// Defensive cap on a materialized consumed array's element count (M2 lane 6,
/// spec §R6). `bounded_dims` already excludes whole-column/row ranges; real FUSE
/// ranges are ~5 cells. A pathologically tall *bounded* range (`B1:B2000000`)
/// would otherwise densely allocate millions of `Value`s — refuse it loudly
/// (`#UNSUPPORTED!`) rather than risk OOM, consistent with the whole-column
/// refuse policy. Costs no fidelity on real inputs.
const MAX_MATERIALIZED_ELEMS: u64 = 1 << 20;

/// Materialize a **bounded** multi-cell range into a dense row-major
/// [`Value::Array`] (absent cells → [`Value::Blank`]) — the M2 lane-6
/// consumed-array *producer* for the RFC-0011 array-context gate (OXP-201).
///
/// Refuses **loudly** (`#UNSUPPORTED!` + diagnostic, so nothing silently changes
/// for the previously-refused idioms) when the range is:
/// - **unbounded** — a whole-column/row range ([`bounded_dims`] returns `None`),
///   so a legacy whole-column `MAX/MIN(IF(…))` idiom stays refused; or
/// - **over the element cap** ([`MAX_MATERIALIZED_ELEMS`]).
///
/// Caller guards on `array_arg_ctx && !rect_is_single`, so this only ever runs
/// for a genuine consumed multi-cell range under array context.
fn materialize_range_as_array(
    sr: &SheetRange,
    values: &ValueStore,
    diags: &mut DiagnosticSink,
) -> Value {
    let Some((rows, cols)) = bounded_dims(sr) else {
        diags.push((
            DiagnosticKind::UnsupportedConstruct,
            "whole-column/row range in an array-context aggregator argument needs \
             array evaluation — unsupported in v1 (RFC-0011 / M2 lane 6)"
                .to_string(),
        ));
        return Value::Error(ErrorKind::Unsupported);
    };
    let n = u64::from(rows) * u64::from(cols);
    if n > MAX_MATERIALIZED_ELEMS {
        diags.push((
            DiagnosticKind::UnsupportedConstruct,
            "consumed range exceeds the array-materialization element cap — \
             unsupported (M2 lane 6)"
                .to_string(),
        ));
        return Value::Error(ErrorKind::Unsupported);
    }
    let rect = sr.range;
    let mut data = Vec::with_capacity(n as usize);
    for row in rect.row_start..=rect.row_end {
        for col in rect.col_start..=rect.col_end {
            let cell = CellId::new(sr.sheet, row, col);
            data.push(values.get(&cell).cloned().unwrap_or(Value::Blank));
        }
    }
    match Array::new(rows as usize, cols as usize, data) {
        Ok(a) => Value::Array(a),
        // Unreachable: dims are bounded and match `data.len()` by construction.
        Err(_) => Value::Error(ErrorKind::Unsupported),
    }
}

/// Reduce a range reaching **scalar context** in a **non-array** formula to a
/// single value via Excel's *legacy implicit intersection* (OXP-004/163,
/// RUN-2026-07-11-oracle01).
///
/// When a multi-cell range is used where a single value is expected — a scalar
/// operand of an operator (`A1:A3+1`, a comparison) or a scalar function
/// argument — Excel intersects the range with the **formula's own row/column**
/// (the `anchor`) and uses that one cell:
///
/// - **1×1** — already scalar: lift to its cell.
/// - **Single column** (width 1, height > 1) — intersect on the formula's
///   *row*: the cell at `anchor.row` if it lies within the range's
///   `[row_start, row_end]`, else `#VALUE!` (the formula's row is outside the
///   range's rows). Farm-pinned: `A1:A3+1` at row 1/2/3 → 11/21/31, at row 10 →
///   `#VALUE!` (RUN-2026-07-11-oracle01, OXP-163).
/// - **Single row** (height 1, width > 1) — the symmetric transpose: intersect
///   on the formula's *column*; outside the range's columns → `#VALUE!`.
/// - **2-D** (both axes > 1) — which axis wins is **not** pinned by the OXP-163
///   probe, so this defers to `#UNSUPPORTED!` rather than guess (the Recalc design rules
///   Principle 2).
///
/// The chosen cell is read from the *range's* sheet (`sr.sheet`) at the
/// formula's row/column — the documented symmetric rule intersects by the
/// formula's position regardless of the range's sheet. This is only ever
/// called for a **non-array-entered** formula (the caller guards on
/// `!env.is_array_formula`); an array-entered formula's range keeps its
/// element-wise/`#UNSUPPORTED!` meaning instead. A multi-cell *computed*
/// (`OFFSET`/`INDIRECT`) reference in scalar context stays deferred (a separate
/// arm); but any **static** range reaching this arm intersects — whether written
/// as a literal `A1:A3` **or** resolved from a defined name (`=MyName+1`).
///
/// **PINNED (OXP-163 + OXP-169 + OXP-195, RUN-2026-07-13).** Every 1-D reduction
/// is oracle-verified across **both axes and all four contexts** — the axis
/// matches Excel and an off-band formula position gives `#VALUE!`. The pins are
/// context-invariant, which is expected: this function receives only
/// `(sr, anchor)`, so it *cannot* see whether the range arrived via an operator,
/// a scalar function argument, a defined name, or a cross-sheet reference.
/// - single-**column** → intersect the formula **row**: operator `=A1:A3+1`
///   (OXP-163); scalar-fn-arg `=ABS(A1:A3)`, named `=MyName+1`, cross-sheet
///   `=Sheet2!A1:A3+1` (OXP-169: H2→20, J2→21, L2→2001; off-band → `#VALUE!`).
/// - single-**row** → intersect the formula **column**: operator `=A6:C6+1`
///   (OXP-169: A8→101, B8→201, C8→301); scalar-fn-arg `=ABS(A6:C6)`, named
///   `=RowName+1`, cross-sheet `=Data!A6:C6+1` (OXP-195: A/C in-band intersect,
///   off-band col → `#VALUE!`).
///
/// OXP-167 had shown `ROW`/`COLUMN` take the *top/left* cell rather than
/// intersecting, so function-argument context was a genuine open question —
/// OXP-169/195 settled it: a **value** operation (`ABS`, `+1`) intersects,
/// unlike the address functions.
///
/// Only the 2-D case (both axes > 1) stays deferred (`#UNSUPPORTED!`, its own
/// arm) — unprobed, so the intersection axis there is still unpinned and must
/// remain loud.
fn implicit_intersect(
    sr: &SheetRange,
    anchor: CellId,
    values: &ValueStore,
    diags: &mut DiagnosticSink,
) -> Value {
    let rect = sr.range;
    let single_row = rect.row_start == rect.row_end;
    let single_col = rect.col_start == rect.col_end;
    match (single_row, single_col) {
        // 1×1: already a scalar cell.
        (true, true) => single_cell_value(sr, values),
        // Single column, multiple rows: intersect on the formula's ROW.
        (false, true) => {
            if (rect.row_start..=rect.row_end).contains(&anchor.row) {
                let cell = CellId::new(sr.sheet, anchor.row, rect.col_start);
                values.get(&cell).cloned().unwrap_or(Value::Blank)
            } else {
                Value::Error(ErrorKind::Value)
            }
        }
        // Single row, multiple columns: intersect on the formula's COLUMN.
        (true, false) => {
            if (rect.col_start..=rect.col_end).contains(&anchor.col) {
                let cell = CellId::new(sr.sheet, rect.row_start, anchor.col);
                values.get(&cell).cloned().unwrap_or(Value::Blank)
            } else {
                Value::Error(ErrorKind::Value)
            }
        }
        // 2-D range (both axes > 1): the intersection axis is unprobed → defer.
        (false, false) => {
            diags.push((
                DiagnosticKind::UnsupportedConstruct,
                "2-D range in scalar context (implicit-intersection axis unprobed) is unsupported"
                    .to_string(),
            ));
            Value::Error(ErrorKind::Unsupported)
        }
    }
}

/// Dispatch a function call through the registry.
#[allow(clippy::too_many_arguments)] // the immutable eval seam: anchor + cur + store + env + ctx + diags
fn eval_call(
    name: &xl_ast::FuncName,
    args: &[Expr],
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    // RFC-0011: inherited into the inner `EngineArgs` so array context flows
    // through nested calls (e.g. `SUM(IF(NOT(ISBLANK(range)),…))`).
    array_arg_ctx: bool,
    // M2 lane 2: the lexical environment (LET/LAMBDA bindings) in force.
    lex: LexEnv<'_>,
) -> Value {
    // M2 lane 2: `LET`/`LAMBDA` and the higher-order lambda functions are
    // SPECIAL FORMS — they need the evaluator itself (lazy binding for `LET`,
    // lambda application for `MAP`/`REDUCE`/…), which `xl-fn` (a layer *below*
    // `xl-engine`) cannot call back into. Dispatch them before the registry
    // lookup. Semantics pinned by OXP-200/201/207.
    if let Some(v) = eval_special_form(
        &name.canonical,
        args,
        anchor,
        cur,
        values,
        env,
        ctx,
        diags,
        lex,
    ) {
        return v;
    }
    // Named-lambda application: `_xlpm.f(args)` where `f` is a LET/LAMBDA-bound
    // name (the call name arrives as `_XLPM.F`). ONLY an `_xlpm.`-prefixed call
    // resolves against `lex`; a builtin like `SUM(3)` is NOT `_xlpm.`-prefixed
    // and so is never hijacked by a same-spelled `LET` binding (falls through to
    // the registry). A bound lambda is applied; a bound non-lambda called like a
    // function refuses loudly (`#VALUE!`) — never silently wrong.
    if let Some(key) = as_xlpm_param(&name.canonical)
        && let Some(bound) = lex_lookup(lex, &key)
    {
        return match bound {
            Value::Lambda(l) => {
                let l = l.clone();
                let argv: Vec<Value> = args
                    .iter()
                    .map(|a| eval_expr(a, anchor, cur, values, env, ctx, diags, false, lex))
                    .collect();
                apply_lambda(&l, &argv, anchor, cur, values, env, ctx, diags)
            }
            // A bound ERROR invoked like a function propagates (ordinary
            // error propagation; in particular a refusal sentinel such as
            // `#UNSUPPORTED!` must propagate distinctly, never be downgraded
            // to a plausible computed error — RFC-0012 §4 composition seam).
            Value::Error(k) => Value::Error(*k),
            // A bound non-lambda, non-error value called like a function
            // refuses loudly — never silently wrong.
            _ => Value::Error(ErrorKind::Value),
        };
    }
    // Sandbox boundary: these Excel functions require a network or live data
    // provider. The engine must never attempt the call or turn it into a
    // generic unknown-function result: the stable sentinel is `#BLOCKED!` and
    // the diagnostic makes the refusal visible to policy/report consumers.
    if matches!(
        name.canonical.as_str(),
        "WEBSERVICE" | "RTD" | "STOCKHISTORY"
    ) {
        diags.push((
            DiagnosticKind::UnsupportedConstruct,
            format!(
                "blocked external-data function {}: network/live-provider access is disabled",
                name.canonical
            ),
        ));
        return Value::Error(ErrorKind::Blocked);
    }
    let Some(spec) = xl_fn::lookup(&name.canonical) else {
        diags.push((
            DiagnosticKind::UnknownFunction,
            format!("unsupported function: {}", name.canonical),
        ));
        return Value::Error(ErrorKind::Unsupported);
    };
    if !spec.accepts_arity(args.len()) {
        diags.push((
            DiagnosticKind::ArityError,
            format!(
                "function {} called with {} argument(s), outside its arity",
                name.canonical,
                args.len()
            ),
        ));
        return Value::Error(ErrorKind::Unsupported);
    }
    let before = diags.len();
    let mut call_args = EngineArgs {
        args,
        anchor,
        cur,
        values,
        env,
        ctx,
        diags,
        array_arg_ctx,
        lex,
        consumed_array_seen: false,
        consumed_array_had_refusal: false,
    };
    let v = (spec.eval)(ctx, &mut call_args);
    let consumed_array_seen = call_args.consumed_array_seen;
    let consumed_array_had_refusal = call_args.consumed_array_had_refusal;
    // Born-refusing boundary diagnostic (M2 lane 6, spec §4): the function
    // received a materialized multi-cell array in a scalar-shaped argument and
    // answered `#UNSUPPORTED!` without a diagnostic of its own (`xl-fn` has no
    // diagnostic channel; its scalar coercion refused the array). The value was
    // already loud — this names the function in the fidelity report.
    if consumed_array_seen
        && !consumed_array_had_refusal
        && matches!(v, Value::Error(ErrorKind::Unsupported))
        && diags.len() == before
    {
        diags.push((
            DiagnosticKind::UnsupportedConstruct,
            format!(
                "function {} received a materialized multi-cell array argument and \
                 refused it: the array shape or element semantics involved are not \
                 oracle-pinned for this call, so the result is #UNSUPPORTED! rather \
                 than an implicit intersection or a guess",
                name.canonical
            ),
        ));
    }
    v
}

// ===========================================================================
// M2 lane 2 — the LET/LAMBDA interpreter (special forms + lambda application).
//
// LET/LAMBDA and the higher-order lambda functions live here rather than in
// `xl-fn` because they need the evaluator itself: `LET` binds lazily and
// evaluates a body; `MAP`/`REDUCE`/`SCAN`/`BYROW`/`BYCOL`/`MAKEARRAY` apply a
// lambda per element. `xl-fn` sits *below* `xl-engine` and cannot call back into
// `eval_expr`, so these are special forms intercepted in `eval_call` before the
// registry lookup. Semantics are oracle-pinned: OXP-200 (LET scoping / closure
// capture / arity → `#VALUE!` / bare lambda → `#CALC!`), OXP-201 (broadcasting),
// OXP-207 (MAP element-wise + positional zip, SCAN output shape, BYROW→column /
// BYCOL→row vectors, MAKEARRAY 1-based indices). Every unpinned edge refuses
// loudly (Principle 2).
// ===========================================================================

/// Dispatch a M2 lane-2 special form by canonical name. Returns `None` if the
/// name is not a special form (the caller then tries named-lambda application,
/// then the registry).
#[allow(clippy::too_many_arguments)]
fn eval_special_form(
    canonical: &str,
    args: &[Expr],
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    lex: LexEnv<'_>,
) -> Option<Value> {
    let v = match canonical {
        "LET" => {
            // Backstop for the load-level duplicate-LET rejection (OXP-200:
            // Excel load-rejects, so every cell program is screened at compile
            // time — see `duplicate_let_rejection` in lib.rs). A duplicate can
            // only reach eval through a path that skipped compilation screening
            // (e.g. a defined-name body); refuse loudly rather than silently
            // shadow.
            if let Some(dup) = crate::lambda::duplicate_let_binding(args) {
                diags.push((
                    DiagnosticKind::UnsupportedConstruct,
                    format!(
                        "duplicate LET parameter `{dup}`: Excel load-rejects this \
                         (OXP-200); refusing rather than shadow"
                    ),
                ));
                Value::Error(ErrorKind::Unsupported)
            } else {
                eval_let(args, anchor, cur, values, env, ctx, diags, lex)
            }
        }
        "LAMBDA" => eval_lambda_literal(args, diags, lex),
        "MAP" => eval_map(args, anchor, cur, values, env, ctx, diags, lex),
        "REDUCE" => eval_reduce(args, anchor, cur, values, env, ctx, diags, lex),
        "SCAN" => eval_scan(args, anchor, cur, values, env, ctx, diags, lex),
        "BYROW" => eval_byrow(args, anchor, cur, values, env, ctx, diags, lex),
        "BYCOL" => eval_bycol(args, anchor, cur, values, env, ctx, diags, lex),
        "MAKEARRAY" => eval_makearray(args, anchor, cur, values, env, ctx, diags, lex),
        "ISOMITTED" => {
            // Not yet oracle-pinned (needs a probe alongside optional-argument
            // LAMBDA semantics — proposed OXP-208). Refuse loudly, never guess.
            diags.push((
                DiagnosticKind::UnsupportedConstruct,
                "ISOMITTED is not yet oracle-pinned (M2 lane 2 defers it)".to_string(),
            ));
            Value::Error(ErrorKind::Unsupported)
        }
        _ => return None,
    };
    Some(v)
}

/// Apply a lambda value to already-evaluated argument values. Arity mismatch is
/// `#VALUE!` (OXP-200 over/under-supply). The body evaluates with the closure's
/// captured environment plus the parameter bindings and an **empty** parent —
/// scope is purely lexical (see [`crate::lambda`]).
#[allow(clippy::too_many_arguments)]
fn apply_lambda(
    lambda: &xl_value::Lambda,
    argv: &[Value],
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
) -> Value {
    let Some(clo) = downcast_closure(lambda) else {
        // A foreign `LambdaObj` (not the engine's `Closure`) — refuse loudly.
        return Value::Error(ErrorKind::Unsupported);
    };
    if argv.len() != clo.params.len() {
        return Value::Error(ErrorKind::Value);
    }
    let mut binds: Vec<(&str, &Value)> = Vec::with_capacity(clo.captured.len() + clo.params.len());
    for (n, v) in &clo.captured {
        binds.push((n.as_str(), v));
    }
    for (p, v) in clo.params.iter().zip(argv) {
        binds.push((p.as_str(), v));
    }
    eval_with_bindings(
        &binds, None, &clo.body, anchor, cur, values, env, ctx, diags,
    )
}

/// Evaluate `body` with `binds` pushed onto `parent` (outermost first, so the
/// last binding is innermost and shadows). Builds the frame chain on the stack.
#[allow(clippy::too_many_arguments)]
fn eval_with_bindings(
    binds: &[(&str, &Value)],
    parent: LexEnv<'_>,
    body: &Expr,
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
) -> Value {
    match binds.split_first() {
        None => eval_expr(body, anchor, cur, values, env, ctx, diags, false, parent),
        Some((&(name, value), rest)) => {
            let frame = LexFrame {
                name,
                value,
                parent,
            };
            eval_with_bindings(
                rest,
                Some(&frame),
                body,
                anchor,
                cur,
                values,
                env,
                ctx,
                diags,
            )
        }
    }
}

/// `LET(name1, value1, [name2, value2, …], calculation)` — bind names
/// sequentially (a later value may reference an earlier binding — OXP-200) then
/// evaluate the final calculation in the accumulated environment.
#[allow(clippy::too_many_arguments)]
fn eval_let(
    args: &[Expr],
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    lex: LexEnv<'_>,
) -> Value {
    match args {
        // Final calculation, evaluated in the accumulated environment.
        [calc] => eval_expr(calc, anchor, cur, values, env, ctx, diags, false, lex),
        // A (name, value) binding, then the rest.
        [name_expr, value_expr, rest @ ..] => {
            let ExprKind::Name(nr) = &peel_paren(name_expr).kind else {
                // A non-name binding slot — malformed (Excel load-rejects a
                // duplicate/invalid LET name, OXP-200); refuse loudly if seen.
                return Value::Error(ErrorKind::Value);
            };
            let name = canon_name(&nr.name);
            let v = eval_expr(value_expr, anchor, cur, values, env, ctx, diags, false, lex);
            let frame = LexFrame {
                name: &name,
                value: &v,
                parent: lex,
            };
            eval_let(rest, anchor, cur, values, env, ctx, diags, Some(&frame))
        }
        // An even argument count with no final calculation — malformed.
        _ => Value::Error(ErrorKind::Value),
    }
}

/// `LAMBDA([param1, param2, …], body)` — evaluate to a lambda *value* capturing
/// the current lexical environment (OXP-200 closure capture). The final argument
/// is the body; every preceding argument is a parameter declaration (a
/// `_xlpm.`-prefixed `Name` node).
fn eval_lambda_literal(args: &[Expr], diags: &mut DiagnosticSink, lex: LexEnv<'_>) -> Value {
    let Some((body, param_exprs)) = args.split_last() else {
        diags.push((
            DiagnosticKind::ArityError,
            "LAMBDA requires a body argument".to_string(),
        ));
        return Value::Error(ErrorKind::Value);
    };
    let mut params = Vec::with_capacity(param_exprs.len());
    for p in param_exprs {
        let ExprKind::Name(nr) = &peel_paren(p).kind else {
            return Value::Error(ErrorKind::Value);
        };
        let name = canon_name(&nr.name);
        // A duplicate LAMBDA parameter list is NOT oracle-pinned (OXP-200
        // pinned only the duplicate-LET load rejection; Excel's open-time
        // validation of a duplicate LAMBDA parameter is unprobed). Refuse
        // loudly rather than resolve the ambiguity by silent shadowing.
        if params.contains(&name) {
            diags.push((
                DiagnosticKind::UnsupportedConstruct,
                format!(
                    "duplicate LAMBDA parameter `{name}`: Excel's behavior is \
                     not oracle-pinned (OXP-200 covers only LET); refusing"
                ),
            ));
            return Value::Error(ErrorKind::Unsupported);
        }
        params.push(name);
    }
    make_lambda(params, body.clone(), snapshot(lex))
}

/// Evaluate a higher-order function's array argument to a concrete [`Array`]. An
/// array literal is built element-wise; any other expression is evaluated in
/// **array context** (so a bounded range materializes — lane 6) with a scalar
/// result lifted to 1×1. An error result propagates as `Err`.
#[allow(clippy::too_many_arguments)]
fn eval_arg_to_array(
    e: &Expr,
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    lex: LexEnv<'_>,
) -> Result<Array, ErrorKind> {
    if let ExprKind::Array(rows) = &peel_paren(e).kind {
        let nrows = rows.len();
        let ncols = rows.first().map_or(0, Vec::len);
        if nrows == 0 || ncols == 0 || rows.iter().any(|r| r.len() != ncols) {
            return Err(ErrorKind::Value);
        }
        let mut data = Vec::with_capacity(nrows * ncols);
        for row in rows {
            for el in row {
                data.push(eval_expr(
                    el, anchor, cur, values, env, ctx, diags, false, lex,
                ));
            }
        }
        return Array::new(nrows, ncols, data).map_err(|_| ErrorKind::Value);
    }
    match eval_expr(e, anchor, cur, values, env, ctx, diags, true, lex) {
        Value::Array(a) => Ok(a),
        Value::Error(k) => Err(k),
        scalar => Ok(Array::from_scalar(scalar)),
    }
}

/// Evaluate an argument that must be a lambda value; propagate an error result,
/// and treat any non-lambda as a loud `#VALUE!`.
#[allow(clippy::too_many_arguments)]
fn eval_arg_to_lambda(
    e: &Expr,
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    lex: LexEnv<'_>,
) -> Result<xl_value::Lambda, Value> {
    match eval_expr(e, anchor, cur, values, env, ctx, diags, false, lex) {
        Value::Lambda(l) => Ok(l),
        Value::Error(k) => Err(Value::Error(k)),
        _ => Err(Value::Error(ErrorKind::Value)),
    }
}

/// `MAP(array1, [array2, …], lambda)` — apply `lambda` element-wise; with N
/// arrays it ZIPS positionally (OXP-207 A3: `SUM(MAP(seq,seq,LAMBDA(a,b,a*b)))`
/// = 1·1+2·2+3·3 = 14, not the outer-product 36). All arrays must share one
/// shape — an unequal-shape MAP is not oracle-pinned, so it refuses loudly.
#[allow(clippy::too_many_arguments)]
fn eval_map(
    args: &[Expr],
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    lex: LexEnv<'_>,
) -> Value {
    let Some((lambda_expr, array_exprs)) = args.split_last() else {
        return Value::Error(ErrorKind::Value);
    };
    if array_exprs.is_empty() {
        return Value::Error(ErrorKind::Value);
    }
    let lambda = match eval_arg_to_lambda(lambda_expr, anchor, cur, values, env, ctx, diags, lex) {
        Ok(l) => l,
        Err(e) => return e,
    };
    let mut arrays = Vec::with_capacity(array_exprs.len());
    for ae in array_exprs {
        match eval_arg_to_array(ae, anchor, cur, values, env, ctx, diags, lex) {
            Ok(a) => arrays.push(a),
            Err(k) => return Value::Error(k),
        }
    }
    let (r, c) = (arrays[0].rows(), arrays[0].cols());
    if arrays.iter().any(|a| a.rows() != r || a.cols() != c) {
        // Unequal shapes: outside the OXP-207 positional-zip pin — refuse.
        return Value::Error(ErrorKind::Unsupported);
    }
    let mut data = Vec::with_capacity(r * c);
    for row in 0..r {
        for col in 0..c {
            let argv: Vec<Value> = arrays
                .iter()
                .map(|a| a.get(row, col).cloned().unwrap_or(Value::Blank))
                .collect();
            data.push(apply_lambda(
                &lambda, &argv, anchor, cur, values, env, ctx, diags,
            ));
        }
    }
    match Array::new(r, c, data) {
        Ok(a) => Value::Array(a),
        Err(_) => Value::Error(ErrorKind::Value),
    }
}

/// `REDUCE(initial, array, lambda(acc, value))` — fold `array` (row-major)
/// through `lambda`, threading the accumulator; returns the final accumulator
/// scalar (OXP-200: `REDUCE("",SEQUENCE(3),LAMBDA(a,b,a&b))` = `"123"`).
#[allow(clippy::too_many_arguments)]
fn eval_reduce(
    args: &[Expr],
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    lex: LexEnv<'_>,
) -> Value {
    let [init_expr, array_expr, lambda_expr] = args else {
        return Value::Error(ErrorKind::Value);
    };
    let mut acc = eval_expr(init_expr, anchor, cur, values, env, ctx, diags, false, lex);
    let array = match eval_arg_to_array(array_expr, anchor, cur, values, env, ctx, diags, lex) {
        Ok(a) => a,
        Err(k) => return Value::Error(k),
    };
    let lambda = match eval_arg_to_lambda(lambda_expr, anchor, cur, values, env, ctx, diags, lex) {
        Ok(l) => l,
        Err(e) => return e,
    };
    for elem in array.iter() {
        acc = apply_lambda(
            &lambda,
            &[acc, elem.clone()],
            anchor,
            cur,
            values,
            env,
            ctx,
            diags,
        );
    }
    acc
}

/// `SCAN(initial, array, lambda(acc, value))` — like `REDUCE` but emits every
/// intermediate accumulator; the result has the SAME shape as `array` (OXP-207
/// A4/A5: element 1 = `lambda(initial, array[0])`; the initial is not a separate
/// leading element).
#[allow(clippy::too_many_arguments)]
fn eval_scan(
    args: &[Expr],
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    lex: LexEnv<'_>,
) -> Value {
    let [init_expr, array_expr, lambda_expr] = args else {
        return Value::Error(ErrorKind::Value);
    };
    let init = eval_expr(init_expr, anchor, cur, values, env, ctx, diags, false, lex);
    let array = match eval_arg_to_array(array_expr, anchor, cur, values, env, ctx, diags, lex) {
        Ok(a) => a,
        Err(k) => return Value::Error(k),
    };
    let lambda = match eval_arg_to_lambda(lambda_expr, anchor, cur, values, env, ctx, diags, lex) {
        Ok(l) => l,
        Err(e) => return e,
    };
    let (r, c) = (array.rows(), array.cols());
    let mut acc = init;
    let mut data = Vec::with_capacity(r * c);
    for elem in array.iter() {
        acc = apply_lambda(
            &lambda,
            &[acc, elem.clone()],
            anchor,
            cur,
            values,
            env,
            ctx,
            diags,
        );
        data.push(acc.clone());
    }
    match Array::new(r, c, data) {
        Ok(a) => Value::Array(a),
        Err(_) => Value::Error(ErrorKind::Value),
    }
}

/// `BYROW(array, lambda(row))` — apply `lambda` to each ROW (a 1×ncols
/// sub-array); collect the results into a COLUMN vector, nrows×1 (OXP-207 A6).
#[allow(clippy::too_many_arguments)]
fn eval_byrow(
    args: &[Expr],
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    lex: LexEnv<'_>,
) -> Value {
    let [array_expr, lambda_expr] = args else {
        return Value::Error(ErrorKind::Value);
    };
    let array = match eval_arg_to_array(array_expr, anchor, cur, values, env, ctx, diags, lex) {
        Ok(a) => a,
        Err(k) => return Value::Error(k),
    };
    let lambda = match eval_arg_to_lambda(lambda_expr, anchor, cur, values, env, ctx, diags, lex) {
        Ok(l) => l,
        Err(e) => return e,
    };
    let (r, c) = (array.rows(), array.cols());
    let mut data = Vec::with_capacity(r);
    for row in 0..r {
        let mut rowvals = Vec::with_capacity(c);
        for col in 0..c {
            rowvals.push(array.get(row, col).cloned().unwrap_or(Value::Blank));
        }
        let subarray = match Array::new(1, c, rowvals) {
            Ok(a) => Value::Array(a),
            Err(_) => return Value::Error(ErrorKind::Value),
        };
        data.push(apply_lambda(
            &lambda,
            &[subarray],
            anchor,
            cur,
            values,
            env,
            ctx,
            diags,
        ));
    }
    match Array::new(r, 1, data) {
        Ok(a) => Value::Array(a),
        Err(_) => Value::Error(ErrorKind::Value),
    }
}

/// `BYCOL(array, lambda(col))` — apply `lambda` to each COLUMN (an nrows×1
/// sub-array); collect the results into a ROW vector, 1×ncols (OXP-207 A7).
#[allow(clippy::too_many_arguments)]
fn eval_bycol(
    args: &[Expr],
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    lex: LexEnv<'_>,
) -> Value {
    let [array_expr, lambda_expr] = args else {
        return Value::Error(ErrorKind::Value);
    };
    let array = match eval_arg_to_array(array_expr, anchor, cur, values, env, ctx, diags, lex) {
        Ok(a) => a,
        Err(k) => return Value::Error(k),
    };
    let lambda = match eval_arg_to_lambda(lambda_expr, anchor, cur, values, env, ctx, diags, lex) {
        Ok(l) => l,
        Err(e) => return e,
    };
    let (r, c) = (array.rows(), array.cols());
    let mut data = Vec::with_capacity(c);
    for col in 0..c {
        let mut colvals = Vec::with_capacity(r);
        for row in 0..r {
            colvals.push(array.get(row, col).cloned().unwrap_or(Value::Blank));
        }
        let subarray = match Array::new(r, 1, colvals) {
            Ok(a) => Value::Array(a),
            Err(_) => return Value::Error(ErrorKind::Value),
        };
        data.push(apply_lambda(
            &lambda,
            &[subarray],
            anchor,
            cur,
            values,
            env,
            ctx,
            diags,
        ));
    }
    match Array::new(1, c, data) {
        Ok(a) => Value::Array(a),
        Err(_) => Value::Error(ErrorKind::Value),
    }
}

/// `MAKEARRAY(rows, cols, lambda(row, col))` — build a `rows`×`cols` array where
/// element (i, j) = `lambda(i, j)` with **1-based** indices (OXP-207 A8/A9/A10:
/// `SUM(MAKEARRAY(2,2,LAMBDA(i,j,i*10+j)))` = 11+12+21+22 = 66; 0-based would be
/// 22). A non-positive dimension is `#VALUE!`; an over-cap total refuses loudly.
#[allow(clippy::too_many_arguments)]
fn eval_makearray(
    args: &[Expr],
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    lex: LexEnv<'_>,
) -> Value {
    let [rows_expr, cols_expr, lambda_expr] = args else {
        return Value::Error(ErrorKind::Value);
    };
    let nrows = match dim_arg(rows_expr, anchor, cur, values, env, ctx, diags, lex) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let ncols = match dim_arg(cols_expr, anchor, cur, values, env, ctx, diags, lex) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let total = (nrows as u64) * (ncols as u64);
    if total > MAX_MATERIALIZED_ELEMS {
        diags.push((
            DiagnosticKind::UnsupportedConstruct,
            "MAKEARRAY exceeds the array-materialization element cap (M2 lane 2)".to_string(),
        ));
        return Value::Error(ErrorKind::Unsupported);
    }
    let lambda = match eval_arg_to_lambda(lambda_expr, anchor, cur, values, env, ctx, diags, lex) {
        Ok(l) => l,
        Err(e) => return e,
    };
    let mut data = Vec::with_capacity(nrows * ncols);
    for i in 1..=nrows {
        for j in 1..=ncols {
            let argv = [Value::number(i as f64), Value::number(j as f64)];
            data.push(apply_lambda(
                &lambda, &argv, anchor, cur, values, env, ctx, diags,
            ));
        }
    }
    match Array::new(nrows, ncols, data) {
        Ok(a) => Value::Array(a),
        Err(_) => Value::Error(ErrorKind::Value),
    }
}

/// Evaluate a `MAKEARRAY` dimension argument to a positive integer count. The
/// value is numeric-coerced and floored (matching the INDEX index convention);
/// a non-positive or non-numeric dimension refuses loudly (`Err`).
#[allow(clippy::too_many_arguments)]
fn dim_arg(
    e: &Expr,
    anchor: CellId,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    ctx: &EvalContext,
    diags: &mut DiagnosticSink,
    lex: LexEnv<'_>,
) -> Result<usize, Value> {
    let v = eval_expr(e, anchor, cur, values, env, ctx, diags, false, lex);
    if let Value::Error(k) = v {
        return Err(Value::Error(k));
    }
    match xl_value::to_number(&v) {
        Ok(n) if n >= 1.0 && n.is_finite() => Ok(n.floor() as usize),
        Ok(_) => Err(Value::Error(ErrorKind::Value)),
        Err(k) => Err(Value::Error(k)),
    }
}

/// Attach a diagnostic to an array-context evaluation that came back
/// `#UNSUPPORTED!` **without** one (`before` is the sink length when the
/// evaluation started). The refusals the RFC-0011 gate produces by construction
/// (spec §4) live in `xl-fn`, which has no diagnostic channel: a nested function
/// whose scalar coercion met the materialized multi-cell array (function
/// broadcasting over a range — `SUM(LEN(A1:A7))`, `SUMPRODUCT(LEFT(A1:A7,1))`),
/// an aggregator other than SUM/SUMPRODUCT, or an unpinned broadcast shape in
/// `ops::broadcast_binary`. The value was already loud; this makes the
/// workbook-level fidelity report name it too.
fn note_undiagnosed_array_ctx_refusal(v: &Value, before: usize, diags: &mut DiagnosticSink) {
    if matches!(v, Value::Error(ErrorKind::Unsupported)) && diags.len() == before {
        diags.push((
            DiagnosticKind::UnsupportedConstruct,
            "array-context argument refused (#UNSUPPORTED!): a construct not on the \
             OXP-201 pinned list — a function receiving a materialized multi-cell \
             array (function broadcasting over a range, e.g. LEN(range)), an \
             aggregator other than SUM/SUMPRODUCT, or an unpinned broadcast shape \
             (RFC-0011 / M2 lane 6)"
                .to_string(),
        ));
    }
}

/// The engine's implementation of `xl-fn`'s [`CallArgs`]: it evaluates argument
/// expressions lazily and streams ranges/arrays on demand.
struct EngineArgs<'a, 'e> {
    args: &'a [Expr],
    /// The cell whose formula is being evaluated — the anchor for a
    /// relative/partial R1C1 `INDIRECT` target (OXP-142). Its sheet is `cur`.
    anchor: CellId,
    cur: SheetId,
    values: &'a ValueStore,
    env: &'a WbIndex<'e>,
    ctx: &'a EvalContext,
    diags: &'a mut DiagnosticSink,
    /// RFC-0011 + M2 lane-6 amendment: `true` when this call is an
    /// **array-context aggregator argument** (or a call nested inside one). Read
    /// only via the `array_arg_ctx` this `EngineArgs`'s `eval_expr` calls
    /// forward — so a bounded multi-cell range reaching scalar context under this
    /// call materializes into a `Value::Array` for the consumed-array evaluator
    /// (OXP-201 subset); unbounded/over-cap and unpinned shapes still refuse
    /// loudly.
    array_arg_ctx: bool,
    /// M2 lane 2: the lexical binding environment (LET/LAMBDA scope) in force,
    /// forwarded into every `eval_expr` this `EngineArgs` performs so a function
    /// argument that references a bound parameter resolves it.
    lex: LexEnv<'a>,
    /// Set once this call has received a **materialized multi-cell array** in a
    /// scalar-shaped argument (a consumed range broadcast under the RFC-0011
    /// array-context gate, or a function-produced array). Read by [`eval_call`]
    /// after the function returns: a `#UNSUPPORTED!` result with no diagnostic
    /// of its own is then the born-refusing boundary (spec §4 — the function's
    /// element-wise/array semantics are unpinned, so its scalar coercion refused
    /// the array), and `eval_call` names the function in a diagnostic.
    consumed_array_seen: bool,
    /// Set alongside `consumed_array_seen` when the materialized array already
    /// carried a Recalc refusal element (`#UNSUPPORTED!` propagated from an
    /// upstream cell): the function's own `#UNSUPPORTED!` is then propagation,
    /// not a verdict on its array semantics, and must not be attributed to it.
    consumed_array_had_refusal: bool,
}

impl EngineArgs<'_, '_> {
    /// Record that this call received a materialized multi-cell array (see
    /// [`EngineArgs::consumed_array_seen`]).
    fn note_consumed_array(&mut self, v: &Value) {
        if let Value::Array(a) = v
            && a.as_scalar().is_none()
        {
            self.consumed_array_seen = true;
            if a.iter()
                .any(|el| matches!(el, Value::Error(ErrorKind::Unsupported)))
            {
                self.consumed_array_had_refusal = true;
            }
        }
    }

    /// Evaluate a **lazy sub-expression occupying an array position** — the
    /// fall-through arm of the streaming walks (`for_each_cell`, `for_each_row`,
    /// `for_each_used_row`/`_col`, `for_each_cell_tagged`).
    ///
    /// Inside an array-context aggregator argument (`self.array_arg_ctx`) the
    /// expression evaluates under the inherited context exactly as before:
    /// `SUM(INDEX(B1:B7*2,3))` materializes the range and INDEX walks the array.
    ///
    /// At **top level** an *operator* expression (`B1:B7*1`, `range>3`,
    /// `range&"x"`, `--(range=x)`) in an array position used to be evaluated in
    /// scalar context — a silent legacy implicit intersection (`LARGE(B1:B7*1,1)`
    /// read B1 in-band and gave `#VALUE!` off-band; Excel array-evaluates the
    /// argument). Only the SUM/SUMPRODUCT consumers of such an array are
    /// OXP-201-pinned, so here the expression is evaluated under the
    /// array-context gate (the range materializes instead of intersecting) and
    /// a resulting multi-cell array is refused LOUDLY — `#UNSUPPORTED!` plus a
    /// diagnostic — rather than computed or intersected (Principle 2). A scalar
    /// result (`A1*2`, `SUM(range)*2`) is returned as before. A non-operator root
    /// (a nested call such as `MAP(...)`/`SEQUENCE(3)`, a literal) keeps the
    /// inherited context: the walks already consume its function-produced array
    /// (OXP-207 `INDEX(MAP(...),2)` = 20) and must not start refusing.
    fn eval_lazy_array_position(&mut self, e: &Expr) -> Value {
        // Operator roots, plus the two reference pass-through calls (`IF`,
        // `CHOOSE`): at top level in a plain cell they intersect their range
        // branch positionally, so `INDEX(IF(TRUE,B1:B7,K1:K7),3)` would read one
        // cell silently. Excel passes the reference through; that is unpinned
        // here, so the branch materializes under the gate and refuses loudly.
        let operator_root = match &e.kind {
            ExprKind::Unary { .. } | ExprKind::Binary { .. } | ExprKind::Postfix { .. } => true,
            ExprKind::Call { name, .. } => matches!(name.canonical.as_str(), "IF" | "CHOOSE"),
            _ => false,
        };
        if self.array_arg_ctx || !operator_root {
            let v = eval_expr(
                e,
                self.anchor,
                self.cur,
                self.values,
                self.env,
                self.ctx,
                self.diags,
                self.array_arg_ctx,
                self.lex,
            );
            self.note_consumed_array(&v);
            return v;
        }
        let before = self.diags.len();
        let v = eval_expr(
            e,
            self.anchor,
            self.cur,
            self.values,
            self.env,
            self.ctx,
            self.diags,
            true,
            self.lex,
        );
        if let Value::Array(a) = &v
            && a.as_scalar().is_none()
        {
            self.diags.push((
                DiagnosticKind::UnsupportedConstruct,
                "operator, IF, or CHOOSE expression over a multi-cell range in an \
                 array-position argument at top level: Excel array-evaluates it (or \
                 passes the reference through), but only the SUM/SUMPRODUCT consumers \
                 are oracle-pinned (OXP-201) — refused rather than implicit-intersected"
                    .to_string(),
            ));
            return Value::Error(ErrorKind::Unsupported);
        }
        note_undiagnosed_array_ctx_refusal(&v, before, self.diags);
        v
    }
}

impl CallArgs for EngineArgs<'_, '_> {
    fn count(&self) -> usize {
        self.args.len()
    }

    fn shape(&mut self, index: usize) -> ArgShape {
        let Some(e) = self.args.get(index).map(peel_paren) else {
            return ArgShape::Omitted;
        };
        // A reference-position expression — a reference-returning call
        // (OFFSET/INDIRECT/ANCHORARRAY) or the spilled-range postfix `A1#` — takes
        // the shape of the rectangle it resolves to: multi-cell → Range,
        // single-cell (or an error/refusal, which the consuming coercion will
        // surface) → Scalar.
        if is_ref_expr(e) {
            return match resolve_ref_expr(
                e,
                self.anchor,
                self.cur,
                self.values,
                self.env,
                self.ctx,
                self.diags,
                0,
            ) {
                RefExpr::Range(sr) if !rect_is_single(&sr.range) => ArgShape::Range,
                _ => ArgShape::Scalar,
            };
        }
        classify_shape(e, self.cur, self.env)
    }

    fn eval_scalar(&mut self, index: usize) -> Value {
        let Some(e) = self.args.get(index) else {
            return Value::Blank;
        };
        let v = eval_expr(
            e,
            self.anchor,
            self.cur,
            self.values,
            self.env,
            self.ctx,
            self.diags,
            self.array_arg_ctx,
            self.lex,
        );
        self.note_consumed_array(&v);
        v
    }

    /// RFC-0011 + M2 lane-6 amendment: evaluate argument `index` in **array
    /// context** — a multi-cell range reaching scalar context anywhere in its
    /// evaluation (including through nested `IF`/`ISBLANK`/operators) is handled
    /// as a genuine array: a bounded range materializes into a `Value::Array` for
    /// the consumed-array evaluator (OXP-201 subset — `SUM(IF(range,…))` folds to
    /// the array-sum), while unbounded/over-cap ranges and unpinned shapes return
    /// loud `#UNSUPPORTED!` (never a silently-wrong implicit intersection).
    fn eval_scalar_array_arg(&mut self, index: usize) -> Value {
        let Some(e) = self.args.get(index) else {
            return Value::Blank;
        };
        let before = self.diags.len();
        let v = eval_expr(
            e,
            self.anchor,
            self.cur,
            self.values,
            self.env,
            self.ctx,
            self.diags,
            true,
            self.lex,
        );
        self.note_consumed_array(&v);
        note_undiagnosed_array_ctx_refusal(&v, before, self.diags);
        v
    }

    // M2 lane 6: surface the RFC-0011 array-context flag (read-only) so nested
    // `IF`/`ISBLANK`/`NOT` decide whether to map their scalar rule element-wise
    // over a materialized consumed array. Inherited into nested `EngineArgs` via
    // `eval_call`, so it flows through `SUM(IF(NOT(ISBLANK(range)),…))`.
    fn array_arg_ctx(&self) -> bool {
        self.array_arg_ctx
    }

    fn for_each_cell(&mut self, index: usize, visit: &mut dyn FnMut(&Value) -> ControlFlow<()>) {
        let Some(e) = self.args.get(index).map(peel_paren) else {
            return;
        };
        match &e.kind {
            ExprKind::Array(rows) => {
                for row in rows {
                    for el in row {
                        let v = eval_expr(
                            el,
                            self.anchor,
                            self.cur,
                            self.values,
                            self.env,
                            self.ctx,
                            self.diags,
                            self.array_arg_ctx,
                            self.lex,
                        );
                        if visit(&v).is_break() {
                            return;
                        }
                    }
                }
            }
            ExprKind::Binary {
                op: BinaryOp::Range,
                lhs,
                rhs,
            } => match resolve_range(lhs, rhs, self.cur, self.env) {
                Some(sr) => stream_range(&sr, self.values, visit),
                None => {
                    let _ = visit(&Value::Error(ErrorKind::Unsupported));
                }
            },
            ExprKind::Name(n) => match resolve_name(n, self.cur, self.env) {
                Some(resolved) => {
                    stream_resolved_name(&resolved, self.cur, self.values, self.env, visit)
                }
                None => {
                    let _ = visit(&Value::Error(ErrorKind::Unsupported));
                }
            },
            // A reference-returning call (OFFSET/INDIRECT): resolve to a rectangle
            // and stream it exactly as a literal range (RFC 0003, approach A2).
            _ if is_ref_expr(e) => {
                match resolve_ref_expr(
                    e,
                    self.anchor,
                    self.cur,
                    self.values,
                    self.env,
                    self.ctx,
                    self.diags,
                    0,
                ) {
                    RefExpr::Range(sr) => stream_range(&sr, self.values, visit),
                    RefExpr::Error(k) => {
                        let _ = visit(&Value::Error(k));
                    }
                    RefExpr::Unsupported => {
                        let _ = visit(&Value::Error(ErrorKind::Unsupported));
                    }
                }
            }
            // A scalar argument streamed (e.g. an argument the function treated
            // as a range but which is a lone value): visit its single value. A
            // computed `Value::Array` (a nested function that produced an array,
            // e.g. `MAP(...)` — M2 lane 2) streams cell-by-cell, so an aggregator
            // consumes it exactly like a range.
            _ => {
                let v = self.eval_lazy_array_position(e);
                if let Value::Array(a) = &v {
                    for cell in a.iter() {
                        if visit(cell).is_break() {
                            break;
                        }
                    }
                } else {
                    let _ = visit(&v);
                }
            }
        }
    }

    fn dims(&mut self, index: usize) -> Option<(u32, u32)> {
        let e = peel_paren(self.args.get(index)?);
        match &e.kind {
            ExprKind::Array(rows) => {
                let r = rows.len() as u32;
                let c = rows.first().map_or(0, Vec::len) as u32;
                if r == 0 || c == 0 { None } else { Some((r, c)) }
            }
            ExprKind::Binary {
                op: BinaryOp::Range,
                lhs,
                rhs,
            } => resolve_range(lhs, rhs, self.cur, self.env).and_then(|sr| bounded_dims(&sr)),
            ExprKind::Name(n) => match resolve_name(n, self.cur, self.env) {
                Some(resolved) => resolved_range_dims(&resolved, self.cur, self.env),
                None => None,
            },
            // A reference-returning call resolves to a rectangle whose dims are
            // reported like any range (bounded); an error/refusal has none.
            _ if is_ref_expr(e) => {
                match resolve_ref_expr(
                    e,
                    self.anchor,
                    self.cur,
                    self.values,
                    self.env,
                    self.ctx,
                    self.diags,
                    0,
                ) {
                    RefExpr::Range(sr) => bounded_dims(&sr),
                    RefExpr::Error(_) | RefExpr::Unsupported => None,
                }
            }
            // Scalars, lazy sub-expressions, and omitted slots have no
            // materializable rectangle.
            _ => None,
        }
    }

    fn for_each_row(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(&[Value]) -> ControlFlow<()>,
    ) -> Result<(), ErrorKind> {
        let Some(e) = self.args.get(index).map(peel_paren) else {
            // An omitted/out-of-range argument is an empty rectangle.
            return Ok(());
        };
        match &e.kind {
            ExprKind::Array(rows) => {
                let mut buf: Vec<Value> = Vec::new();
                for row in rows {
                    buf.clear();
                    for el in row {
                        buf.push(eval_expr(
                            el,
                            self.anchor,
                            self.cur,
                            self.values,
                            self.env,
                            self.ctx,
                            self.diags,
                            self.array_arg_ctx,
                            self.lex,
                        ));
                    }
                    if visit(&buf).is_break() {
                        break;
                    }
                }
                Ok(())
            }
            ExprKind::Binary {
                op: BinaryOp::Range,
                lhs,
                rhs,
            } => match resolve_range(lhs, rhs, self.cur, self.env) {
                Some(sr) => rows_of_range(&sr, self.values, visit),
                None => Err(ErrorKind::Unsupported),
            },
            ExprKind::Name(n) => match resolve_name(n, self.cur, self.env) {
                Some(resolved) => {
                    rows_of_resolved_name(&resolved, self.cur, self.values, self.env, visit)
                }
                None => Err(ErrorKind::Unsupported),
            },
            // A reference-returning call: dense row-walk of the rectangle it
            // resolves to (an unbounded whole-column result is refused by
            // `rows_of_range`, per policy).
            _ if is_ref_expr(e) => {
                match resolve_ref_expr(
                    e,
                    self.anchor,
                    self.cur,
                    self.values,
                    self.env,
                    self.ctx,
                    self.diags,
                    0,
                ) {
                    RefExpr::Range(sr) => rows_of_range(&sr, self.values, visit),
                    RefExpr::Error(k) => Err(k),
                    RefExpr::Unsupported => Err(ErrorKind::Unsupported),
                }
            }
            // A lazy sub-expression: evaluate it. A computed `Value::Array`
            // (e.g. `INDEX(MAP(...),k)` — M2 lane 2) is walked as the rectangle
            // it is, row by row; any other value is a 1×1 rectangle.
            _ => {
                let v = self.eval_lazy_array_position(e);
                // A Recalc refusal sentinel from the lazy evaluation is a refused
                // rectangle, not a 1×1 cell holding an error: surface it as the
                // walk's own `Err` so the consumer stays loud (`INDEX(B1:B7*2,3)`
                // would otherwise read a 1-row rectangle and answer `#REF!`).
                if matches!(v, Value::Error(ErrorKind::Unsupported)) {
                    return Err(ErrorKind::Unsupported);
                }
                if let Value::Array(a) = &v {
                    let (r, c) = (a.rows(), a.cols());
                    let mut buf: Vec<Value> = Vec::with_capacity(c);
                    for row in 0..r {
                        buf.clear();
                        for col in 0..c {
                            buf.push(a.get(row, col).cloned().unwrap_or(Value::Blank));
                        }
                        if visit(&buf).is_break() {
                            break;
                        }
                    }
                } else {
                    let _ = visit(std::slice::from_ref(&v));
                }
                Ok(())
            }
        }
    }

    fn for_each_used_row(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
    ) -> Result<(), ErrorKind> {
        let Some(e) = self.args.get(index).map(peel_paren) else {
            // An omitted/out-of-range argument is an empty rectangle.
            return Ok(());
        };
        match &e.kind {
            ExprKind::Binary {
                op: BinaryOp::Range,
                lhs,
                rhs,
            } => match resolve_range(lhs, rhs, self.cur, self.env) {
                Some(sr) => used_rows_of_range(&sr, self.values, visit),
                None => Err(ErrorKind::Unsupported),
            },
            ExprKind::Name(n) => match resolve_name(n, self.cur, self.env) {
                Some(resolved) => {
                    used_rows_of_resolved_name(&resolved, self.cur, self.values, self.env, visit)
                }
                None => Err(ErrorKind::Unsupported),
            },
            // A reference-returning call: used-extent (sparse) row-walk of the
            // rectangle it resolves to, so a whole-column OFFSET result streams
            // only its populated rows (RFC 0001 / RFC 0003).
            _ if is_ref_expr(e) => {
                match resolve_ref_expr(
                    e,
                    self.anchor,
                    self.cur,
                    self.values,
                    self.env,
                    self.ctx,
                    self.diags,
                    0,
                ) {
                    RefExpr::Range(sr) => used_rows_of_range(&sr, self.values, visit),
                    RefExpr::Error(k) => Err(k),
                    RefExpr::Unsupported => Err(ErrorKind::Unsupported),
                }
            }
            // Array constants and scalars are already bounded, so the dense
            // `for_each_row` serves them and `for_each_row_or_used` never routes
            // them here; handle them anyway so the method is self-consistent when
            // called directly. Each array row carries its 0-based (relative) index.
            ExprKind::Array(rows) => {
                let mut buf: Vec<Value> = Vec::new();
                for (i, row) in rows.iter().enumerate() {
                    buf.clear();
                    for el in row {
                        buf.push(eval_expr(
                            el,
                            self.anchor,
                            self.cur,
                            self.values,
                            self.env,
                            self.ctx,
                            self.diags,
                            self.array_arg_ctx,
                            self.lex,
                        ));
                    }
                    if visit(i as u32, &buf).is_break() {
                        break;
                    }
                }
                Ok(())
            }
            _ => {
                let v = self.eval_lazy_array_position(e);
                // A refusal sentinel is a refused rectangle (see `for_each_row`).
                if matches!(v, Value::Error(ErrorKind::Unsupported)) {
                    return Err(ErrorKind::Unsupported);
                }
                let _ = visit(0, std::slice::from_ref(&v));
                Ok(())
            }
        }
    }

    fn for_each_used_col(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
    ) -> Result<(), ErrorKind> {
        let Some(e) = self.args.get(index).map(peel_paren) else {
            // An omitted/out-of-range argument is an empty rectangle.
            return Ok(());
        };
        match &e.kind {
            ExprKind::Binary {
                op: BinaryOp::Range,
                lhs,
                rhs,
            } => match resolve_range(lhs, rhs, self.cur, self.env) {
                Some(sr) => used_cols_of_range(&sr, self.values, visit),
                None => Err(ErrorKind::Unsupported),
            },
            ExprKind::Name(n) => match resolve_name(n, self.cur, self.env) {
                Some(resolved) => {
                    used_cols_of_resolved_name(&resolved, self.cur, self.values, self.env, visit)
                }
                None => Err(ErrorKind::Unsupported),
            },
            // A reference-returning call: used-extent (sparse) column-walk of the
            // rectangle it resolves to, so a whole-row OFFSET result streams only
            // its populated columns (RFC 0008 / RFC 0003).
            _ if is_ref_expr(e) => {
                match resolve_ref_expr(
                    e,
                    self.anchor,
                    self.cur,
                    self.values,
                    self.env,
                    self.ctx,
                    self.diags,
                    0,
                ) {
                    RefExpr::Range(sr) => used_cols_of_range(&sr, self.values, visit),
                    RefExpr::Error(k) => Err(k),
                    RefExpr::Unsupported => Err(ErrorKind::Unsupported),
                }
            }
            // Array constants and scalars are already bounded, so the dense
            // `for_each_row` serves them and `for_each_row_or_used_any_axis` never
            // routes them here; handle them anyway so the method is self-consistent
            // when called directly. An array is transposed to columns: each
            // populated column carries its 0-based (relative) index, and absent
            // cells within the (ragged) span surface as `Value::Blank`.
            ExprKind::Array(rows) => {
                let width = rows.iter().map(Vec::len).max().unwrap_or(0);
                let height = rows.len();
                let mut col_buf: Vec<Value> = Vec::with_capacity(height);
                for c in 0..width {
                    col_buf.clear();
                    for row in rows {
                        let v = match row.get(c) {
                            Some(el) => eval_expr(
                                el,
                                self.anchor,
                                self.cur,
                                self.values,
                                self.env,
                                self.ctx,
                                self.diags,
                                self.array_arg_ctx,
                                self.lex,
                            ),
                            None => Value::Blank,
                        };
                        col_buf.push(v);
                    }
                    if visit(c as u32, &col_buf).is_break() {
                        break;
                    }
                }
                Ok(())
            }
            _ => {
                let v = self.eval_lazy_array_position(e);
                // A refusal sentinel is a refused rectangle (see `for_each_row`).
                if matches!(v, Value::Error(ErrorKind::Unsupported)) {
                    return Err(ErrorKind::Unsupported);
                }
                let _ = visit(0, std::slice::from_ref(&v));
                Ok(())
            }
        }
    }

    fn for_each_cell_tagged(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(&Value, CellFlags) -> ControlFlow<()>,
    ) -> Result<(), ()> {
        let Some(e) = self.args.get(index).map(peel_paren) else {
            // Omitted / out-of-range: no cells to stream, trivially served.
            return Ok(());
        };
        match &e.kind {
            // An array constant has cells but no originating formula, so there is
            // no provenance to surface: refuse (`Err(())`) so the caller falls
            // back to the plain, un-tagged `for_each_cell` walk (RFC 0002).
            ExprKind::Array(_) => Err(()),
            ExprKind::Binary {
                op: BinaryOp::Range,
                lhs,
                rhs,
            } => {
                match resolve_range(lhs, rhs, self.cur, self.env) {
                    Some(sr) => stream_range_tagged(
                        &sr,
                        self.values,
                        self.env.subtotals,
                        self.env.hidden_rows,
                        visit,
                    ),
                    // Mirror `for_each_cell`: surface the unresolved-range error
                    // (untagged) so the aggregate propagates it.
                    None => {
                        let _ = visit(&Value::Error(ErrorKind::Unsupported), CellFlags::default());
                    }
                }
                Ok(())
            }
            ExprKind::Name(n) => {
                match resolve_name(n, self.cur, self.env) {
                    Some(resolved) => stream_resolved_name_tagged(
                        &resolved,
                        self.cur,
                        self.values,
                        self.env,
                        visit,
                    ),
                    None => {
                        let _ = visit(&Value::Error(ErrorKind::Unsupported), CellFlags::default());
                    }
                }
                Ok(())
            }
            // A single-cell reference: surface its value tagged with whether that
            // cell is itself a SUBTOTAL, so `SUBTOTAL(9, A1, …)` excludes a nested
            // sub-total reached through a bare ref (not just through a range).
            ExprKind::Ref(r) => {
                match resolve_reference(r, self.cur, self.env) {
                    RefResolution::Cell(c) => {
                        let v = self.values.get(&c).cloned().unwrap_or(Value::Blank);
                        let flags = cell_flags(&c, self.env.subtotals, self.env.hidden_rows);
                        let _ = visit(&v, flags);
                    }
                    RefResolution::Unsupported => {
                        let _ = visit(&Value::Error(ErrorKind::Unsupported), CellFlags::default());
                    }
                }
                Ok(())
            }
            // A reference-returning call (OFFSET/INDIRECT): stream the resolved
            // rectangle's cells with provenance, so a `SUBTOTAL` over an OFFSET
            // range still excludes nested sub-totals (RFC 0002 × RFC 0003).
            _ if is_ref_expr(e) => {
                match resolve_ref_expr(
                    e,
                    self.anchor,
                    self.cur,
                    self.values,
                    self.env,
                    self.ctx,
                    self.diags,
                    0,
                ) {
                    RefExpr::Range(sr) => {
                        stream_range_tagged(
                            &sr,
                            self.values,
                            self.env.subtotals,
                            self.env.hidden_rows,
                            visit,
                        );
                    }
                    RefExpr::Error(k) => {
                        let _ = visit(&Value::Error(k), CellFlags::default());
                    }
                    RefExpr::Unsupported => {
                        let _ = visit(&Value::Error(ErrorKind::Unsupported), CellFlags::default());
                    }
                }
                Ok(())
            }
            // Scalars, literals, and computed sub-expressions have no cell
            // provenance but can still contribute a value (`SUBTOTAL(9, 5)`);
            // serve them with default (all-false) flags, mirroring
            // `for_each_cell`'s scalar arm.
            _ => {
                let v = self.eval_lazy_array_position(e);
                let _ = visit(&v, CellFlags::default());
                Ok(())
            }
        }
    }

    /// The calling cell's own position (RFC 0005). `EngineArgs` already holds the
    /// anchor — the cell whose formula is being evaluated — so `ROW()`/`COLUMN()`
    /// read its 0-based `(row, col)` (plus sheet) here.
    fn anchor(&self) -> Option<(SheetId, u32, u32)> {
        Some((self.anchor.sheet, self.anchor.row, self.anchor.col))
    }

    /// The position + full extent of a *reference* argument (RFC 0005).
    ///
    /// Resolves the argument through the same reference machinery every other
    /// `EngineArgs` method uses — [`resolve_ref_expr`] handles the ordinary
    /// reference shapes (a bare cell, a `ref:ref` range including whole
    /// column/row, a simple defined name) **and** the RFC 0003
    /// reference-returning calls (`OFFSET`/`INDIRECT`), so `ROW(OFFSET(A1,2,0))`
    /// reads the computed reference's top row. Anything that is not a resolvable
    /// single-area reference (a literal, an array, a union, or a computed
    /// reference that errored) yields `None`.
    ///
    /// Unlike [`Self::dims`], the whole-column/row extent is **not** refused
    /// here: the returned `height`/`width` carry the full sheet-axis size
    /// (`A:A` → `height == 1_048_576`), which `ROWS`/`COLUMNS` need for OXP-116.
    fn arg_ref_extent(&mut self, index: usize) -> Option<RefRect> {
        let e = self.args.get(index)?;
        match resolve_ref_expr(
            e,
            self.anchor,
            self.cur,
            self.values,
            self.env,
            self.ctx,
            self.diags,
            0,
        ) {
            RefExpr::Range(sr) => {
                let rect = sr.range;
                Some(RefRect {
                    row: rect.row_start,
                    col: rect.col_start,
                    height: rect.row_end - rect.row_start + 1,
                    width: rect.col_end - rect.col_start + 1,
                })
            }
            RefExpr::Error(_) | RefExpr::Unsupported => None,
        }
    }

    /// Read one cell at an absolute `(row, col)` inside argument `index`'s
    /// declared reference rectangle (RFC 0006).
    ///
    /// Resolves the argument through the same [`resolve_ref_expr`] path every
    /// other `EngineArgs` method uses, then — only if `(row, col)` lies inside the
    /// resolved rectangle — reads the sparse value store at that absolute
    /// coordinate (an absent cell reads as [`Value::Blank`]). A position outside
    /// the rectangle, or an argument that is not a resolvable single-area
    /// reference, yields `None`. The read never escapes the AST-declared
    /// rectangle, so the cell is already a static dependency of the formula — no
    /// dependency-graph hazard and no whole-sheet scan (see
    /// `rfcs/0006-in-rectangle-absolute-read.md`).
    fn cell_at(&mut self, index: usize, row: u32, col: u32) -> Option<Value> {
        let e = self.args.get(index)?;
        match resolve_ref_expr(
            e,
            self.anchor,
            self.cur,
            self.values,
            self.env,
            self.ctx,
            self.diags,
            0,
        ) {
            RefExpr::Range(sr) => {
                let rect = sr.range;
                if row < rect.row_start
                    || row > rect.row_end
                    || col < rect.col_start
                    || col > rect.col_end
                {
                    return None;
                }
                let cell = CellId::new(sr.sheet, row, col);
                Some(self.values.get(&cell).cloned().unwrap_or(Value::Blank))
            }
            RefExpr::Error(_) | RefExpr::Unsupported => None,
        }
    }
}

/// The dense-walk dimensions of a resolved range, or `None` if it is unbounded
/// (a whole-column/row range spanning the full sheet — see
/// [`CallArgs::for_each_row`] for the refusal policy).
fn bounded_dims(sr: &SheetRange) -> Option<(u32, u32)> {
    if is_unbounded(sr) {
        return None;
    }
    let rect = sr.range;
    let rows = rect.row_end - rect.row_start + 1;
    let cols = rect.col_end - rect.col_start + 1;
    Some((rows, cols))
}

/// `dims` for a defined name that resolves to a bare ref or a range.
fn resolved_range_dims(resolved: &Expr, cur: SheetId, env: &WbIndex) -> Option<(u32, u32)> {
    match &resolved.kind {
        ExprKind::Binary {
            op: BinaryOp::Range,
            lhs,
            rhs,
        } => resolve_range(lhs, rhs, cur, env).and_then(|sr| bounded_dims(&sr)),
        ExprKind::Ref(r) => match resolve_reference(r, cur, env) {
            RefResolution::Cell(_) => Some((1, 1)),
            RefResolution::Unsupported => None,
        },
        _ => None,
    }
}

/// Whether a resolved range spans the full sheet on either axis, so a dense
/// row-by-row walk of it would materialize up to Excel's 1,048,576 rows.
/// Refusing this is a documented engineering policy (see
/// [`CallArgs::for_each_row`]), not an Excel semantic.
fn is_unbounded(sr: &SheetRange) -> bool {
    let rect = sr.range;
    (rect.row_start == 0 && rect.row_end == MAX_ROW0)
        || (rect.col_start == 0 && rect.col_end == MAX_COL0)
}

/// Stream a resolved defined-name reference (a bare ref or a range).
fn stream_resolved_name(
    resolved: &Expr,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    visit: &mut dyn FnMut(&Value) -> ControlFlow<()>,
) {
    match &resolved.kind {
        ExprKind::Binary {
            op: BinaryOp::Range,
            lhs,
            rhs,
        } => match resolve_range(lhs, rhs, cur, env) {
            Some(sr) => stream_range(&sr, values, visit),
            None => {
                let _ = visit(&Value::Error(ErrorKind::Unsupported));
            }
        },
        ExprKind::Ref(r) => match resolve_reference(r, cur, env) {
            RefResolution::Cell(c) => {
                let _ = visit(values.get(&c).unwrap_or(&Value::Blank));
            }
            RefResolution::Unsupported => {
                let _ = visit(&Value::Error(ErrorKind::Unsupported));
            }
        },
        _ => {
            let _ = visit(&Value::Error(ErrorKind::Unsupported));
        }
    }
}

/// Row-by-row dense walk of a resolved defined name (a bare ref or a range).
fn rows_of_resolved_name(
    resolved: &Expr,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    visit: &mut dyn FnMut(&[Value]) -> ControlFlow<()>,
) -> Result<(), ErrorKind> {
    match &resolved.kind {
        ExprKind::Binary {
            op: BinaryOp::Range,
            lhs,
            rhs,
        } => match resolve_range(lhs, rhs, cur, env) {
            Some(sr) => rows_of_range(&sr, values, visit),
            None => Err(ErrorKind::Unsupported),
        },
        ExprKind::Ref(r) => match resolve_reference(r, cur, env) {
            RefResolution::Cell(c) => {
                let v = values.get(&c).cloned().unwrap_or(Value::Blank);
                let _ = visit(std::slice::from_ref(&v));
                Ok(())
            }
            RefResolution::Unsupported => Err(ErrorKind::Unsupported),
        },
        _ => Err(ErrorKind::Unsupported),
    }
}

/// Classify an argument's syntactic shape for coercion-mode selection.
/// Peel grouping parentheses: `(X)` is pure grouping, so its argument shape and
/// its streamed cells are exactly `X`'s. Only [`ExprKind::Paren`] is peeled —
/// never [`ExprKind::ImplicitIntersection`] (`@X`), which deliberately *changes*
/// semantics. This mirrors the `Paren` unwrap in [`resolve_ref_expr`]
/// (`refx.rs`); without it, `arg_ref_extent` unwraps a paren-wrapped range to an
/// aggregate extent while `for_each_cell`/`classify_shape` fall through to the
/// scalar path and implicitly-intersect a single cell — the silently-wrong,
/// formula-position-dependent `SUM((A1:A5))` (contentious-areas audit
/// 2026-07-13; behavior pinned by the paren-shape farm probe).
fn peel_paren(mut e: &Expr) -> &Expr {
    while let ExprKind::Paren(inner) = &e.kind {
        e = inner;
    }
    e
}

fn classify_shape(expr: &Expr, cur: SheetId, env: &WbIndex) -> ArgShape {
    let expr = peel_paren(expr);
    match &expr.kind {
        ExprKind::Missing => ArgShape::Omitted,
        ExprKind::Array(rows) => {
            let n: usize = rows.iter().map(|r| r.len()).sum();
            if n > 1 {
                ArgShape::Array
            } else {
                ArgShape::Scalar
            }
        }
        ExprKind::Binary {
            op: BinaryOp::Range,
            lhs,
            rhs,
        } => match resolve_range(lhs, rhs, cur, env) {
            Some(sr) if !rect_is_single(&sr.range) => ArgShape::Range,
            // A 1×1 range (or an unresolvable one, which will error) is treated
            // as a scalar.
            _ => ArgShape::Scalar,
        },
        ExprKind::Name(n) => match resolve_name(n, cur, env) {
            Some(resolved) => classify_shape(&resolved, cur, env),
            None => ArgShape::Scalar,
        },
        _ => ArgShape::Scalar,
    }
}

/// Visit every *materialized* cell of a range, in row-major (`CellId`) order.
///
/// Absent cells are blanks that aggregation skips, so they are elided (see
/// [`CallArgs::for_each_cell`]). Uses a `BTreeMap` band scan (`CellId` orders as
/// `(sheet, row, col)`) so a whole-column range costs `O(populated cells)`, not
/// `O(1,048,576)`. A [`ControlFlow::Break`] from `visit` stops the scan.
fn stream_range(
    sr: &SheetRange,
    values: &ValueStore,
    visit: &mut dyn FnMut(&Value) -> ControlFlow<()>,
) {
    let rect = sr.range;
    if rect.row_start > rect.row_end || rect.col_start > rect.col_end {
        return;
    }
    let lo = CellId {
        sheet: sr.sheet,
        row: rect.row_start,
        col: 0,
    };
    let hi = CellId {
        sheet: sr.sheet,
        row: rect.row_end,
        col: u32::MAX,
    };
    for (cell, value) in values.range(lo..=hi) {
        if cell.col >= rect.col_start && cell.col <= rect.col_end && visit(value).is_break() {
            return;
        }
    }
}

/// Like [`stream_range`], but yields each materialized cell's [`CellFlags`]
/// provenance alongside its value (RFC 0002). A cell is flagged
/// [`CellFlags::is_subtotal`] when its [`CellId`] is in `subtotals` — the
/// workbook's set of cells whose own formula head is a `SUBTOTAL` call — and
/// [`CellFlags::is_hidden_row`] when its `(sheet, row)` is in `hidden_rows` —
/// the workbook's `<row hidden="1">` rows (OXP-121) — so a `SUBTOTAL` aggregate
/// can skip nested sub-totals and (for `101`–`111`) hidden-row cells. Same
/// sparse band scan and [`ControlFlow::Break`] short-circuit as [`stream_range`].
fn stream_range_tagged(
    sr: &SheetRange,
    values: &ValueStore,
    subtotals: &std::collections::BTreeSet<CellId>,
    hidden_rows: &std::collections::BTreeSet<(SheetId, u32)>,
    visit: &mut dyn FnMut(&Value, CellFlags) -> ControlFlow<()>,
) {
    let rect = sr.range;
    if rect.row_start > rect.row_end || rect.col_start > rect.col_end {
        return;
    }
    let lo = CellId {
        sheet: sr.sheet,
        row: rect.row_start,
        col: 0,
    };
    let hi = CellId {
        sheet: sr.sheet,
        row: rect.row_end,
        col: u32::MAX,
    };
    for (cell, value) in values.range(lo..=hi) {
        if cell.col >= rect.col_start && cell.col <= rect.col_end {
            let flags = cell_flags(cell, subtotals, hidden_rows);
            if visit(value, flags).is_break() {
                return;
            }
        }
    }
}

/// Build the [`CellFlags`] provenance for a single cell from the workbook's
/// SUBTOTAL tag set and hidden-row set (RFC 0002 / OXP-121). Centralizes the two
/// membership lookups so every tagged path derives the same flags.
fn cell_flags(
    cell: &CellId,
    subtotals: &std::collections::BTreeSet<CellId>,
    hidden_rows: &std::collections::BTreeSet<(SheetId, u32)>,
) -> CellFlags {
    CellFlags {
        is_subtotal: subtotals.contains(cell),
        is_hidden_row: hidden_rows.contains(&(cell.sheet, cell.row)),
    }
}

/// Tagged counterpart to [`stream_resolved_name`]: stream a resolved defined-name
/// reference (a bare ref or a range) with each cell's [`CellFlags`] (RFC 0002).
fn stream_resolved_name_tagged(
    resolved: &Expr,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    visit: &mut dyn FnMut(&Value, CellFlags) -> ControlFlow<()>,
) {
    match &resolved.kind {
        ExprKind::Binary {
            op: BinaryOp::Range,
            lhs,
            rhs,
        } => match resolve_range(lhs, rhs, cur, env) {
            Some(sr) => stream_range_tagged(&sr, values, env.subtotals, env.hidden_rows, visit),
            None => {
                let _ = visit(&Value::Error(ErrorKind::Unsupported), CellFlags::default());
            }
        },
        ExprKind::Ref(r) => match resolve_reference(r, cur, env) {
            RefResolution::Cell(c) => {
                let v = values.get(&c).cloned().unwrap_or(Value::Blank);
                let flags = cell_flags(&c, env.subtotals, env.hidden_rows);
                let _ = visit(&v, flags);
            }
            RefResolution::Unsupported => {
                let _ = visit(&Value::Error(ErrorKind::Unsupported), CellFlags::default());
            }
        },
        _ => {
            let _ = visit(&Value::Error(ErrorKind::Unsupported), CellFlags::default());
        }
    }
}

/// Visit a range one **row** at a time, densely: every cell of the rectangle is
/// yielded (absent cells as [`Value::Blank`] at their column position), unlike
/// the blank-eliding [`stream_range`]. Refuses an unbounded whole-column/row
/// range with [`ErrorKind::Unsupported`] (see [`CallArgs::for_each_row`]).
fn rows_of_range(
    sr: &SheetRange,
    values: &ValueStore,
    visit: &mut dyn FnMut(&[Value]) -> ControlFlow<()>,
) -> Result<(), ErrorKind> {
    if is_unbounded(sr) {
        return Err(ErrorKind::Unsupported);
    }
    let rect = sr.range;
    if rect.row_start > rect.row_end || rect.col_start > rect.col_end {
        return Ok(());
    }
    let width = (rect.col_end - rect.col_start + 1) as usize;
    let mut row_buf: Vec<Value> = Vec::with_capacity(width);
    for row in rect.row_start..=rect.row_end {
        row_buf.clear();
        for col in rect.col_start..=rect.col_end {
            let cell = CellId::new(sr.sheet, row, col);
            row_buf.push(values.get(&cell).cloned().unwrap_or(Value::Blank));
        }
        if visit(&row_buf).is_break() {
            break;
        }
    }
    Ok(())
}

/// Visit the **populated** rows of a range within its column span, one row at a
/// time, in ascending row order — the sparse *used-extent* walk (RFC 0001).
///
/// Unlike [`rows_of_range`], which materialises every row of the rectangle (and
/// therefore refuses an unbounded whole-column range), this scans only the rows
/// that actually contain a cell in the column span, so a whole-column `A:D`
/// costs `O(populated cells)` via the `BTreeMap` band scan. Each populated row
/// is yielded as a full-column-span slice (`col_end - col_start + 1` wide) with
/// absent cells surfaced as [`Value::Blank`]; the `u32` handed to `visit` is the
/// row's index **relative to `rect.row_start`** (0 = the range's top row).
///
/// A **whole-row** range (unbounded *columns*, `col_start == 0 && col_end ==
/// MAX_COL0`) is refused with [`ErrorKind::Unsupported`]: its per-row slice would
/// be 16,384 wide, which this row-oriented walk does not serve. Ascending row
/// order is inherited from the `(sheet, row, col)` ordering of [`CellId`].
fn used_rows_of_range(
    sr: &SheetRange,
    values: &ValueStore,
    visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
) -> Result<(), ErrorKind> {
    let rect = sr.range;
    // Refuse whole-row ranges (unbounded columns): the column span is the whole
    // sheet width, so a per-row slice would be 16,384 cells. This row iterator
    // serves whole-COLUMN ranges; whole-row support needs a column iterator.
    if rect.col_start == 0 && rect.col_end == MAX_COL0 {
        return Err(ErrorKind::Unsupported);
    }
    if rect.row_start > rect.row_end || rect.col_start > rect.col_end {
        return Ok(());
    }
    let width = (rect.col_end - rect.col_start + 1) as usize;
    let lo = CellId {
        sheet: sr.sheet,
        row: rect.row_start,
        col: 0,
    };
    let hi = CellId {
        sheet: sr.sheet,
        row: rect.row_end,
        col: u32::MAX,
    };
    let mut cur_row: Option<u32> = None;
    let mut buf: Vec<Value> = vec![Value::Blank; width];
    for (cell, value) in values.range(lo..=hi) {
        if cell.col < rect.col_start || cell.col > rect.col_end {
            continue;
        }
        if cur_row != Some(cell.row) {
            // The row changed: flush the row we were accumulating, then reset.
            if let Some(r) = cur_row
                && visit(r - rect.row_start, &buf).is_break()
            {
                return Ok(());
            }
            for v in buf.iter_mut() {
                *v = Value::Blank;
            }
            cur_row = Some(cell.row);
        }
        buf[(cell.col - rect.col_start) as usize] = value.clone();
    }
    // Flush the final accumulated row.
    if let Some(r) = cur_row {
        let _ = visit(r - rect.row_start, &buf);
    }
    Ok(())
}

/// Used-extent walk of a resolved defined name (a bare ref or a range), mirroring
/// [`rows_of_resolved_name`] but yielding relative row indices.
fn used_rows_of_resolved_name(
    resolved: &Expr,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
) -> Result<(), ErrorKind> {
    match &resolved.kind {
        ExprKind::Binary {
            op: BinaryOp::Range,
            lhs,
            rhs,
        } => match resolve_range(lhs, rhs, cur, env) {
            Some(sr) => used_rows_of_range(&sr, values, visit),
            None => Err(ErrorKind::Unsupported),
        },
        ExprKind::Ref(r) => match resolve_reference(r, cur, env) {
            RefResolution::Cell(c) => {
                let v = values.get(&c).cloned().unwrap_or(Value::Blank);
                let _ = visit(0, std::slice::from_ref(&v));
                Ok(())
            }
            RefResolution::Unsupported => Err(ErrorKind::Unsupported),
        },
        _ => Err(ErrorKind::Unsupported),
    }
}

/// Visit the **populated** columns of a range within its row span, one column at
/// a time, in ascending **column** order — the sparse *used-extent* COLUMN walk
/// (RFC 0008), the transpose of [`used_rows_of_range`].
///
/// Serves a whole-**row** range (unbounded *columns*, small row span — usually a
/// single row) in `O(populated)`. Each populated column is yielded as a
/// full-row-span slice (`row_end - row_start + 1` tall) with absent cells
/// surfaced as [`Value::Blank`]; the `u32` handed to `visit` is the column's
/// index **relative to `rect.col_start`** (0 = the range's left column).
///
/// # Ascending order is not free (RFC 0008 §2)
/// [`used_rows_of_range`] inherits ascending row order from the
/// `(sheet, row, col)` ordering of [`CellId`] — a row-band scan is intrinsically
/// row-ascending. A row-major scan encounters *columns* out of order, so this
/// **buffers** the row band's populated cells grouped by column (a
/// `BTreeMap<col, column-slice>`), then emits them in ascending column order.
/// Cost is `O(populated columns × row-span height)` memory + `O(P log P)` for the
/// distinct columns `P`, bounded by the row band's populated width (one row's
/// worth in the common `1:1` case), never the 16,384 sheet width.
///
/// A whole-**column** range (unbounded *rows*, `row_start == 0 && row_end ==
/// MAX_ROW0`) is refused with [`ErrorKind::Unsupported`]: its per-column slice
/// would be 1,048,576 cells tall, which this column-oriented walk does not serve
/// (that is [`used_rows_of_range`]'s job). Only populated columns are emitted.
fn used_cols_of_range(
    sr: &SheetRange,
    values: &ValueStore,
    visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
) -> Result<(), ErrorKind> {
    let rect = sr.range;
    // Refuse whole-column ranges (unbounded rows): the row span is the whole sheet
    // height, so a per-column slice would be 1,048,576 cells. This column iterator
    // serves whole-ROW ranges; whole-column support is `used_rows_of_range`.
    if rect.row_start == 0 && rect.row_end == MAX_ROW0 {
        return Err(ErrorKind::Unsupported);
    }
    if rect.row_start > rect.row_end || rect.col_start > rect.col_end {
        return Ok(());
    }
    let height = (rect.row_end - rect.row_start + 1) as usize;
    // Buffer the row band's populated cells grouped by column. A (sheet, row, col)
    // band scan is row-major, so columns arrive out of order; the `BTreeMap`
    // restores ascending column order (RFC 0008 §2 — the one non-mechanical part
    // of the transpose). Each column buffer is a full row-span-tall slice, with
    // absent cells left as `Value::Blank`.
    //
    // Unlike the row walk (which streams one col-span-wide row at a time), this
    // must materialise every populated column at once to sort by column, so its
    // footprint is `O(populated_cols × height)`. A real whole-row band is a
    // handful of rows; a pathological one (`1:1048576`) is not. Cap the total
    // buffered cells and fail loud with `#RESOURCE!` rather than OOM (the height
    // factor makes the first over-cap column bail after ≤ ~one column's worth).
    const USED_COL_BUFFER_CAP: usize = 1 << 20; // ~1M Values (~32 MB worst case)
    let lo = CellId {
        sheet: sr.sheet,
        row: rect.row_start,
        col: 0,
    };
    let hi = CellId {
        sheet: sr.sheet,
        row: rect.row_end,
        col: u32::MAX,
    };
    let mut cols: BTreeMap<u32, Vec<Value>> = BTreeMap::new();
    for (cell, value) in values.range(lo..=hi) {
        if cell.col < rect.col_start || cell.col > rect.col_end {
            continue;
        }
        if !cols.contains_key(&cell.col) && (cols.len() + 1) * height > USED_COL_BUFFER_CAP {
            // Adding another `height`-tall column would exceed the cap.
            return Err(ErrorKind::Resource);
        }
        let col_buf = cols
            .entry(cell.col)
            .or_insert_with(|| vec![Value::Blank; height]);
        col_buf[(cell.row - rect.row_start) as usize] = value.clone();
    }
    for (col, buf) in &cols {
        if visit(col - rect.col_start, buf).is_break() {
            return Ok(());
        }
    }
    Ok(())
}

/// Used-extent COLUMN walk of a resolved defined name (a bare ref or a range),
/// mirroring [`used_rows_of_resolved_name`] with the axes swapped.
fn used_cols_of_resolved_name(
    resolved: &Expr,
    cur: SheetId,
    values: &ValueStore,
    env: &WbIndex,
    visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
) -> Result<(), ErrorKind> {
    match &resolved.kind {
        ExprKind::Binary {
            op: BinaryOp::Range,
            lhs,
            rhs,
        } => match resolve_range(lhs, rhs, cur, env) {
            Some(sr) => used_cols_of_range(&sr, values, visit),
            None => Err(ErrorKind::Unsupported),
        },
        ExprKind::Ref(r) => match resolve_reference(r, cur, env) {
            RefResolution::Cell(c) => {
                let v = values.get(&c).cloned().unwrap_or(Value::Blank);
                let _ = visit(0, std::slice::from_ref(&v));
                Ok(())
            }
            RefResolution::Unsupported => Err(ErrorKind::Unsupported),
        },
        _ => Err(ErrorKind::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xl_value::RectRange;

    fn store(cells: &[(u32, u32, Value)]) -> ValueStore {
        let mut m: ValueStore = BTreeMap::new();
        for (row, col, v) in cells {
            m.insert(CellId::new(SheetId(0), *row, *col), v.clone());
        }
        m
    }

    fn sr(row_start: u32, row_end: u32, col_start: u32, col_end: u32) -> SheetRange {
        SheetRange::new(
            SheetId(0),
            RectRange::new(row_start, row_end, col_start, col_end),
        )
    }

    // `rows_of_range` surfaces absent cells as `Value::Blank` at their column
    // positions (unlike the blank-eliding `stream_range`).
    #[test]
    fn rows_of_range_yields_blanks_positionally() {
        // A 2×2 rectangle A1:B2 with holes at B1 and A2.
        let values = store(&[
            (0, 0, Value::number(1.0)), // A1
            (1, 1, Value::number(4.0)), // B2
        ]);
        let mut seen: Vec<Vec<Value>> = Vec::new();
        let res = rows_of_range(&sr(0, 1, 0, 1), &values, &mut |row| {
            seen.push(row.to_vec());
            ControlFlow::Continue(())
        });
        assert_eq!(res, Ok(()));
        assert_eq!(
            seen,
            vec![
                vec![Value::number(1.0), Value::Blank],
                vec![Value::Blank, Value::number(4.0)],
            ]
        );
    }

    // `ControlFlow::Break` stops the dense walk after the current row.
    #[test]
    fn rows_of_range_early_termination() {
        let values = store(&[
            (0, 0, Value::number(1.0)),
            (1, 0, Value::number(2.0)),
            (2, 0, Value::number(3.0)),
        ]);
        let mut visited = 0usize;
        let res = rows_of_range(&sr(0, 2, 0, 0), &values, &mut |_| {
            visited += 1;
            ControlFlow::Break(())
        });
        assert_eq!(res, Ok(()));
        assert_eq!(visited, 1, "stopped after the first row");
    }

    // Documented whole-column policy: an unbounded range refuses the dense walk.
    #[test]
    fn rows_of_range_refuses_unbounded_whole_column() {
        let values = store(&[(0, 0, Value::number(1.0))]);
        // A:A → rows 0..=MAX_ROW0, one column.
        let whole_col = sr(0, MAX_ROW0, 0, 0);
        assert!(is_unbounded(&whole_col));
        let mut visited = 0usize;
        let res = rows_of_range(&whole_col, &values, &mut |_| {
            visited += 1;
            ControlFlow::Continue(())
        });
        assert_eq!(res, Err(ErrorKind::Unsupported));
        assert_eq!(visited, 0, "no row is materialized for a refused range");
    }

    // `stream_range` (the sparse path) honors `ControlFlow::Break` mid-scan.
    #[test]
    fn stream_range_early_termination() {
        let values = store(&[
            (0, 0, Value::number(1.0)),
            (1, 0, Value::Error(ErrorKind::Div0)),
            (2, 0, Value::number(3.0)),
        ]);
        let mut seen: Vec<Value> = Vec::new();
        stream_range(&sr(0, 2, 0, 0), &values, &mut |v| {
            seen.push(v.clone());
            if matches!(v, Value::Error(_)) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        assert_eq!(
            seen,
            vec![Value::number(1.0), Value::Error(ErrorKind::Div0)],
            "scan stopped at the error; the third cell was never visited"
        );
    }

    #[test]
    fn bounded_dims_reports_extent_and_refuses_unbounded() {
        // Bounded A1:C2 → 2 rows × 3 cols.
        assert_eq!(bounded_dims(&sr(0, 1, 0, 2)), Some((2, 3)));
        // Whole column and whole row are unbounded → None.
        assert_eq!(bounded_dims(&sr(0, MAX_ROW0, 0, 0)), None);
        assert_eq!(bounded_dims(&sr(5, 5, 0, MAX_COL0)), None);
    }

    #[test]
    fn is_unbounded_detects_full_sheet_axes() {
        assert!(is_unbounded(&sr(0, MAX_ROW0, 3, 3))); // whole column
        assert!(is_unbounded(&sr(7, 7, 0, MAX_COL0))); // whole row
        assert!(!is_unbounded(&sr(0, 99, 0, 0))); // large but bounded
    }

    // ---- legacy implicit intersection (OXP-004/163) ----------------------

    // A single-COLUMN range in scalar context intersects on the formula's ROW
    // (RUN-2026-07-11-oracle01, OXP-163): A1:A3 = [10,20,30]; a formula anchored
    // in row 0/1/2 reads A1/A2/A3, and a row outside [0,2] is #VALUE!.
    #[test]
    fn implicit_intersect_single_column_picks_anchor_row() {
        let values = store(&[
            (0, 0, Value::number(10.0)),
            (1, 0, Value::number(20.0)),
            (2, 0, Value::number(30.0)),
        ]);
        let range = sr(0, 2, 0, 0); // A1:A3
        let mut diags: crate::DiagnosticSink = Vec::new();
        for (row, expected) in [(0u32, 10.0), (1, 20.0), (2, 30.0)] {
            let anchor = CellId::new(SheetId(0), row, 1); // a cell in that row
            assert_eq!(
                implicit_intersect(&range, anchor, &values, &mut diags),
                Value::number(expected),
                "anchor row {row} intersects the single-column range"
            );
        }
        // A formula row OUTSIDE [0,2] → #VALUE! (OXP-163: row 10).
        let outside = CellId::new(SheetId(0), 9, 1);
        assert_eq!(
            implicit_intersect(&range, outside, &values, &mut diags),
            Value::Error(ErrorKind::Value)
        );
        assert!(diags.is_empty(), "the 1-D cases push no diagnostic");
    }

    // A single-ROW range intersects on the formula's COLUMN (the symmetric rule).
    #[test]
    fn implicit_intersect_single_row_picks_anchor_column() {
        // Row 5: A6/B6/C6 = 10/20/30 (cols 0,1,2).
        let values = store(&[
            (5, 0, Value::number(10.0)),
            (5, 1, Value::number(20.0)),
            (5, 2, Value::number(30.0)),
        ]);
        let range = sr(5, 5, 0, 2); // A6:C6
        let mut diags: crate::DiagnosticSink = Vec::new();
        for (col, expected) in [(0u32, 10.0), (1, 20.0), (2, 30.0)] {
            let anchor = CellId::new(SheetId(0), 0, col);
            assert_eq!(
                implicit_intersect(&range, anchor, &values, &mut diags),
                Value::number(expected)
            );
        }
        // A column outside [0,2] → #VALUE!.
        let outside = CellId::new(SheetId(0), 0, 4);
        assert_eq!(
            implicit_intersect(&range, outside, &values, &mut diags),
            Value::Error(ErrorKind::Value)
        );
    }

    // A 1×1 range lifts to its cell regardless of the anchor.
    #[test]
    fn implicit_intersect_1x1_lifts_to_cell() {
        let values = store(&[(2, 3, Value::number(7.0))]); // D3
        let range = sr(2, 2, 3, 3);
        let mut diags: crate::DiagnosticSink = Vec::new();
        let anchor = CellId::new(SheetId(0), 99, 99);
        assert_eq!(
            implicit_intersect(&range, anchor, &values, &mut diags),
            Value::number(7.0)
        );
        assert!(diags.is_empty());
    }

    // A 2-D range (both axes > 1) in scalar context is unprobed → #UNSUPPORTED!.
    #[test]
    fn implicit_intersect_2d_defers_to_unsupported() {
        let values = store(&[(0, 0, Value::number(1.0))]);
        let range = sr(0, 1, 0, 1); // A1:B2
        let mut diags: crate::DiagnosticSink = Vec::new();
        let anchor = CellId::new(SheetId(0), 0, 0);
        assert_eq!(
            implicit_intersect(&range, anchor, &values, &mut diags),
            Value::Error(ErrorKind::Unsupported)
        );
        assert_eq!(diags.len(), 1, "the deferral records a diagnostic");
    }

    // ---- EngineArgs through the CallArgs trait ---------------------------
    //
    // Exercises `dims`/`for_each_row`/`for_each_cell` exactly as a Tier-0
    // function would see them: over parsed argument expressions and a value
    // store, via `dyn CallArgs`.

    /// Parse `srcs` as the argument expressions of a call and run `f` on the
    /// resulting `EngineArgs` (as `&mut dyn CallArgs`).
    fn with_engine_args<R>(
        srcs: &[&str],
        values: &ValueStore,
        f: impl FnOnce(&mut dyn CallArgs) -> R,
    ) -> R {
        let exprs: Vec<Expr> = srcs
            .iter()
            .map(|s| xl_ast::parse(s).expect("test arg parses"))
            .collect();
        let sheet_ids: BTreeMap<String, SheetId> =
            [("sheet1".to_string(), SheetId(0))].into_iter().collect();
        let no_subtotals = std::collections::BTreeSet::new();
        let no_hidden_rows = std::collections::BTreeSet::new();
        let no_spills = std::collections::BTreeMap::new();
        let env = WbIndex {
            sheet_ids: &sheet_ids,
            defined_names: &[],
            subtotals: &no_subtotals,
            hidden_rows: &no_hidden_rows,
            spills: &no_spills,
            is_array_formula: false,
        };
        let ctx = EvalContext::new();
        let mut diags: crate::DiagnosticSink = Vec::new();
        let mut ea = EngineArgs {
            args: &exprs,
            anchor: CellId::new(SheetId(0), 0, 0),
            cur: SheetId(0),
            values,
            env: &env,
            ctx: &ctx,
            diags: &mut diags,
            array_arg_ctx: false,
            lex: None,
            consumed_array_seen: false,
            consumed_array_had_refusal: false,
        };
        f(&mut ea)
    }

    /// Like [`with_engine_args`] but seeds the RFC 0002 SUBTOTAL tag set and the
    /// OXP-121 hidden-row set so [`CallArgs::for_each_cell_tagged`] can flag
    /// cells as nested sub-totals and/or hidden-row members.
    fn with_engine_args_tagged<R>(
        srcs: &[&str],
        values: &ValueStore,
        subtotals: &std::collections::BTreeSet<CellId>,
        hidden_rows: &std::collections::BTreeSet<(SheetId, u32)>,
        f: impl FnOnce(&mut dyn CallArgs) -> R,
    ) -> R {
        let exprs: Vec<Expr> = srcs
            .iter()
            .map(|s| xl_ast::parse(s).expect("test arg parses"))
            .collect();
        let sheet_ids: BTreeMap<String, SheetId> =
            [("sheet1".to_string(), SheetId(0))].into_iter().collect();
        let no_spills = std::collections::BTreeMap::new();
        let env = WbIndex {
            sheet_ids: &sheet_ids,
            defined_names: &[],
            subtotals,
            hidden_rows,
            spills: &no_spills,
            is_array_formula: false,
        };
        let ctx = EvalContext::new();
        let mut diags: crate::DiagnosticSink = Vec::new();
        let mut ea = EngineArgs {
            args: &exprs,
            anchor: CellId::new(SheetId(0), 0, 0),
            cur: SheetId(0),
            values,
            env: &env,
            ctx: &ctx,
            diags: &mut diags,
            array_arg_ctx: false,
            lex: None,
            consumed_array_seen: false,
            consumed_array_had_refusal: false,
        };
        f(&mut ea)
    }

    #[test]
    fn engine_args_for_each_cell_tagged_flags_subtotal_cells() {
        use std::collections::BTreeSet;
        // A1=10, A2=30 (a sub-total), A3=20 in a single column.
        let values = store(&[
            (0, 0, Value::number(10.0)),
            (1, 0, Value::number(30.0)),
            (2, 0, Value::number(20.0)),
        ]);
        // Tag A2 (row 1) as a SUBTOTAL cell.
        let mut subtotals = BTreeSet::new();
        subtotals.insert(CellId::new(SheetId(0), 1, 0));
        let no_hidden = BTreeSet::new();

        with_engine_args_tagged(&["A1:A3"], &values, &subtotals, &no_hidden, |args| {
            let mut seen: Vec<(f64, bool)> = Vec::new();
            let res = args.for_each_cell_tagged(0, &mut |v, flags| {
                if let Value::Number(n) = v {
                    seen.push((*n, flags.is_subtotal));
                }
                ControlFlow::Continue(())
            });
            assert_eq!(res, Ok(()), "a resolvable range is served, not refused");
            assert_eq!(
                seen,
                vec![(10.0, false), (30.0, true), (20.0, false)],
                "only the tagged A2 reports is_subtotal"
            );
        });
    }

    #[test]
    fn engine_args_for_each_cell_tagged_flags_hidden_rows() {
        use std::collections::BTreeSet;
        // A1:A5 = 10,20,30,40,50 in one column; row 3 (0-based 2) is hidden —
        // the OXP-121 layout. The tagged walk must report is_hidden_row only for
        // the cell in the hidden row (RUN-2026-07-11-oracle01).
        let values = store(&[
            (0, 0, Value::number(10.0)),
            (1, 0, Value::number(20.0)),
            (2, 0, Value::number(30.0)),
            (3, 0, Value::number(40.0)),
            (4, 0, Value::number(50.0)),
        ]);
        let no_subtotals = BTreeSet::new();
        let mut hidden = BTreeSet::new();
        hidden.insert((SheetId(0), 2u32)); // row 3, 0-based 2

        with_engine_args_tagged(&["A1:A5"], &values, &no_subtotals, &hidden, |args| {
            let mut seen: Vec<(f64, bool, bool)> = Vec::new();
            let res = args.for_each_cell_tagged(0, &mut |v, flags| {
                if let Value::Number(n) = v {
                    seen.push((*n, flags.is_subtotal, flags.is_hidden_row));
                }
                ControlFlow::Continue(())
            });
            assert_eq!(res, Ok(()));
            assert_eq!(
                seen,
                vec![
                    (10.0, false, false),
                    (20.0, false, false),
                    (30.0, false, true),
                    (40.0, false, false),
                    (50.0, false, false),
                ],
                "only the cell in hidden row 3 reports is_hidden_row"
            );
        });
    }

    #[test]
    fn engine_args_for_each_cell_tagged_single_ref_and_array() {
        use std::collections::BTreeSet;
        let values = store(&[(0, 0, Value::number(42.0))]);
        let mut subtotals = BTreeSet::new();
        subtotals.insert(CellId::new(SheetId(0), 0, 0)); // A1 is a sub-total
        let no_hidden = BTreeSet::new();

        with_engine_args_tagged(
            &["A1", "{1,2,3}"],
            &values,
            &subtotals,
            &no_hidden,
            |args| {
                // A bare single-cell ref reports its own provenance.
                let mut ref_seen: Vec<(f64, bool)> = Vec::new();
                let res = args.for_each_cell_tagged(0, &mut |v, flags| {
                    if let Value::Number(n) = v {
                        ref_seen.push((*n, flags.is_subtotal));
                    }
                    ControlFlow::Continue(())
                });
                assert_eq!(res, Ok(()));
                assert_eq!(
                    ref_seen,
                    vec![(42.0, true)],
                    "the tagged single ref A1 flags true"
                );

                // An array constant carries no provenance → refuse (Err(())), so the
                // caller falls back to the plain walk.
                let res = args.for_each_cell_tagged(1, &mut |_, _| ControlFlow::Continue(()));
                assert_eq!(res, Err(()), "array constant refuses the tagged walk");
            },
        );
    }

    #[test]
    fn engine_args_dims_scalar_range_array_whole_column() {
        let values = store(&[]);
        with_engine_args(&["7", "A1:C2", "{1,2,3}", "A:A", "1:1"], &values, |args| {
            assert_eq!(args.dims(0), None, "scalar literal has no rectangle");
            assert_eq!(args.dims(1), Some((2, 3)), "A1:C2 is 2 rows x 3 cols");
            assert_eq!(args.dims(2), Some((1, 3)), "{{1,2,3}} is a 1x3 row");
            assert_eq!(args.dims(3), None, "whole column is unbounded (policy)");
            assert_eq!(args.dims(4), None, "whole row is unbounded (policy)");
            assert_eq!(args.dims(9), None, "out-of-range index");
        });
    }

    #[test]
    fn engine_args_for_each_row_range_with_holes_yields_blanks() {
        // A1=1, B2=4; A2/B1 never populated.
        let values = store(&[(0, 0, Value::number(1.0)), (1, 1, Value::number(4.0))]);
        with_engine_args(&["A1:B2"], &values, |args| {
            let mut seen: Vec<Vec<Value>> = Vec::new();
            let res = args.for_each_row(0, &mut |row| {
                seen.push(row.to_vec());
                ControlFlow::Continue(())
            });
            assert_eq!(res, Ok(()));
            assert_eq!(
                seen,
                vec![
                    vec![Value::number(1.0), Value::Blank],
                    vec![Value::Blank, Value::number(4.0)],
                ]
            );
        });
    }

    #[test]
    fn engine_args_for_each_row_whole_column_refused_scalar_is_1x1() {
        let values = store(&[(0, 0, Value::number(1.0))]);
        with_engine_args(&["A:A", "42"], &values, |args| {
            // Whole-column dense walk is refused per the documented policy.
            let res = args.for_each_row(0, &mut |_| ControlFlow::Continue(()));
            assert_eq!(res, Err(ErrorKind::Unsupported));
            // A scalar argument walks as a single 1x1 row.
            let mut seen: Vec<Vec<Value>> = Vec::new();
            let res = args.for_each_row(1, &mut |row| {
                seen.push(row.to_vec());
                ControlFlow::Continue(())
            });
            assert_eq!(res, Ok(()));
            assert_eq!(seen, vec![vec![Value::number(42.0)]]);
        });
    }

    #[test]
    fn engine_args_for_each_row_array_literal_rows() {
        let values = store(&[]);
        with_engine_args(&["{1,2;3,4}"], &values, |args| {
            assert_eq!(args.dims(0), Some((2, 2)));
            let mut seen: Vec<Vec<Value>> = Vec::new();
            let res = args.for_each_row(0, &mut |row| {
                seen.push(row.to_vec());
                ControlFlow::Continue(())
            });
            assert_eq!(res, Ok(()));
            assert_eq!(
                seen,
                vec![
                    vec![Value::number(1.0), Value::number(2.0)],
                    vec![Value::number(3.0), Value::number(4.0)],
                ]
            );
        });
    }

    #[test]
    fn engine_args_for_each_cell_short_circuits_on_break() {
        let values = store(&[
            (0, 0, Value::number(1.0)),
            (1, 0, Value::number(2.0)),
            (2, 0, Value::number(3.0)),
        ]);
        with_engine_args(&["A1:A3"], &values, |args| {
            let mut visited = 0usize;
            args.for_each_cell(0, &mut |_| {
                visited += 1;
                ControlFlow::Break(())
            });
            assert_eq!(visited, 1, "break stops the sparse stream immediately");
        });
    }

    // ---- used_rows_of_range (RFC 0001) -----------------------------------

    // A whole-column A:B range yields only its populated rows, in ascending
    // order, each as a full-column-span slice with blanks within the span, and
    // the relative row index (== absolute here, since the range top is row 0).
    #[test]
    fn used_rows_of_range_whole_column_populated_only_in_order() {
        // A1=1, B4=40, A8=8/B8=80. Rows 0, 3, 7 populated within cols A..B.
        let values = store(&[
            (0, 0, Value::number(1.0)),
            (3, 1, Value::number(40.0)),
            (7, 0, Value::number(8.0)),
            (7, 1, Value::number(80.0)),
        ]);
        let whole = sr(0, MAX_ROW0, 0, 1); // A:B
        assert!(is_unbounded(&whole));
        let mut seen: Vec<(u32, Vec<Value>)> = Vec::new();
        let res = used_rows_of_range(&whole, &values, &mut |rel, row| {
            seen.push((rel, row.to_vec()));
            ControlFlow::Continue(())
        });
        assert_eq!(res, Ok(()));
        assert_eq!(
            seen,
            vec![
                (0, vec![Value::number(1.0), Value::Blank]),
                (3, vec![Value::Blank, Value::number(40.0)]),
                (7, vec![Value::number(8.0), Value::number(80.0)]),
            ]
        );
        assert!(seen.windows(2).all(|w| w[0].0 < w[1].0), "ascending order");
    }

    // A bounded range yields relative row indices (row - row_start) for its
    // populated rows only (the used-extent semantics, distinct from the dense
    // for_each_row which surfaces every row).
    #[test]
    fn used_rows_of_range_bounded_relative_indices() {
        // Range B3:B10 (rows 2..9, col 1). Populate B3 and B7.
        let values = store(&[(2, 1, Value::number(3.0)), (6, 1, Value::number(7.0))]);
        let bounded = sr(2, 9, 1, 1);
        let mut seen: Vec<(u32, Vec<Value>)> = Vec::new();
        let res = used_rows_of_range(&bounded, &values, &mut |rel, row| {
            seen.push((rel, row.to_vec()));
            ControlFlow::Continue(())
        });
        assert_eq!(res, Ok(()));
        // B3 is relative row 0, B7 is relative row 4.
        assert_eq!(
            seen,
            vec![(0, vec![Value::number(3.0)]), (4, vec![Value::number(7.0)])]
        );
    }

    // A whole-ROW range (unbounded columns) is refused — the row iterator serves
    // whole-column ranges only.
    #[test]
    fn used_rows_of_range_refuses_whole_row() {
        let values = store(&[(5, 0, Value::number(1.0))]);
        let whole_row = sr(5, 5, 0, MAX_COL0); // 6:6
        let res = used_rows_of_range(&whole_row, &values, &mut |_, _| ControlFlow::Continue(()));
        assert_eq!(res, Err(ErrorKind::Unsupported));
    }

    // ControlFlow::Break stops the used-extent walk.
    #[test]
    fn used_rows_of_range_early_break() {
        let values = store(&[
            (0, 0, Value::number(1.0)),
            (1, 0, Value::number(2.0)),
            (2, 0, Value::number(3.0)),
        ]);
        let whole = sr(0, MAX_ROW0, 0, 0);
        let mut visited = 0usize;
        let res = used_rows_of_range(&whole, &values, &mut |_, _| {
            visited += 1;
            ControlFlow::Break(())
        });
        assert_eq!(res, Ok(()));
        assert_eq!(visited, 1, "break stops after the first populated row");
    }

    // Through the CallArgs trait: EngineArgs::for_each_used_row over a parsed
    // whole-column reference resolves and yields populated rows.
    #[test]
    fn engine_args_for_each_used_row_whole_column() {
        let values = store(&[(0, 0, Value::number(1.0)), (2, 0, Value::number(3.0))]);
        with_engine_args(&["A:A"], &values, |args| {
            let mut seen: Vec<(u32, Vec<Value>)> = Vec::new();
            let res = args.for_each_used_row(0, &mut |rel, row| {
                seen.push((rel, row.to_vec()));
                ControlFlow::Continue(())
            });
            assert_eq!(res, Ok(()));
            assert_eq!(
                seen,
                vec![(0, vec![Value::number(1.0)]), (2, vec![Value::number(3.0)])]
            );
        });
    }

    // ---- used_cols_of_range (RFC 0008) -----------------------------------

    // RFC 0008 §5(a): a (sheet,row,col)-ordered band scan is row-major, so it
    // encounters columns OUT of order (col 5's cell in row 0 is scanned BEFORE
    // col 2's cell in row 1); the buffered emit must restore ascending column
    // order. This is the one non-mechanical part of the transpose (§2).
    #[test]
    fn used_cols_of_range_ascending_order_from_row_major_scan() {
        // 1:2 band (rows 0..1, all columns). Only (row0,col5) and (row1,col2).
        let values = store(&[(0, 5, Value::number(50.0)), (1, 2, Value::number(21.0))]);
        let whole = sr(0, 1, 0, MAX_COL0); // whole-row band (unbounded columns)
        assert!(is_unbounded(&whole));
        let mut seen: Vec<(u32, Vec<Value>)> = Vec::new();
        let res = used_cols_of_range(&whole, &values, &mut |rel, col| {
            seen.push((rel, col.to_vec()));
            ControlFlow::Continue(())
        });
        assert_eq!(res, Ok(()));
        // col_start == 0 → relative col == absolute col. Ascending: col 2 first,
        // then col 5 — despite col 5's cell being scanned first.
        assert_eq!(
            seen,
            vec![
                (2, vec![Value::Blank, Value::number(21.0)]),
                (5, vec![Value::number(50.0), Value::Blank]),
            ]
        );
        assert!(
            seen.windows(2).all(|w| w[0].0 < w[1].0),
            "ascending column order"
        );
    }

    // RFC 0008 §5 hardening (the acceptance-review nit): the column walk must
    // buffer every populated column at once (to sort by column), so a
    // pathological tall band (`2:1048576`, height ≈ 1M) could allocate GBs. The
    // cap fails loud with `#RESOURCE!` after ≤ ~one column instead of OOMing. A
    // real whole-row band (a few rows) never approaches it.
    #[test]
    fn used_cols_of_range_caps_pathological_tall_band() {
        // Rows 1..=MAX_ROW0 (height ≈ 1M), NOT whole-column (row_start != 0), so
        // it reaches the column buffer. Two populated columns → the second
        // `height`-tall allocation exceeds the ~1M-Value cap.
        let values = store(&[(1, 2, Value::number(1.0)), (1, 5, Value::number(2.0))]);
        let tall = sr(1, MAX_ROW0, 0, MAX_COL0);
        let mut emitted = 0;
        let res = used_cols_of_range(&tall, &values, &mut |_, _| {
            emitted += 1;
            ControlFlow::Continue(())
        });
        assert_eq!(res, Err(ErrorKind::Resource));
        assert_eq!(emitted, 0, "bails during buffering, before any emit");
    }

    // RFC 0008 §5(b): a multi-row whole-row band groups a column's cells across
    // rows into one row-span-tall slice, blanks filled for absent rows.
    #[test]
    fn used_cols_of_range_multi_row_band_groups_by_column() {
        // 1:2 band. col 3 populated in BOTH rows; col 1 only in row 1; col 7 in row 0.
        let values = store(&[
            (0, 3, Value::number(3.0)),
            (1, 3, Value::number(30.0)),
            (1, 1, Value::number(11.0)),
            (0, 7, Value::number(7.0)),
        ]);
        let whole = sr(0, 1, 0, MAX_COL0);
        let mut seen: Vec<(u32, Vec<Value>)> = Vec::new();
        let res = used_cols_of_range(&whole, &values, &mut |rel, col| {
            seen.push((rel, col.to_vec()));
            ControlFlow::Continue(())
        });
        assert_eq!(res, Ok(()));
        assert_eq!(
            seen,
            vec![
                (1, vec![Value::Blank, Value::number(11.0)]),
                (3, vec![Value::number(3.0), Value::number(30.0)]),
                (7, vec![Value::number(7.0), Value::Blank]),
            ]
        );
    }

    // A bounded range yields relative column indices (col - col_start) for its
    // populated columns only — the column transpose of the used-extent row rule.
    #[test]
    fn used_cols_of_range_bounded_relative_indices() {
        // Range C1:H1 (row 0, cols 2..7). Populate C1 (col 2) and G1 (col 6).
        let values = store(&[(0, 2, Value::number(3.0)), (0, 6, Value::number(7.0))]);
        let bounded = sr(0, 0, 2, 7);
        let mut seen: Vec<(u32, Vec<Value>)> = Vec::new();
        let res = used_cols_of_range(&bounded, &values, &mut |rel, col| {
            seen.push((rel, col.to_vec()));
            ControlFlow::Continue(())
        });
        assert_eq!(res, Ok(()));
        // C1 is relative col 0, G1 is relative col 4.
        assert_eq!(
            seen,
            vec![(0, vec![Value::number(3.0)]), (4, vec![Value::number(7.0)])]
        );
    }

    // A whole-COLUMN range (unbounded rows) is refused — the column iterator
    // serves whole-row ranges only (the dual of used_rows_of_range's refusal).
    #[test]
    fn used_cols_of_range_refuses_whole_column() {
        let values = store(&[(0, 5, Value::number(1.0))]);
        let whole_col = sr(0, MAX_ROW0, 5, 5); // F:F
        let res = used_cols_of_range(&whole_col, &values, &mut |_, _| ControlFlow::Continue(()));
        assert_eq!(res, Err(ErrorKind::Unsupported));
    }

    // ControlFlow::Break stops the used-extent column walk.
    #[test]
    fn used_cols_of_range_early_break() {
        let values = store(&[
            (0, 0, Value::number(1.0)),
            (0, 1, Value::number(2.0)),
            (0, 2, Value::number(3.0)),
        ]);
        let whole = sr(0, 0, 0, MAX_COL0);
        let mut visited = 0usize;
        let res = used_cols_of_range(&whole, &values, &mut |_, _| {
            visited += 1;
            ControlFlow::Break(())
        });
        assert_eq!(res, Ok(()));
        assert_eq!(visited, 1, "break stops after the first populated column");
    }

    // Through the CallArgs trait: EngineArgs::for_each_used_col over a parsed
    // whole-row reference (`1:1`) resolves and yields populated columns; a
    // whole-column reference (`A:A`) refuses (that is for_each_used_row's job).
    #[test]
    fn engine_args_for_each_used_col_whole_row_and_refuses_whole_column() {
        let values = store(&[(0, 0, Value::number(1.0)), (0, 2, Value::number(3.0))]);
        with_engine_args(&["1:1", "A:A"], &values, |args| {
            let mut seen: Vec<(u32, Vec<Value>)> = Vec::new();
            let res = args.for_each_used_col(0, &mut |rel, col| {
                seen.push((rel, col.to_vec()));
                ControlFlow::Continue(())
            });
            assert_eq!(res, Ok(()));
            assert_eq!(
                seen,
                vec![(0, vec![Value::number(1.0)]), (2, vec![Value::number(3.0)])]
            );
            // A whole-column reference is refused by the column iterator.
            let res = args.for_each_used_col(1, &mut |_, _| ControlFlow::Continue(()));
            assert_eq!(res, Err(ErrorKind::Unsupported));
        });
    }

    // ---- RFC 0005 reference-position channel -----------------------------

    /// Like [`with_engine_args`] but with an explicit calling-cell anchor, so
    /// the RFC-0005 `anchor()` channel can be exercised with a non-trivial cell.
    fn with_engine_args_at<R>(
        srcs: &[&str],
        anchor: CellId,
        values: &ValueStore,
        f: impl FnOnce(&mut dyn CallArgs) -> R,
    ) -> R {
        let exprs: Vec<Expr> = srcs
            .iter()
            .map(|s| xl_ast::parse(s).expect("test arg parses"))
            .collect();
        let sheet_ids: BTreeMap<String, SheetId> =
            [("sheet1".to_string(), SheetId(0))].into_iter().collect();
        let no_subtotals = std::collections::BTreeSet::new();
        let no_hidden_rows = std::collections::BTreeSet::new();
        let no_spills = std::collections::BTreeMap::new();
        let env = WbIndex {
            sheet_ids: &sheet_ids,
            defined_names: &[],
            subtotals: &no_subtotals,
            hidden_rows: &no_hidden_rows,
            spills: &no_spills,
            is_array_formula: false,
        };
        let ctx = EvalContext::new();
        let mut diags: crate::DiagnosticSink = Vec::new();
        let mut ea = EngineArgs {
            args: &exprs,
            anchor,
            cur: SheetId(0),
            values,
            env: &env,
            ctx: &ctx,
            diags: &mut diags,
            array_arg_ctx: false,
            lex: None,
            consumed_array_seen: false,
            consumed_array_had_refusal: false,
        };
        f(&mut ea)
    }

    // `anchor()` surfaces the calling cell's own (sheet, row, col).
    #[test]
    fn engine_args_anchor_reports_calling_cell() {
        let values = store(&[]);
        with_engine_args_at(&[], CellId::new(SheetId(0), 6, 2), &values, |args| {
            assert_eq!(args.anchor(), Some((SheetId(0), 6, 2)));
        });
    }

    // `arg_ref_extent` reports a reference's position + full extent (the whole
    // sheet-axis size for `A:A`/`1:1`), and `None` for a non-reference.
    #[test]
    fn engine_args_arg_ref_extent_cells_ranges_and_whole_axis() {
        let values = store(&[]);
        with_engine_args(&["B3", "A1:C4", "A:A", "1:1", "5"], &values, |args| {
            // B3 -> 0-based (row 2, col 1), 1x1.
            assert_eq!(
                args.arg_ref_extent(0),
                Some(RefRect {
                    row: 2,
                    col: 1,
                    height: 1,
                    width: 1
                })
            );
            // A1:C4 -> top-left (0,0), 4 rows x 3 cols.
            assert_eq!(
                args.arg_ref_extent(1),
                Some(RefRect {
                    row: 0,
                    col: 0,
                    height: 4,
                    width: 3
                })
            );
            // A:A -> whole column: full row extent (1,048,576), 1 col.
            assert_eq!(
                args.arg_ref_extent(2),
                Some(RefRect {
                    row: 0,
                    col: 0,
                    height: MAX_ROW0 + 1,
                    width: 1
                })
            );
            // 1:1 -> whole row: 1 row, full column extent (16,384).
            assert_eq!(
                args.arg_ref_extent(3),
                Some(RefRect {
                    row: 0,
                    col: 0,
                    height: 1,
                    width: MAX_COL0 + 1
                })
            );
            // A literal has no reference address.
            assert_eq!(args.arg_ref_extent(4), None);
        });
    }

    // RFC 0005 decision: `arg_ref_extent` resolves a *computed* reference
    // (OFFSET/INDIRECT) via the RFC-0003 resolver. `OFFSET(A1,2,3)` -> D3.
    #[test]
    fn engine_args_arg_ref_extent_resolves_computed_offset() {
        let values = store(&[]);
        with_engine_args(&["OFFSET(A1,2,3)"], &values, |args| {
            assert_eq!(
                args.arg_ref_extent(0),
                Some(RefRect {
                    row: 2,
                    col: 3,
                    height: 1,
                    width: 1
                })
            );
        });
    }

    // End-to-end through the registry: ROW/COLUMN over the real engine seam.
    #[test]
    fn registry_row_column_through_engine_args() {
        let values = store(&[]);
        let ctx = EvalContext::new();
        let row = xl_fn::lookup("ROW").expect("ROW registered");
        let col = xl_fn::lookup("COLUMN").expect("COLUMN registered");
        // No-arg at anchor (row 6, col 2): ROW()=7, COLUMN()=3.
        with_engine_args_at(&[], CellId::new(SheetId(0), 6, 2), &values, |args| {
            assert_eq!((row.eval)(&ctx, args), Value::number(7.0));
        });
        with_engine_args_at(&[], CellId::new(SheetId(0), 6, 2), &values, |args| {
            assert_eq!((col.eval)(&ctx, args), Value::number(3.0));
        });
        // ROW(B3)=3, COLUMN(B3)=2 (top row / left column, 1-based).
        with_engine_args(&["B3"], &values, |args| {
            assert_eq!((row.eval)(&ctx, args), Value::number(3.0));
        });
        with_engine_args(&["B3"], &values, |args| {
            assert_eq!((col.eval)(&ctx, args), Value::number(2.0));
        });
        // OXP-167 (RUN-2026-07-11-oracle01): ROW(A1:A5) over a multi-row
        // reference returns the range's TOP row (1); COLUMN(A1:C1) over a
        // multi-column reference returns its LEFT column (1, by symmetry).
        with_engine_args(&["A1:A5"], &values, |args| {
            assert_eq!((row.eval)(&ctx, args), Value::number(1.0));
        });
        with_engine_args(&["A1:C1"], &values, |args| {
            assert_eq!((col.eval)(&ctx, args), Value::number(1.0));
        });
    }

    // OXP-116 (RUN-2026-07-11-oracle01): ROWS/COLUMNS over a whole-column/row
    // reference, resolved end-to-end through the engine reference resolver and
    // the RFC-0005 arg_ref_extent channel.
    #[test]
    fn registry_rows_columns_whole_axis_through_engine_args() {
        let values = store(&[]);
        let ctx = EvalContext::new();
        let rows = xl_fn::lookup("ROWS").expect("ROWS registered");
        let cols = xl_fn::lookup("COLUMNS").expect("COLUMNS registered");
        // ROWS(A:A) = 1,048,576 ; ROWS(1:1) = 1.
        with_engine_args(&["A:A"], &values, |args| {
            assert_eq!((rows.eval)(&ctx, args), Value::number(1_048_576.0));
        });
        with_engine_args(&["1:1"], &values, |args| {
            assert_eq!((rows.eval)(&ctx, args), Value::number(1.0));
        });
        // COLUMNS(A:A) = 1 ; COLUMNS(1:1) = 16,384.
        with_engine_args(&["A:A"], &values, |args| {
            assert_eq!((cols.eval)(&ctx, args), Value::number(1.0));
        });
        with_engine_args(&["1:1"], &values, |args| {
            assert_eq!((cols.eval)(&ctx, args), Value::number(16_384.0));
        });
    }

    // ---- RFC 0006 in-rectangle absolute read (OXP-105/106) ---------------

    // `cell_at` reads one cell at an absolute position confined to the
    // argument's declared rectangle; out-of-rect → None, absent-in-rect → Blank.
    #[test]
    fn engine_args_cell_at_confined_to_declared_rectangle() {
        // B2=5 inside the range B2:C3 (rows 1..2, cols 1..2).
        let values = store(&[(1, 1, Value::number(5.0))]);
        with_engine_args(&["B2:C3"], &values, |args| {
            // Populated cell inside the rect.
            assert_eq!(args.cell_at(0, 1, 1), Some(Value::number(5.0)));
            // Absent cell inside the rect reads as Blank, not None.
            assert_eq!(args.cell_at(0, 2, 2), Some(Value::Blank));
            // Outside the declared rectangle → None (the read is confined).
            assert_eq!(args.cell_at(0, 0, 0), None);
            assert_eq!(args.cell_at(0, 5, 5), None);
        });
        // A literal is not a reference → None regardless of position.
        with_engine_args(&["42"], &store(&[]), |args| {
            assert_eq!(args.cell_at(0, 0, 0), None);
        });
    }

    // OXP-106 (RUN-2026-07-11-oracle01) via RFC 0006: INDEX over a whole-column
    // reference reads by absolute position — INDEX(A:A, 3) = A3 = 30 — with the
    // #REF! bound at the full sheet-axis height, and the row_num=0 spill form
    // still deferred.
    #[test]
    fn registry_index_whole_column_absolute_read() {
        let values = store(&[
            (0, 0, Value::number(10.0)),
            (1, 0, Value::number(20.0)),
            (2, 0, Value::number(30.0)),
        ]);
        let ctx = EvalContext::new();
        let index = xl_fn::lookup("INDEX").expect("INDEX registered");
        with_engine_args(&["A:A", "3"], &values, |args| {
            assert_eq!((index.eval)(&ctx, args), Value::number(30.0));
        });
        // Beyond the sheet's 1,048,576-row extent → #REF!.
        with_engine_args(&["A:A", "1048577"], &values, |args| {
            assert_eq!((index.eval)(&ctx, args), Value::Error(ErrorKind::Ref));
        });
        // row_num = 0 whole-column spill form stays deferred (v0 spill scope).
        with_engine_args(&["A:A", "0"], &values, |args| {
            assert_eq!(
                (index.eval)(&ctx, args),
                Value::Error(ErrorKind::Unsupported)
            );
        });
    }

    // OXP-105 via RFC 0006: a whole-column HLOOKUP resolves via in-rectangle
    // reads (search the first row, return from row_index) instead of
    // #UNSUPPORTED!. Table A:E — search row 1 = [1,3,5], result row 2 =
    // [10,30,50]; HLOOKUP(3, A:E, 2) matches column B and returns B2 = 30.
    #[test]
    fn registry_hlookup_whole_column_absolute_read() {
        let values = store(&[
            (0, 0, Value::number(1.0)),
            (0, 1, Value::number(3.0)),
            (0, 2, Value::number(5.0)),
            (1, 0, Value::number(10.0)),
            (1, 1, Value::number(30.0)),
            (1, 2, Value::number(50.0)),
        ]);
        let ctx = EvalContext::new();
        let hlookup = xl_fn::lookup("HLOOKUP").expect("HLOOKUP registered");
        with_engine_args(&["3", "A:E", "2"], &values, |args| {
            assert_eq!((hlookup.eval)(&ctx, args), Value::number(30.0));
        });
        // row_index beyond the sheet's row extent → #REF!.
        with_engine_args(&["3", "A:E", "1048577"], &values, |args| {
            assert_eq!((hlookup.eval)(&ctx, args), Value::Error(ErrorKind::Ref));
        });
    }
}
