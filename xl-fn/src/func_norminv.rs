//! `NORMINV` — inverse of the **general** normal cumulative distribution:
//! given a probability `p`, a mean `μ`, and a standard deviation `σ`, return
//! the value `x` such that `Φ((x − μ)/σ) = p`. Equivalently,
//! `NORMINV(p, μ, σ) = μ + σ · NORMSINV(p)`.
//!
//! # Provenance
//! Behavior contract: `docs/specs/NORMINV.md`. Farm-pinned expected values:
//! **RUN-2026-07-11-oracle01**, experiment **OXP-154**
//! (`tools/oracle/out/results/OXP-154.*.sidecar.json`). Every pinned target is
//! reproduced **bit-for-bit** (see the unit tests).
//!
//! # Numerical method
//! The standard-normal quantile is computed by [`func_normsinv::probit`] —
//! Wichura's Algorithm AS 241 — and this module applies the location/scale
//! transform `μ + σ · z`. The probit kernel is **not** duplicated here; it has
//! a single source of truth in [`func_normsinv`]. See that module for the
//! algorithm, its published provenance (clean-room, no GPL source), and the
//! central-branch grouping that reproduces Excel's bits.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Coerce all three arguments via scalar numeric coercion (bool → 1/0,
//!   numeric text → number, blank → 0) (NORMINV.md §Coercion). A non-coercible
//!   text argument yields `#VALUE!`; an error-valued argument propagates as-is
//!   (NORMINV.md §Error behavior).
//! - Domain: `σ ≤ 0` → `#NUM!` (a distribution needs a positive spread; the
//!   observed `σ = 0` → `#NUM!` and `σ = −15` → `#NUM!`), and `p ≤ 0` or
//!   `p ≥ 1` → `#NUM!` (inherited from the probit's open-interval domain; the
//!   observed `p = 0` → `#NUM!`, `p = 1` → `#NUM!`) (NORMINV.md §1).
//! - Otherwise return `μ + σ · probit(p)` (NORMINV.md §1). With `σ = 1, μ = 0`
//!   this is exactly `NORMSINV(p)`; with `p = 0.5` it is `μ` (probit(0.5) = 0).

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::func_normsinv::probit;

/// Evaluate a `NORMINV(probability, mean, standard_dev)` call. See the module
/// docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Coerce all three arguments left-to-right; a coercion error (e.g. #VALUE!
    // from non-numeric text) or an error-valued argument propagates.
    let p = match to_number(&args.eval_scalar(0)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    let mean = match to_number(&args.eval_scalar(1)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    let sd = match to_number(&args.eval_scalar(2)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };

    // A non-positive standard deviation has no distribution → #NUM! (observed
    // σ = 0 and σ = −15 → #NUM!). Both this and the p-domain guard below yield
    // #NUM!, so their relative order is unobservable from the oracle.
    if sd <= 0.0 {
        return Value::Error(ErrorKind::Num);
    }

    match probit(p) {
        // Location/scale transform: μ + σ·z. Value::number maps any non-finite
        // result (μ/σ overflow) to #NUM! per the crate-wide invariant.
        Some(z) => Value::number(mean + sd * z),
        // p ≤ 0 or p ≥ 1: outside the CDF's (0,1) range → #NUM!.
        None => Value::Error(ErrorKind::Num),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    // All expected values below are farm-pinned:
    // RUN-2026-07-11-oracle01, OXP-154. Reproduced bit-for-bit (AS 241 probit
    // plus the μ + σ·z transform), so the assertions are exact — no tolerance.

    #[test]
    fn median_returns_mean() {
        // NORMINV(0.5, μ, σ) = μ exactly (probit(0.5) = 0).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.5)), Scalar(num(100.0)), Scalar(num(15.0))]
            ),
            num(100.0)
        );
    }

    #[test]
    fn upper_and_lower_quartile_bits() {
        // 100 + 15·probit(0.975) and 100 + 15·probit(0.025).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.975)), Scalar(num(100.0)), Scalar(num(15.0))]
            ),
            num(129.3994597681008)
        );
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.025)), Scalar(num(100.0)), Scalar(num(15.0))]
            ),
            num(70.60054023189919)
        );
    }

    #[test]
    fn standard_normal_matches_normsinv() {
        // μ = 0, σ = 1 reduces to NORMSINV(0.9) — the central-branch case.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.9)), Scalar(num(0.0)), Scalar(num(1.0))]
            ),
            num(1.2815515655446006)
        );
    }

    #[test]
    fn zero_standard_deviation_is_num_error() {
        // σ = 0 → #NUM! (observed).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.5)), Scalar(num(100.0)), Scalar(num(0.0))]
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn negative_standard_deviation_is_num_error() {
        // σ = −15 → #NUM! (observed).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.5)), Scalar(num(100.0)), Scalar(num(-15.0))]
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn probability_zero_is_num_error() {
        // p = 0 → #NUM! (observed).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(0.0)), Scalar(num(100.0)), Scalar(num(15.0))]
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn probability_one_is_num_error() {
        // p = 1 → #NUM! (observed).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(1.0)), Scalar(num(100.0)), Scalar(num(15.0))]
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn arguments_coerce_from_text() {
        // Numeric text coerces on every argument (to_number).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("0.5")), Scalar(txt("100")), Scalar(txt("15"))]
            ),
            num(100.0)
        );
    }

    #[test]
    fn non_numeric_text_is_value_error() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("abc")), Scalar(num(0.0)), Scalar(num(1.0))]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn error_argument_propagates() {
        // An error-valued argument propagates (first one, left-to-right).
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(0.5)),
                    Scalar(Value::Error(ErrorKind::Na)),
                    Scalar(num(1.0))
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }
}
