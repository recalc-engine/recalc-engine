//! `IF` — lazily select one of two branch expressions on a boolean test.
//!
//! # Provenance
//! Behavior contract: `docs/specs/IF.md` (which cites the Microsoft Learn IF
//! function page). Boolean coercion of the test is `xl-value`'s [`to_bool`].
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - The `logical_test` (argument 0) is evaluated in scalar context and coerced
//!   with [`to_bool`]: nonzero → `TRUE`, 0 → `FALSE`, `"TRUE"`/`"FALSE"`
//!   (case-insensitive) → boolean, blank → `FALSE`, other text → `#VALUE!`
//!   (IF.md §Coercion). An error in the test propagates immediately (IF.md
//!   §Error behavior).
//! - **Lazy evaluation** (IF.md §2): only the *selected* branch is forced. The
//!   unselected branch is never evaluated, so an error, a `#UNSUPPORTED!`
//!   construct, a division by zero, or a volatile read inside it cannot affect
//!   the result. Its value passes through untouched (no branch-type unification;
//!   IF.md §Coercion).
//!
//! # Elided / absent selected branch — RESOLVED (`OXP-080`, RUN-2026-07-16-oracle01)
//! IF.md's §"Oracle experiments needed" deferred the precise value/type of
//! every elision combination to a grid probe. `OXP-080` (Excel 16.0) pins two
//! **distinct** results, keyed on whether the selected branch is
//! *present-but-elided* or *absent altogether*:
//! - **Selected branch present but elided** (an empty `,` slot) → the number
//!   **`0`**: `IF(TRUE,)` = 0 (H1), `IF(TRUE,,9)` = 0 (H2), `IF(FALSE,1,)` = 0
//!   (H4), `IF(1,,)` = 0 (H5).
//! - **Selected branch absent** (no such argument at all) → **`FALSE`**:
//!   `IF(FALSE,1)` selects the missing else → `FALSE` (H3, a `bool`).
//!
//! The one-argument form `IF(1)` is rejected by Excel *at entry* (an unloadable
//! formula), so it has no runtime value and is deliberately out of scope
//! (`docs/oracle-experiments.md`, OXP-080 note). The common both-branches-
//! present forms are unchanged.

use xl_value::{Array, ErrorKind, Value, to_bool};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Evaluate an `IF(...)` call. See the module docs for the semantics and their
/// spec provenance.
///
/// # Consumed-array context (M2 lane 6, OXP-201 #10)
/// Under an array-context aggregator argument ([`CallArgs::array_arg_ctx`]) a
/// **multi-cell condition array** takes the element-wise *array-select* path:
/// **both** branches are evaluated (laziness is necessarily dropped for the
/// array case), broadcast to the condition's shape, and selected per element.
/// This is the residual `SUM(IF(range,…))` idiom Excel auto-array-evaluates. A
/// **scalar** condition keeps the existing lazy scalar path unchanged. The
/// omitted/absent false-branch stays `#UNSUPPORTED!` in array context: OXP-080
/// (RUN-2026-07-16-oracle01) pinned only the SCALAR elision forms (resolved on
/// the scalar path below); the element-wise array-context elision is a
/// different, unprobed shape (deferred FUSE variant #3). Spec:
/// `docs/plans/2026-07-14-consumed-array-eval-spec.md` §2c.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Argument 0 (logical_test) is guaranteed present by the registry arity
    // check (min 2 args). Evaluate eagerly in scalar context.
    let cond_val = args.eval_scalar(0);

    if args.array_arg_ctx()
        && let Value::Array(c) = &cond_val
        && c.as_scalar().is_none()
    {
        return array_select(args, c);
    }

    let cond = match to_bool(&cond_val) {
        Ok(b) => b,
        Err(k) => return Value::Error(k),
    };

    let branch = if cond { 1 } else { 2 };

    // Selected branch ABSENT (e.g. `IF(FALSE,1)` selecting the missing else) →
    // `FALSE` (OXP-080 H3, RUN-2026-07-16-oracle01).
    if branch >= args.count() {
        return Value::Bool(false);
    }
    // Selected branch present but ELIDED (an empty `,` slot: `IF(TRUE,)`,
    // `IF(TRUE,,9)`, `IF(FALSE,1,)`, `IF(1,,)`) → the number `0` (OXP-080
    // H1/H2/H4/H5).
    if args.shape(branch) == ArgShape::Omitted {
        return Value::Number(0.0);
    }

    // Force *only* the selected branch — laziness guaranteed.
    args.eval_scalar(branch)
}

/// Element-wise `IF` over a consumed condition array (OXP-201 #10). Both
/// branches are evaluated and selected per element. The absent/omitted
/// false-branch stays `#UNSUPPORTED!` here (§4.10): OXP-080 pinned only the
/// SCALAR elision forms (`0` / `FALSE`); array context does not unlock an
/// element-wise elision reading — that shape is unprobed.
fn array_select(args: &mut dyn CallArgs, cond: &Array) -> Value {
    // Both branches must be present: an omitted/absent false-branch in array
    // context is unpinned (OXP-080) → refuse, exactly as the scalar path does.
    if args.count() < 3 || args.shape(1) == ArgShape::Omitted || args.shape(2) == ArgShape::Omitted
    {
        return Value::Error(ErrorKind::Unsupported);
    }
    let then_v = args.eval_scalar(1);
    let else_v = args.eval_scalar(2);

    let (rows, cols) = (cond.rows(), cond.cols());
    let mut data = Vec::with_capacity(rows.saturating_mul(cols));
    for i in 0..rows {
        for j in 0..cols {
            let Some(cv) = cond.get(i, j) else {
                return Value::Error(ErrorKind::Unsupported);
            };
            let el = match to_bool(cv) {
                Ok(true) => branch_elem(&then_v, i, j, rows, cols),
                Ok(false) => branch_elem(&else_v, i, j, rows, cols),
                // A condition error (incl. a recalc sentinel) propagates per
                // element, matching scalar IF's test-error propagation.
                Err(k) => Some(Value::Error(k)),
            };
            match el {
                Some(v) => data.push(v),
                // Branch shape not pinned against the condition → refuse.
                None => return Value::Error(ErrorKind::Unsupported),
            }
        }
    }
    match Array::new(rows, cols, data) {
        Ok(arr) => Value::Array(arr),
        Err(_) => Value::Error(ErrorKind::Unsupported),
    }
}

/// The element a branch contributes at condition position `(i, j)`: a scalar (or
/// a 1×1 array) broadcasts to every position (OXP-201 rule a); a same-shape
/// array selects positionally (rule b). Any other branch shape is unpinned →
/// `None` (refuse).
///
/// Deliberately MORE CONSERVATIVE than a general array-IF: a branch/condition
/// length mismatch refuses (`#UNSUPPORTED!`) rather than applying rule-(d)
/// `#N/A` padding. `#N/A` padding is farm-pinned (OXP-201 #4) only for a *binary
/// operator* over two column vectors — NOT for IF's positional branch selection
/// — so IF stays loud here until that shape is itself pinned (Principle 2:
/// refuse over guess).
fn branch_elem(branch: &Value, i: usize, j: usize, rows: usize, cols: usize) -> Option<Value> {
    match branch {
        Value::Array(a) => match a.as_scalar() {
            Some(s) => Some(s.clone()),
            None if a.rows() == rows && a.cols() == cols => {
                Some(a.get(i, j).cloned().unwrap_or(Value::Blank))
            }
            None => None,
        },
        other => Some(other.clone()),
    }
}
