//! `LOG` — the logarithm of a number to a given base (base 10 by default).
//!
//! # Provenance
//! Microsoft Learn LOG function page
//! (`https://support.microsoft.com/en-us/office/log-function-4e82f196-1ca9-4747-8fb0-6c4a3abb3280`).
//! Coercion via `xl-value`'s [`to_number`]. The `number` domain ("Number … the
//! positive real number for which you want the logarithm") is documented
//! directly on the Microsoft page — mirroring the sibling `LN`'s documented
//! non-positive `#NUM!` — so the `number ≤ 0` → `#NUM!` check is not a guess.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Coerce `number` (arg 0) and, when supplied, `base` (arg 1) via scalar
//!   numeric coercion (bool → 1/0, numeric text → number, blank → 0). An
//!   error-valued argument propagates (LOG.md §Error behavior).
//! - `base` (arg 1) is **optional and defaults to 10** (`LOG(number)` =
//!   base-10 log) (LOG.md §Semantics). `args.count()` distinguishes an omitted
//!   base from an explicit one.
//! - `number ≤ 0` → `#NUM!` (the logarithm is real only for a positive number;
//!   documented, as in `LN`).
//! - Otherwise return `log_base(number)`: the exact `f64::log10`/`log2` routine
//!   for the exactly-representable common bases 10 and 2 (so `LOG(100)` = `2`,
//!   `LOG(8, 2)` = `3` land cleanly), else the general `number.log(base)` =
//!   `ln(number)/ln(base)`. This is a numerical-accuracy choice for the
//!   *unambiguous* base-b logarithm, not a semantic decision.
//!
//! # Degenerate base edges — OXP-224 (RESOLVED, RUN-2026-07-20-oracle01)
//! The Microsoft LOG page documents **no** error behavior for a degenerate
//! `base`; the two degenerate cases are a division-by-zero-shaped singularity in
//! the change-of-base `ln(number)/ln(base)`. The pinned Excel 16.0 build was
//! probed (`=LOG(10,1)`, `=LOG(10,0)`, `=LOG(10,-2)`) and decides:
//! - `base == 1` — `ln(base) == 0`, a genuine divide-by-zero → **`#DIV/0!`**.
//! - `base <= 0` — no real logarithm of a non-positive base → **`#NUM!`**
//!   (matching the `number <= 0` domain error; `base == 0` and `base < 0` both
//!   give `#NUM!`, not `#DIV/0!`).
//!
//! Every non-degenerate base (`base > 0`, `base != 1`), including fractional
//! bases like `0.5`, is fully served. Provenance: OXP-224.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// Evaluate a `LOG(number, [base])` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let number = match to_number(&args.eval_scalar(0)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    // base is optional and defaults to 10 (the base-10 log).
    let base = if args.count() > 1 {
        match to_number(&args.eval_scalar(1)) {
            Ok(n) => n,
            Err(k) => return Value::Error(k),
        }
    } else {
        10.0
    };

    // Documented domain: number must be positive (as for LN).
    if number <= 0.0 {
        return Value::Error(ErrorKind::Num);
    }
    // OXP-224 (RUN-2026-07-20-oracle01): base == 1 is a genuine divide-by-zero
    // (`ln(base) == 0`) → #DIV/0!; a non-positive base has no real log → #NUM!.
    if base == 1.0 {
        return Value::Error(ErrorKind::Div0);
    }
    if base <= 0.0 {
        return Value::Error(ErrorKind::Num);
    }

    // Exact libm routine for the common exactly-representable bases; general
    // change-of-base otherwise. The base-b log itself is unambiguous.
    let result = if base == 10.0 {
        number.log10()
    } else if base == 2.0 {
        number.log2()
    } else {
        number.log(base)
    };
    Value::number(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    #[test]
    fn default_base_ten() {
        // LOG(number) with base omitted = base-10 log (Microsoft example).
        assert_eq!(eval_direct(eval, vec![Scalar(num(100.0))]), num(2.0));
        assert_eq!(eval_direct(eval, vec![Scalar(num(1000.0))]), num(3.0));
        assert_eq!(eval_direct(eval, vec![Scalar(num(1.0))]), num(0.0));
    }

    #[test]
    fn explicit_base() {
        // LOG(8, 2) = 3 (Microsoft example); LOG(x, 10) matches the default.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(8.0)), Scalar(num(2.0))]),
            num(3.0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(100.0)), Scalar(num(10.0))]),
            num(2.0)
        );
    }

    #[test]
    fn fractional_base_is_served() {
        // base 0.5 is > 0 and != 1, fully served: log_0.5(8) = -3.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(8.0)), Scalar(num(0.5))]),
            num(8.0_f64.log(0.5))
        );
    }

    #[test]
    fn nonpositive_number_is_num_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0))]),
            Value::Error(ErrorKind::Num)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-5.0))]),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn degenerate_base_error_kinds_oxp224() {
        // OXP-224 (RUN-2026-07-20-oracle01): base == 1 → #DIV/0! (ln(base)==0);
        // base == 0 and base < 0 → #NUM! (no real log of a non-positive base).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(10.0)), Scalar(num(1.0))]),
            Value::Error(ErrorKind::Div0)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(10.0)), Scalar(num(0.0))]),
            Value::Error(ErrorKind::Num)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(10.0)), Scalar(num(-2.0))]),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn coercion_and_error_propagation() {
        // Numeric text / bool coerce through to_number.
        assert_eq!(eval_direct(eval, vec![Scalar(txt("100"))]), num(2.0));
        // Non-numeric text -> #VALUE!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("x"))]),
            Value::Error(ErrorKind::Value)
        );
        // Error in either argument propagates.
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Div0))]),
            Value::Error(ErrorKind::Div0)
        );
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(8.0)), Scalar(Value::Error(ErrorKind::Ref))]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }
}
