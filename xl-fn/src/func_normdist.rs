//! `NORMDIST` / `NORM.DIST` — the normal distribution for the given mean and
//! standard deviation: its cumulative distribution function (CDF) when
//! `cumulative` is TRUE, or its probability density function (PDF) when FALSE.
//!
//! # Provenance
//! Behavior contract: `docs/specs/NORMDIST.md`, which cites the Microsoft Learn
//! NORMDIST / NORM.DIST function pages
//! (`https://support.microsoft.com/en-us/office/normdist-function-126db625-c53e-4591-9a22-c9ff422d6d58`,
//! `https://support.microsoft.com/en-us/office/norm-dist-function-edb1cc14-a21c-4e53-839d-8082074c9f8d`).
//! `NORM.DIST` (2010+) and legacy `NORMDIST` share one implementation — the
//! function was renamed, not changed.
//!
//! # Numerical method (NORMDIST.md §Numerical method)
//! - **CDF** (`cumulative = TRUE`): `Φ(z) = ½·erfc(−z/√2)` with the
//!   standardized `z = (x − μ)/σ`, using [`crate::func_erfc::erfc`] — the same
//!   clean-room Cody `CALERF` kernel that backs `ERFC` (no coefficient
//!   duplication; `func_normsinv::probit` / `func_norminv` set the precedent).
//!   The `erfc(−z/√2)` form (rather than `½(1 + erf(z/√2))`) avoids
//!   catastrophic cancellation in the left tail.
//! - **PDF** (`cumulative = FALSE`): `φ(x) = e^(−z²/2) / (σ·√(2π))`.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - Coerce `x`, `mean`, `standard_dev` via scalar numeric coercion and
//!   `cumulative` via logical coercion ([`to_bool`]), left-to-right; the first
//!   coercion error (or an error-valued argument) propagates (NORMDIST.md
//!   §Coercion / §Error behavior). Argument coercion happens **before** the
//!   domain check, matching `NORMINV`.
//! - `standard_dev ≤ 0` → `#NUM!` (a distribution needs a positive spread;
//!   documented on the MS page) (NORMDIST.md §1).
//! - `cumulative = TRUE` → the CDF `Φ((x−μ)/σ)`; `cumulative = FALSE` → the PDF
//!   `φ` (NORMDIST.md §1). `Value::number` maps any non-finite result to
//!   `#NUM!` per the crate-wide invariant.
//!
//! # Oracle confirmation (OXP-214, RUN-2026-07-16-oracle01)
//! The core (CDF/PDF, `σ ≤ 0` → `#NUM!`) is documented and implemented; OXP-214
//! ran on the pinned Excel 16.0 build as a **confirmation** (not a blocker) and
//! **both queued questions came back matching this implementation**: (1) the
//! value grid — the Cody-kernel CDF and the PDF agree with the pinned build to
//! the workbook-wide 15-significant-figure float rule (e.g.
//! `NORMDIST(1,0,1,TRUE) = 0.841344746068543`,
//! `NORMDIST(8,5,2,TRUE) = 0.9331927987311419`,
//! `NORMDIST(1.96,0,1,TRUE) = 0.9750021048517795`); (2) the error precedence —
//! `NORMDIST(1,0,0,"x")` returns **`#VALUE!`** (the non-logical `cumulative`
//! coercion error), *not* the `σ ≤ 0` `#NUM!`, confirming that all four
//! arguments are coerced before the domain check (the `NORMINV`-consistent
//! choice). The grid + precedence are bit-pinned in the tests below.

use std::f64::consts::{SQRT_2, TAU};

use xl_value::{ErrorKind, Value, to_bool, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::func_erfc::erfc;

/// The standard-normal CDF `Φ(z) = ½·erfc(−z/√2)`.
fn norm_cdf(z: f64) -> f64 {
    0.5 * erfc(-z / SQRT_2)
}

/// The normal PDF `φ(x) = e^(−z²/2) / (σ·√(2π))` for the standardized `z`.
/// `TAU = 2π`, so `√(2π) = TAU.sqrt()`.
fn norm_pdf(z: f64, sd: f64) -> f64 {
    (-0.5 * z * z).exp() / (sd * TAU.sqrt())
}

/// Evaluate a `NORMDIST(x, mean, standard_dev, cumulative)` call. See the
/// module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Coerce all four arguments left-to-right; the first coercion error (or an
    // error-valued argument) propagates — before the domain check (NORMINV
    // precedent; OXP-214 confirms the cumulative-vs-#NUM ordering).
    let x = match to_number(&args.eval_scalar(0)) {
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
    let cumulative = match to_bool(&args.eval_scalar(3)) {
        Ok(b) => b,
        Err(k) => return Value::Error(k),
    };

    // A non-positive standard deviation has no distribution → #NUM!
    // (documented).
    if sd <= 0.0 {
        return Value::Error(ErrorKind::Num);
    }

    let z = (x - mean) / sd;
    if cumulative {
        Value::number(norm_cdf(z))
    } else {
        Value::number(norm_pdf(z, sd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    fn call(x: f64, mean: f64, sd: f64, cum: Value) -> Value {
        eval_direct(
            eval,
            vec![
                Scalar(num(x)),
                Scalar(num(mean)),
                Scalar(num(sd)),
                Scalar(cum),
            ],
        )
    }

    /// Assert a Number result within a tight relative bound. Bit-exact oracle
    /// pinning is queued (OXP-214); the mathematical targets here are
    /// unambiguous (standard-normal CDF/PDF reference values).
    fn assert_close(got: Value, want: f64) {
        match got {
            Value::Number(n) => {
                let rel = (n - want).abs() / want.abs().max(1e-300);
                assert!(rel < 1e-13, "got {n}, want {want} (rel {rel:e})");
            }
            other => panic!("expected Number, got {other:?}"),
        }
    }

    #[test]
    fn standard_normal_cdf_reference_values() {
        // Φ(0)=0.5, Φ(1)=0.8413447460685429, Φ(-1)=0.15865525393145707,
        // Φ(1.96)=0.9750021048517795.
        assert_close(call(0.0, 0.0, 1.0, Value::bool(true)), 0.5);
        assert_close(call(1.0, 0.0, 1.0, Value::bool(true)), 0.8413447460685429);
        assert_close(call(-1.0, 0.0, 1.0, Value::bool(true)), 0.15865525393145707);
        assert_close(call(1.96, 0.0, 1.0, Value::bool(true)), 0.9750021048517795);
    }

    #[test]
    fn general_normal_cdf_reference_value() {
        // NORMDIST(8, 5, 2, TRUE) = Φ(1.5) = 0.9331927987311419.
        assert_close(call(8.0, 5.0, 2.0, Value::bool(true)), 0.9331927987311419);
    }

    #[test]
    fn standard_normal_pdf_reference_values() {
        // φ(0)=0.3989422804014327, φ(1)=0.24197072451914337.
        assert_close(call(0.0, 0.0, 1.0, Value::bool(false)), 0.3989422804014327);
        assert_close(call(1.0, 0.0, 1.0, Value::bool(false)), 0.24197072451914337);
    }

    #[test]
    fn general_normal_pdf_reference_value() {
        // NORMDIST(8, 5, 2, FALSE) = φ((8-5)/2)/2 = 0.0647587978329459.
        assert_close(call(8.0, 5.0, 2.0, Value::bool(false)), 0.0647587978329459);
    }

    #[test]
    fn cumulative_coerces_number_and_text() {
        // Nonzero number and "TRUE"/"FALSE" text coerce like the logical.
        assert_close(call(1.0, 0.0, 1.0, num(1.0)), 0.8413447460685429);
        assert_close(call(1.0, 0.0, 1.0, num(0.0)), 0.24197072451914337);
        assert_close(call(1.0, 0.0, 1.0, txt("TRUE")), 0.8413447460685429);
    }

    #[test]
    fn nonpositive_sd_is_num_error() {
        assert_eq!(
            call(1.0, 0.0, 0.0, Value::bool(true)),
            Value::Error(ErrorKind::Num)
        );
        assert_eq!(
            call(1.0, 0.0, -2.0, Value::bool(true)),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn non_numeric_text_arg_is_value_error() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("nope")),
                    Scalar(num(0.0)),
                    Scalar(num(1.0)),
                    Scalar(Value::bool(true)),
                ],
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn non_logical_cumulative_is_value_error() {
        assert_eq!(
            call(1.0, 0.0, 1.0, txt("maybe")),
            Value::Error(ErrorKind::Value)
        );
    }

    /// OXP-214 (RUN-2026-07-16-oracle01, Excel 16.0) — the observed value grid
    /// plus the error-precedence probe, pinned as the regression oracle. The
    /// float assertions use the module's tight relative bound (`assert_close`,
    /// the documented 15-sig rule); the `#VALUE!` case pins the coercion-before-
    /// domain-check ordering.
    #[test]
    fn normdist_oxp214_oracle_grid() {
        assert_close(call(1.0, 0.0, 1.0, Value::bool(true)), 0.841344746068543);
        assert_close(call(1.0, 0.0, 1.0, Value::bool(false)), 0.24197072451914337);
        assert_close(call(8.0, 5.0, 2.0, Value::bool(true)), 0.9331927987311419);
        assert_close(call(-1.0, 0.0, 1.0, Value::bool(true)), 0.158655253931457);
        assert_close(call(1.96, 0.0, 1.0, Value::bool(true)), 0.9750021048517795);
        // Error precedence: cumulative "x" (non-logical → #VALUE!) with sd = 0
        // (would be #NUM!). Excel returns #VALUE! — the coercion error wins
        // because all four arguments are coerced before the sd ≤ 0 domain check.
        assert_eq!(
            call(1.0, 0.0, 0.0, txt("x")),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn error_argument_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Error(ErrorKind::Div0)),
                    Scalar(num(0.0)),
                    Scalar(num(1.0)),
                    Scalar(Value::bool(true)),
                ],
            ),
            Value::Error(ErrorKind::Div0)
        );
    }
}
