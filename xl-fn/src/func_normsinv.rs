//! `NORMSINV` — inverse of the **standard** normal cumulative distribution
//! (the probit function): given a probability `p`, return the value `z` such
//! that `Φ(z) = p`, where `Φ` is the standard-normal CDF (mean 0, variance 1).
//!
//! # Provenance
//! Behavior contract: `docs/specs/NORMSINV.md`. Farm-pinned expected values:
//! **RUN-2026-07-11-oracle01**, experiment **OXP-153**
//! (`tools/oracle/out/results/OXP-153.*.sidecar.json`). Every pinned target is
//! reproduced **bit-for-bit** (see the unit tests).
//!
//! # Numerical method (NORMSINV.md §Numerical method)
//! The quantile is computed with **Wichura's Algorithm AS 241** (`PPND16`), the
//! standard high-accuracy double-precision probit — M. J. Wichura, "Algorithm
//! AS 241: The Percentage Points of the Normal Distribution", *Applied
//! Statistics* 37(3), 1988, pp. 477–484. This is a clean-room reconstruction
//! from the **published** rational-approximation coefficients; no GPL source
//! was consulted (a Recalc design rule). The relative accuracy of the approximation is
//! ≈1e-15 (≤4 ULP versus the exact quantile across the whole open interval).
//!
//! Excel's own `NORMSINV`/`NORM.S.INV` *is* this rational approximation: its
//! published values sit 1–2 ULP from the exact quantile in the tails, so
//! reproducing AS 241 faithfully — rather than refining toward the exact
//! quantile with an erf/erfc Newton step — is precisely what matches the
//! oracle (a refinement would move the tail cases *away* from Excel's bits).
//! The central branch is evaluated as `q * (num/den)` — form the rational
//! value first, then scale by the offset `q = p − 0.5`; this reproduces the
//! farm-pinned bits exactly, whereas the Fortran operator-precedence grouping
//! `(q*num)/den` differs by 1 ULP at `p = 0.9`.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Coerce the one argument `p` via scalar numeric coercion (bool → 1/0,
//!   numeric text → number, blank → 0) (NORMSINV.md §Coercion).
//! - Domain: `p ≤ 0` or `p ≥ 1` → `#NUM!`. A CDF's range is the *open* interval
//!   (0, 1), so its inverse is undefined at and beyond the endpoints; this is
//!   the observed `NORMSINV(0)` → `#NUM!`, `NORMSINV(1)` → `#NUM!`
//!   (NORMSINV.md §1).
//! - Otherwise return the probit `z` with `Φ(z) = p` via AS 241
//!   (NORMSINV.md §1).
//! - A non-coercible text argument yields `#VALUE!`; an error-valued argument
//!   propagates as-is, no special containment (NORMSINV.md §Error behavior).

use xl_value::{ErrorKind, Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

/// The inverse standard-normal CDF (probit) via Wichura's Algorithm AS 241
/// (`PPND16`).
///
/// Returns `Some(z)` with `Φ(z) = p` for `p` in the open interval `(0, 1)`, or
/// `None` when `p ≤ 0` or `p ≥ 1` (outside the CDF's range — the caller maps
/// `None` to `#NUM!`).
///
/// This is the single source of truth for the numerical kernel: [`func_norminv`]
/// reuses it (`NORMINV(p, μ, σ) = μ + σ · probit(p)`) rather than duplicating
/// the coefficients. See the module docs for the algorithm and its provenance.
///
/// [`func_norminv`]: crate::func_norminv
// Coefficients are transcribed verbatim from the published AS 241 tables and
// kept at their as-published precision for auditability against the paper; the
// trailing digits beyond f64's ~17 significant figures are intentional.
#[allow(clippy::excessive_precision)]
pub(crate) fn probit(p: f64) -> Option<f64> {
    if p <= 0.0 || p >= 1.0 {
        return None;
    }

    let q = p - 0.5;
    let z = if q.abs() <= 0.425 {
        // Central region: rational approximation in `r = 0.180625 − q²`.
        let r = 0.180625 - q * q;
        let num = ((((((2.5090809287301226727e3 * r + 3.3430575583588128105e4) * r
            + 6.7265770927008700853e4)
            * r
            + 4.5921953931549871457e4)
            * r
            + 1.3731693765509461125e4)
            * r
            + 1.9715909503065514427e3)
            * r
            + 1.3314166789178437745e2)
            * r
            + 3.3871328727963666080e0;
        let den = ((((((5.2264952788528545610e3 * r + 2.8729085735721942674e4) * r
            + 3.9307895800092710610e4)
            * r
            + 2.1213794301586595867e4)
            * r
            + 5.3941960214247511077e3)
            * r
            + 6.8718700749205790830e2)
            * r
            + 4.2313330701600911252e1)
            * r
            + 1.0;
        // Form the rational value, *then* scale by `q` (matches Excel's bits).
        q * (num / den)
    } else {
        // Tail region: rational approximation in `r = sqrt(−ln(min(p, 1−p)))`.
        let r_min = if q < 0.0 { p } else { 1.0 - p };
        let r0 = (-r_min.ln()).sqrt();
        let val = if r0 <= 5.0 {
            // Intermediate tail.
            let r = r0 - 1.6;
            let num = ((((((7.74545014278341407640e-4 * r + 2.27238449892691845833e-2) * r
                + 2.41780725177450611770e-1)
                * r
                + 1.27045825245236838258e0)
                * r
                + 3.64784832476320460504e0)
                * r
                + 5.76949722146069140550e0)
                * r
                + 4.63033784615654529590e0)
                * r
                + 1.42343711074968357734e0;
            let den = ((((((1.05075007164441684324e-9 * r + 5.47593808499534494600e-4) * r
                + 1.51986665636164571966e-2)
                * r
                + 1.48103976427480074590e-1)
                * r
                + 6.89767334985100004550e-1)
                * r
                + 1.67638483018380384940e0)
                * r
                + 2.05319162663775882187e0)
                * r
                + 1.0;
            num / den
        } else {
            // Far tail.
            let r = r0 - 5.0;
            let num = ((((((2.01033439929228813265e-7 * r + 2.71155556874348757815e-5) * r
                + 1.24266094738807843860e-3)
                * r
                + 2.65321895265761230930e-2)
                * r
                + 2.96560571828504891230e-1)
                * r
                + 1.78482653991729133580e0)
                * r
                + 5.46378491116411436990e0)
                * r
                + 6.65790464350110377720e0;
            let den = ((((((2.04426310338993978564e-15 * r + 1.42151175831644588870e-7) * r
                + 1.84631831751005468180e-5)
                * r
                + 7.86869131145613259100e-4)
                * r
                + 1.48753612908506148525e-2)
                * r
                + 1.36929880922735805310e-1)
                * r
                + 5.99832206555887937690e-1)
                * r
                + 1.0;
            num / den
        };
        // AS 241 computes the upper-tail magnitude; mirror it for `p < 0.5`.
        if q < 0.0 { -val } else { val }
    };

    Some(z)
}

/// Evaluate a `NORMSINV(probability)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    match to_number(&args.eval_scalar(0)) {
        Ok(p) => match probit(p) {
            Some(z) => Value::number(z),
            // p ≤ 0 or p ≥ 1: outside the CDF's (0,1) range → #NUM!.
            None => Value::Error(ErrorKind::Num),
        },
        Err(k) => Value::Error(k),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    // All expected values below are farm-pinned:
    // RUN-2026-07-11-oracle01, OXP-153. Reproduced bit-for-bit by AS 241, so
    // the assertions are exact (`assert_eq!`) — no tolerance is claimed.

    #[test]
    fn median_is_zero() {
        // NORMSINV(0.5) = 0 exactly (the standard-normal median).
        assert_eq!(eval_direct(eval, vec![Scalar(num(0.5))]), num(0.0));
    }

    #[test]
    fn upper_and_lower_975_asymmetric_bits() {
        // The 0.975/0.025 pair is *not* bit-symmetric: the upper branch uses
        // r = 1 − 0.975 (a different float than 0.025), so the magnitudes
        // differ by 2 ULP — exactly as the farm observed.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.975))]),
            num(1.9599639845400536)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.025))]),
            num(-1.9599639845400538)
        );
    }

    #[test]
    fn ninety_nine_percentile() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.99))]),
            num(2.3263478740408408)
        );
    }

    #[test]
    fn lower_tail_thousandth() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.001))]),
            num(-3.090232306167813)
        );
    }

    #[test]
    fn far_upper_tail() {
        // p = 0.999999 exercises the intermediate-tail (r ≤ 5) branch.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.999999))]),
            num(4.753424308817089)
        );
    }

    #[test]
    fn central_branch_point_nine() {
        // p = 0.9 is the central branch; the `q * (num/den)` grouping is what
        // makes this bit-exact (the `(q*num)/den` grouping is 1 ULP high).
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.9))]),
            num(1.2815515655446006)
        );
    }

    #[test]
    fn zero_and_one_are_num_error() {
        // p = 0 and p = 1 are outside the open interval (0,1) → #NUM!.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(0.0))]),
            Value::Error(ErrorKind::Num)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.0))]),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn out_of_range_is_num_error() {
        // Negative and > 1 probabilities are equally out of range.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(-0.5))]),
            Value::Error(ErrorKind::Num)
        );
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(1.5))]),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn text_coercion() {
        // Numeric text coerces (to_number), then evaluates normally.
        assert_eq!(eval_direct(eval, vec![Scalar(txt("0.5"))]), num(0.0));
    }

    #[test]
    fn non_numeric_text_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn error_argument_propagates() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Div0))]),
            Value::Error(ErrorKind::Div0)
        );
    }
}
