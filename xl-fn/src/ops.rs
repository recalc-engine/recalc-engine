//! The scalar formula operators: binary arithmetic/comparison/concat, unary
//! `+`/`-`, and postfix `%`.
//!
//! # Provenance
//! Operator behavior is reconstructed clean-room from Microsoft Learn
//! **"Calculation operators and precedence in Excel"** and the per-operator
//! semantics that `xl-value` already froze. **All coercion routes through
//! `xl-value`** ([`to_number`], [`to_text`], [`compare`]) — this module never
//! re-implements a coercion rule (a hard rule: one coercion table, in one
//! crate). The operator *enums* are reused from `xl-ast`
//! ([`BinaryOp`]/[`UnaryOp`]/[`PostfixOp`]) rather than duplicated.
//!
//! # Error and invariant discipline
//! - Errors propagate **leftmost-first** in binary operators, matching
//!   [`compare`] and the arithmetic coercion order (operands are coerced left
//!   then right; the first error short-circuits).
//! - Division by zero yields [`ErrorKind::Div0`] (never an `Inf` value).
//! - Every arithmetic result is constructed via [`Value::number`], which maps a
//!   non-finite result to [`ErrorKind::Num`] (the frozen no-NaN/Inf invariant).
//! - The reference operators (`:` range, ` ` intersection, `,` union) are
//!   **not** scalar operators; if one reaches this module it is a caller bug and
//!   yields [`ErrorKind::Unsupported`] rather than a guess. `xl-engine` handles
//!   ranges structurally before evaluation.
//!
//! # Exponentiation edge cases — RESOLVED (`OXP-081`, RUN-2026-07-11-oracle01)
//! The `^` operator's edge cases were pinned by the oracle rather than guessed:
//! - `0^0` → `#NUM!` (not IEEE `powf`'s `1`).
//! - `0^negative` (`0^-1`, `0^-2`) → `#DIV/0!` (not `#NUM!` from an `Inf`).
//! - A **negative base** with a **non-integer** exponent returns a real
//!   (negative) result **only when the exponent is the reciprocal of an odd
//!   integer** — an odd root: `(-8)^(1/3)` = `-1.9999999999999998`
//!   (= `-(8^(1/3))`; the tiny deficit from `2` is Excel's x86 `pow`, within
//!   float tolerance of any platform's `powf`). Any other non-integer exponent
//!   (e.g. `(-2)^0.5`, an even/complex root) → `#NUM!`. The two confirming
//!   probes are `(-8)^(1/3)` (odd reciprocal `3` → real) and `(-2)^0.5` (even
//!   reciprocal `2` → `#NUM!`).
//! - Integer exponents on a negative base are ordinary powers: `(-2)^2` = `4`,
//!   `(-2)^-3` = `-0.125`.
//!
//! Ordinary powers still compute via `f64::powf`; any remaining non-finite
//! outcome becomes [`ErrorKind::Num`] through [`Value::number`].

use core::cmp::Ordering;

use xl_ast::{BinaryOp, PostfixOp, UnaryOp};
use xl_value::{Array, ErrorKind, Value, compare_fuzzy, to_number, to_text};

/// Apply a binary operator to two already-evaluated scalar operands.
///
/// See the module docs for coercion, error propagation, and the reference-operator
/// caveat.
#[must_use]
pub fn apply_binary(op: BinaryOp, lhs: &Value, rhs: &Value) -> Value {
    match op {
        BinaryOp::Add => arith(lhs, rhs, |a, b| a + b),
        BinaryOp::Sub => arith(lhs, rhs, |a, b| a - b),
        BinaryOp::Mul => arith(lhs, rhs, |a, b| a * b),
        BinaryOp::Div => divide(lhs, rhs),
        BinaryOp::Pow => power(lhs, rhs),
        BinaryOp::Concat => concat(lhs, rhs),
        BinaryOp::Eq => compare_op(lhs, rhs, |o| o == Ordering::Equal),
        BinaryOp::Ne => compare_op(lhs, rhs, |o| o != Ordering::Equal),
        BinaryOp::Lt => compare_op(lhs, rhs, |o| o == Ordering::Less),
        BinaryOp::Le => compare_op(lhs, rhs, |o| o != Ordering::Greater),
        BinaryOp::Gt => compare_op(lhs, rhs, |o| o == Ordering::Greater),
        BinaryOp::Ge => compare_op(lhs, rhs, |o| o != Ordering::Less),
        // Reference operators are not scalar operators — see module docs.
        BinaryOp::Range | BinaryOp::Intersect | BinaryOp::Union => {
            Value::Error(ErrorKind::Unsupported)
        }
    }
}

/// Apply a prefix unary operator (`-`/`+`).
///
/// **Unary `+` is a type-preserving no-op**, **unary `-` coerces to a number
/// and negates** — the asymmetry Excel actually implements, pinned on build
/// 16.0 by OXP-175 (`tools/oracle/manual_probes.py::oxp_175`,
/// `docs/oracle-experiments.md`). Under `+`, a `Text` (numeric *or* not),
/// `Bool`, `Number`, or `Error` operand passes through **unchanged** (`+"abc"`
/// = the text `"abc"`, `+"1"` = the text `"1"` — *not* the number 1, `+TRUE` =
/// `TRUE` — *not* 1, `+#N/A` = `#N/A`); only `Blank` coerces (to `0`). Under
/// `-`, everything coerces (`-"1"` = -1, `-TRUE` = -1, non-numeric text =
/// `#VALUE!`). A **1×1 array** operand under `+` is pinned by OXP-176: it
/// collapses to its element and preserves that element's type (`+{"abc"}` =
/// `"abc"`, `+{5}` = 5, `+{TRUE}` = `TRUE`), mirroring the scalar `+` rule.
/// **Multi-cell** `Array`/`Ref` operands remain unpinned by OXP-175/176 and
/// keep the coercion path for both operators (implicit-intersection reduction
/// is OXP-004/169, out of scope here).
#[must_use]
pub fn apply_unary(op: UnaryOp, operand: &Value) -> Value {
    if let UnaryOp::Plus = op {
        match operand {
            // Type-preserving no-op for the OXP-175-pinned scalar types.
            Value::Text(_) | Value::Bool(_) | Value::Number(_) | Value::Error(_) => {
                return operand.clone();
            }
            // OXP-176 (RUN-2026-07-16-oracle01): unary `+` over a **1×1 array
            // literal** PRESERVES the element's type, mirroring the scalar case
            // (`=+{"abc"}` → the text "abc", `=+{5}` → 5, `=+{TRUE}` → TRUE).
            // Collapse the 1×1 array to its single element and re-apply the
            // scalar `+` rule (recursion handles a Blank element → 0, an Error
            // element → itself, etc.). A **multi-cell** array is unpinned
            // (implicit-intersection reduction is OXP-004/169) and falls through
            // to the coercion path below, unchanged.
            Value::Array(a) => {
                if let Some(inner) = a.as_scalar() {
                    return apply_unary(UnaryOp::Plus, inner);
                }
            }
            // `+Blank` = 0 (OXP-175 H5) via the shared `to_number` below — this
            // module never re-implements a coercion rule. Ref (unpinned;
            // implicit-intersection is OXP-004/169) also falls through.
            Value::Blank | Value::Ref(_) => {}
            // BC-6 (RFC-0012, Part B #3): a lambda must get its OWN refusing
            // arm and must NOT join the Blank/Array/Ref fall-through above —
            // that would route `+lambda` into the shared `to_number` coercion
            // (a compute path). Refuse `+lambda` explicitly here.
            Value::Lambda(_) => return Value::Error(ErrorKind::Unsupported),
        }
    }
    match to_number(operand) {
        Ok(n) => match op {
            UnaryOp::Neg => Value::number(-n),
            UnaryOp::Plus => Value::number(n),
        },
        Err(k) => Value::Error(k),
    }
}

/// Apply a postfix operator. `%` divides the coerced number by 100; the
/// spill-range `#` is out of scope (dynamic arrays) and is `#UNSUPPORTED!`.
#[must_use]
pub fn apply_postfix(op: PostfixOp, operand: &Value) -> Value {
    match op {
        PostfixOp::Percent => match to_number(operand) {
            Ok(n) => Value::number(n / 100.0),
            Err(k) => Value::Error(k),
        },
        PostfixOp::SpillRange => Value::Error(ErrorKind::Unsupported),
    }
}

// ── Consumed-array broadcasting (M2 lane 6) ───────────────────────────────────
//
// The `broadcast_*` combinators map the scalar `apply_*` cores element-wise over
// a `Value::Array` operand, implementing **exactly** the broadcasting rules
// pinned by **OXP-201** (`docs/oracle-experiments.md`, Excel 365 build
// 16.0.20131, RUN 2026-07-14) and no others — every unpinned shape refuses
// loudly (`#UNSUPPORTED!`), the born-refusing default (Recalc Principle 2).
// Spec: `docs/plans/2026-07-14-consumed-array-eval-spec.md` §3.
//
// These are called **only** from `xl-engine`'s operator arms under
// `array_arg_ctx` (the consumed-array evaluation context, RFC-0011): a
// `Value::Array` reaching an operator there can only be a genuine consumed array
// materialized by the array-context gate. The scalar `apply_binary`/`apply_unary`
// /`apply_postfix` cores above stay scalar-only and are reused per element — this
// module never re-implements a scalar rule.
//
// Pinned rules (OXP-201): (a) scalar↔array (#1/#8); (b) same-shape elementwise
// (#2/#7); (c) pure row×column outer product (#3); (d) column-vector
// length-mismatch → `#N/A`-pad-and-propagate (#4); (e-collapse) a 1×1 array
// collapses to its element (#5). Comparisons (f, #9) fall out of `apply_binary`
// producing a `Bool` per element.

/// A binary/unary operand, viewed as either a scalar or a genuine multi-cell
/// array. A **1×1** array collapses to its single element (OXP-201 rule
/// e-collapse, #5), so every [`Operand::Arr`] is genuinely multi-cell.
enum Operand<'a> {
    Scalar(&'a Value),
    Arr(&'a Array),
}

fn view(v: &Value) -> Operand<'_> {
    match v {
        Value::Array(a) => match a.as_scalar() {
            Some(s) => Operand::Scalar(s),
            None => Operand::Arr(a),
        },
        other => Operand::Scalar(other),
    }
}

/// Build a `Value::Array` from row-major `data`, or refuse defensively if the
/// dimensions somehow do not form a rectangle (unreachable on the happy path —
/// every producer here supplies `rows * cols` elements — but we never panic).
fn build_array(rows: usize, cols: usize, data: Vec<Value>) -> Value {
    match Array::new(rows, cols, data) {
        Ok(arr) => Value::Array(arr),
        Err(_) => Value::Error(ErrorKind::Unsupported),
    }
}

/// Map `f` over every element of `a`, preserving its shape.
fn map_array(a: &Array, f: impl Fn(&Value) -> Value) -> Value {
    let data: Vec<Value> = a.iter().map(f).collect();
    build_array(a.rows(), a.cols(), data)
}

/// Element-wise broadcast of the scalar [`apply_binary`] over array operands,
/// implementing exactly the OXP-201-pinned rules (a)–(d),(f). Any shape not on
/// that pinned list refuses (`#UNSUPPORTED!`). See the module section above.
#[must_use]
pub fn broadcast_binary(op: BinaryOp, lhs: &Value, rhs: &Value) -> Value {
    match (view(lhs), view(rhs)) {
        // Both collapsed to scalars (each was a 1×1 array): the plain operator.
        (Operand::Scalar(a), Operand::Scalar(b)) => apply_binary(op, a, b),
        // (a) scalar ↔ array. OXP-201 §4.8: a *scalar error* against an array is
        // unprobed → refuse (do not silently broadcast the error).
        (Operand::Scalar(s), Operand::Arr(a)) => {
            if s.is_error() {
                return Value::Error(ErrorKind::Unsupported);
            }
            map_array(a, |el| apply_binary(op, s, el))
        }
        (Operand::Arr(a), Operand::Scalar(s)) => {
            if s.is_error() {
                return Value::Error(ErrorKind::Unsupported);
            }
            map_array(a, |el| apply_binary(op, el, s))
        }
        (Operand::Arr(l), Operand::Arr(r)) => broadcast_arrays(op, l, r),
    }
}

/// The array ⊗ array cases of [`broadcast_binary`]. Only the OXP-201-pinned
/// shape combinations compute; every other shape refuses (`#UNSUPPORTED!`).
fn broadcast_arrays(op: BinaryOp, l: &Array, r: &Array) -> Value {
    let (lr, lc) = (l.rows(), l.cols());
    let (rr, rc) = (r.rows(), r.cols());

    // (b) identical shape → elementwise. OXP-201 #2 (1-D), #7 (2×2).
    if lr == rr && lc == rc {
        let data: Vec<Value> = l
            .iter()
            .zip(r.iter())
            .map(|(a, b)| apply_binary(op, a, b))
            .collect();
        return build_array(lr, lc, data);
    }

    // A 1×1 array is already collapsed by `view`, so an `Arr` has rows>1 or
    // cols>1: `rows == 1` ⇒ a row vector, `cols == 1` ⇒ a column vector.
    let (l_row, l_col) = (lr == 1, lc == 1);
    let (r_row, r_col) = (rr == 1, rc == 1);

    // (c) pure row-vector ⊗ column-vector outer product. OXP-201 #3 pinned the
    // row-left × col-right shape with `*`; the reversed order (col-left ×
    // row-right) and non-commutative operators rest on the COMPOSITIONAL reading
    // — the pinned outer-product SHAPE rule applied over the frozen scalar cores,
    // the same way rules (a)/(b)/(d) generalize across operators — and the
    // element multiset is order-invariant under the downstream SUM reduction, so
    // exposure is ~nil. The singleton axis of each operand broadcasts against the
    // other's extent; operand order (`lhs op rhs`) is preserved per position.
    if (l_row && r_col) || (l_col && r_row) {
        let rows = lr.max(rr);
        let cols = lc.max(rc);
        let mut data = Vec::with_capacity(rows.saturating_mul(cols));
        for i in 0..rows {
            for j in 0..cols {
                let a = l.get(if lr == 1 { 0 } else { i }, if lc == 1 { 0 } else { j });
                let b = r.get(if rr == 1 { 0 } else { i }, if rc == 1 { 0 } else { j });
                match (a, b) {
                    (Some(a), Some(b)) => data.push(apply_binary(op, a, b)),
                    _ => return Value::Error(ErrorKind::Unsupported),
                }
            }
        }
        return build_array(rows, cols, data);
    }

    // (d) column-vector length mismatch (N×1 vs M×1): pad the shorter with
    // `#N/A`, which propagates through the downstream reduction. OXP-201 #4.
    // §4.7: if either operand already holds an error element, the `#N/A`-vs-real
    // precedence is unprobed → refuse rather than guess ordering.
    if l_col && r_col {
        if l.iter().any(Value::is_error) || r.iter().any(Value::is_error) {
            return Value::Error(ErrorKind::Unsupported);
        }
        let rows = lr.max(rr);
        let mut data = Vec::with_capacity(rows);
        for i in 0..rows {
            match (l.get(i, 0), r.get(i, 0)) {
                (Some(a), Some(b)) => data.push(apply_binary(op, a, b)),
                // Past the shorter vector's end → Excel pads with `#N/A`.
                _ => data.push(Value::Error(ErrorKind::Na)),
            }
        }
        return build_array(rows, 1, data);
    }

    // Every other combination — row-width mismatch (§4.4), vector-vs-matrix
    // partial broadcast (§4.3), unequal matrices — is unpinned → refuse.
    Value::Error(ErrorKind::Unsupported)
}

/// Element-wise broadcast of the scalar [`apply_unary`] over an array operand
/// (a 1×1 array collapses to its element first). See the module section above.
#[must_use]
pub fn broadcast_unary(op: UnaryOp, v: &Value) -> Value {
    match view(v) {
        Operand::Scalar(s) => apply_unary(op, s),
        Operand::Arr(a) => map_array(a, |el| apply_unary(op, el)),
    }
}

/// Element-wise broadcast of the scalar [`apply_postfix`] over an array operand
/// (a 1×1 array collapses to its element first). See the module section above.
#[must_use]
pub fn broadcast_postfix(op: PostfixOp, v: &Value) -> Value {
    match view(v) {
        Operand::Scalar(s) => apply_postfix(op, s),
        Operand::Arr(a) => map_array(a, |el| apply_postfix(op, el)),
    }
}

/// Shared arithmetic core: coerce both operands (leftmost error wins), apply
/// `f`, and route the result through the finiteness invariant.
fn arith(lhs: &Value, rhs: &Value, f: impl Fn(f64, f64) -> f64) -> Value {
    let a = match to_number(lhs) {
        Ok(a) => a,
        Err(k) => return Value::Error(k),
    };
    let b = match to_number(rhs) {
        Ok(b) => b,
        Err(k) => return Value::Error(k),
    };
    Value::number(f(a, b))
}

/// Division, with the divide-by-zero guard emitting `#DIV/0!` before any `Inf`
/// can form.
fn divide(lhs: &Value, rhs: &Value) -> Value {
    let a = match to_number(lhs) {
        Ok(a) => a,
        Err(k) => return Value::Error(k),
    };
    let b = match to_number(rhs) {
        Ok(b) => b,
        Err(k) => return Value::Error(k),
    };
    if b == 0.0 {
        return Value::Error(ErrorKind::Div0);
    }
    Value::number(a / b)
}

/// Exponentiation. See the module docs' `OXP-081` note (RUN-2026-07-11-oracle01)
/// for `0^0`, `0^negative`, and negative-base / non-integer-exponent handling.
fn power(lhs: &Value, rhs: &Value) -> Value {
    let base = match to_number(lhs) {
        Ok(a) => a,
        Err(k) => return Value::Error(k),
    };
    let exp = match to_number(rhs) {
        Ok(b) => b,
        Err(k) => return Value::Error(k),
    };
    // OXP-081: a zero base is special — `0^0` = `#NUM!`, `0^negative` =
    // `#DIV/0!` (both observed on the Excel farm; neither is `powf`'s value).
    if base == 0.0 {
        if exp == 0.0 {
            return Value::Error(ErrorKind::Num);
        }
        if exp < 0.0 {
            return Value::Error(ErrorKind::Div0);
        }
        // `0^positive` = 0 falls through to `powf`.
    }
    // OXP-081: a negative base with a non-integer exponent yields a real result
    // only for an odd root — when the exponent is the reciprocal of an odd
    // integer (`1/3`, `1/5`, ...). Excel then returns `-(|base|^exp)`; every
    // other non-integer exponent (even roots, irrational, complex) is `#NUM!`.
    if base < 0.0 && exp.fract() != 0.0 {
        let recip = 1.0 / exp;
        let is_odd_root = recip.is_finite() && recip.fract() == 0.0 && (recip as i64) % 2 != 0;
        if is_odd_root {
            return Value::number(-((-base).powf(exp)));
        }
        return Value::Error(ErrorKind::Num);
    }
    // Any remaining non-finite outcome becomes `#NUM!` via the value invariant.
    Value::number(base.powf(exp))
}

/// Text concatenation (`&`): coerce both sides to text (leftmost error wins).
fn concat(lhs: &Value, rhs: &Value) -> Value {
    let a = match to_text(lhs) {
        Ok(a) => a,
        Err(k) => return Value::Error(k),
    };
    let b = match to_text(rhs) {
        Ok(b) => b,
        Err(k) => return Value::Error(k),
    };
    let mut s = String::with_capacity(a.as_str().len() + b.as_str().len());
    s.push_str(a.as_str());
    s.push_str(b.as_str());
    Value::text(&s)
}

/// Comparison core for the six operators: `xl-value`'s [`compare_fuzzy`] mapped
/// to a boolean by `pick`. It shares all of [`compare`]'s type behavior
/// (case-insensitive text, cross-type ordering, `Blank`-morphing incl.
/// `Blank ↔ Bool` per OXP-030, error propagation) but compares two `Number`s
/// under Excel's **~15-significant-figure fuzz** (RFC 0009, pinned by
/// OXP-179/180/181/182): `0.1+0.2 = 0.3` is TRUE, and a fuzz-equal pair is
/// neither `<` nor `>`. Only the operators are switched — lookups/criteria/
/// min-max stay on the exact [`compare`] until each is separately probed
/// (RFC 0009 §4). There is no `Blank`-vs-`Bool` `#UNSUPPORTED!` path.
fn compare_op(lhs: &Value, rhs: &Value, pick: impl Fn(Ordering) -> bool) -> Value {
    match compare_fuzzy(lhs, rhs) {
        Ok(ord) => Value::Bool(pick(ord)),
        Err(k) => Value::Error(k),
    }
}

/// Consumed-array broadcasting combinators (M2 lane 6, OXP-201). Each test names
/// the pinned rule it locks and, for refusals, the born-refusing boundary
/// (spec §4) it enforces.
#[cfg(test)]
mod broadcast_tests {
    use super::*;

    fn col(vals: &[f64]) -> Value {
        Value::Array(
            Array::new(
                vals.len(),
                1,
                vals.iter().map(|&x| Value::number(x)).collect(),
            )
            .unwrap(),
        )
    }
    fn row(vals: &[f64]) -> Value {
        Value::Array(
            Array::new(
                1,
                vals.len(),
                vals.iter().map(|&x| Value::number(x)).collect(),
            )
            .unwrap(),
        )
    }
    fn arr(v: &Value) -> &Array {
        match v {
            Value::Array(a) => a,
            other => panic!("expected array, got {other:?}"),
        }
    }
    fn n(x: f64) -> Value {
        Value::number(x)
    }

    // (a) scalar ↔ array, both operand orders. OXP-201 #1.
    #[test]
    fn scalar_array_broadcast_both_orders() {
        let r = broadcast_binary(BinaryOp::Mul, &col(&[1.0, 2.0, 3.0]), &n(2.0));
        let a = arr(&r);
        assert_eq!((a.rows(), a.cols()), (3, 1));
        assert_eq!(
            a.iter().cloned().collect::<Vec<_>>(),
            vec![n(2.0), n(4.0), n(6.0)]
        );
        // scalar on the left preserves operand order (matters for `-`/`/`).
        let s = broadcast_binary(BinaryOp::Sub, &n(10.0), &col(&[1.0, 2.0]));
        assert_eq!(
            arr(&s).iter().cloned().collect::<Vec<_>>(),
            vec![n(9.0), n(8.0)]
        );
    }

    // (b) same-shape elementwise, 1-D and 2-D. OXP-201 #2 / #7.
    #[test]
    fn same_shape_elementwise() {
        let r = broadcast_binary(
            BinaryOp::Add,
            &col(&[1.0, 2.0, 3.0]),
            &col(&[1.0, 2.0, 3.0]),
        );
        assert_eq!(
            arr(&r).iter().cloned().collect::<Vec<_>>(),
            vec![n(2.0), n(4.0), n(6.0)]
        );
        // 2×2 square: [[1,2],[3,4]] * itself = [[1,4],[9,16]].
        let sq = Value::Array(Array::new(2, 2, vec![n(1.0), n(2.0), n(3.0), n(4.0)]).unwrap());
        let m = broadcast_binary(BinaryOp::Mul, &sq, &sq);
        assert_eq!(
            arr(&m).iter().cloned().collect::<Vec<_>>(),
            vec![n(1.0), n(4.0), n(9.0), n(16.0)]
        );
    }

    // (c) pure row × column outer product → 3×3. OXP-201 #3: Σ = (Σrow)(Σcol).
    #[test]
    fn row_col_outer_product() {
        let r = broadcast_binary(
            BinaryOp::Mul,
            &row(&[1.0, 2.0, 3.0]),
            &col(&[1.0, 2.0, 3.0]),
        );
        let a = arr(&r);
        assert_eq!((a.rows(), a.cols()), (3, 3));
        // result[i][j] = row[j] * col[i]; e.g. (i=2,j=2) = 3*3 = 9.
        assert_eq!(a.get(2, 2), Some(&n(9.0)));
        let sum: f64 = a
            .iter()
            .map(|v| if let Value::Number(x) = v { *x } else { 0.0 })
            .sum();
        assert_eq!(sum, 36.0);
    }

    // (d) column-vector length mismatch pads the shorter with #N/A. OXP-201 #4.
    #[test]
    fn column_length_mismatch_pads_na() {
        let r = broadcast_binary(BinaryOp::Add, &col(&[1.0, 2.0, 3.0]), &col(&[1.0, 2.0]));
        assert_eq!(
            arr(&r).iter().cloned().collect::<Vec<_>>(),
            vec![n(2.0), n(4.0), Value::Error(ErrorKind::Na)]
        );
    }

    // (f) comparison broadcasts element-wise to a Bool array. OXP-201 #9.
    #[test]
    fn comparison_yields_bool_array() {
        let r = broadcast_binary(BinaryOp::Gt, &col(&[1.0, 2.0, 3.0]), &n(2.0));
        assert_eq!(
            arr(&r).iter().cloned().collect::<Vec<_>>(),
            vec![Value::Bool(false), Value::Bool(false), Value::Bool(true)]
        );
    }

    // (e-collapse) a 1×1 array collapses to its element before broadcasting.
    #[test]
    fn one_by_one_array_collapses() {
        let one = Value::Array(Array::new(1, 1, vec![n(10.0)]).unwrap());
        let r = broadcast_binary(BinaryOp::Add, &col(&[1.0, 2.0]), &one);
        assert_eq!(
            arr(&r).iter().cloned().collect::<Vec<_>>(),
            vec![n(11.0), n(12.0)]
        );
    }

    // Unary and postfix map element-wise.
    #[test]
    fn unary_and_postfix_map() {
        let neg = broadcast_unary(UnaryOp::Neg, &col(&[1.0, 2.0, 3.0]));
        assert_eq!(
            arr(&neg).iter().cloned().collect::<Vec<_>>(),
            vec![n(-1.0), n(-2.0), n(-3.0)]
        );
        let pct = broadcast_postfix(PostfixOp::Percent, &col(&[100.0, 200.0]));
        assert_eq!(
            arr(&pct).iter().cloned().collect::<Vec<_>>(),
            vec![n(1.0), n(2.0)]
        );
    }

    // ── Born-refusing boundary (spec §4): unpinned shapes stay #UNSUPPORTED! ──

    #[test]
    fn row_width_mismatch_refuses() {
        // §4.4: only the column-vector mismatch is pinned; row-width is not.
        let r = broadcast_binary(BinaryOp::Add, &row(&[1.0, 2.0, 3.0]), &row(&[1.0, 2.0]));
        assert_eq!(r, Value::Error(ErrorKind::Unsupported));
    }

    #[test]
    fn vector_vs_matrix_refuses() {
        // §4.3: partial broadcast of a vector against a genuine matrix.
        let mat = Value::Array(Array::new(2, 2, vec![n(1.0), n(2.0), n(3.0), n(4.0)]).unwrap());
        let r = broadcast_binary(BinaryOp::Add, &mat, &col(&[1.0, 2.0]));
        assert_eq!(r, Value::Error(ErrorKind::Unsupported));
    }

    #[test]
    fn scalar_error_operand_refuses() {
        // §4.8: a scalar-error ⊗ array is unprobed → refuse (do not broadcast it).
        let r = broadcast_binary(
            BinaryOp::Mul,
            &Value::Error(ErrorKind::Div0),
            &col(&[1.0, 2.0, 3.0]),
        );
        assert_eq!(r, Value::Error(ErrorKind::Unsupported));
    }

    #[test]
    fn mismatch_pad_with_preexisting_error_refuses() {
        // §4.7: mixing #N/A padding with a real error element has unpinned
        // precedence → refuse rather than guess ordering.
        let with_err = Value::Array(
            Array::new(3, 1, vec![n(1.0), Value::Error(ErrorKind::Div0), n(3.0)]).unwrap(),
        );
        let r = broadcast_binary(BinaryOp::Add, &with_err, &col(&[1.0, 2.0]));
        assert_eq!(r, Value::Error(ErrorKind::Unsupported));
    }
}
