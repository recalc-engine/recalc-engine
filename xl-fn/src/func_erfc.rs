//! `ERFC` — the complementary error function `erfc(x) = 1 − erf(x)`.
//!
//! # Provenance
//! Behavior contract: `docs/specs/ERFC.md`, which cites the Microsoft Learn
//! ERFC function page
//! (`https://support.microsoft.com/en-us/office/erfc-function-736e0318-70ba-4e8b-8d08-461fe68b71b3`).
//! Coercion is deferred to `xl-value`'s [`to_number`].
//!
//! # Numerical method (ERFC.md §Numerical method)
//! `erf`/`erfc` are computed with **W. J. Cody's rational-Chebyshev algorithm**
//! — W. J. Cody, "Rational Chebyshev Approximation for the Error Function",
//! *Math. Comp.* 23 (1969), pp. 631–637 (the `CALERF` procedure). This is a
//! clean-room reconstruction from the **published** coefficient tables; no GPL
//! source was consulted (a Recalc design rule). The [`erf`]/[`erfc`] kernel is
//! `pub(crate)`: [`crate::func_normdist`] reuses `erfc` for the standard-normal
//! CDF (`Φ(z) = ½·erfc(−z/√2)`) rather than duplicating the coefficients — the
//! same single-source-of-truth pattern as `func_normsinv::probit` /
//! `func_norminv`.
//!
//! ## Bug-for-bug `exp(-x²)` (OXP-226 — the fidelity fix, not an accuracy fix)
//! The tail regions evaluate `erfc(x) = R(x)·exp(-x²)`. The **target is Excel,
//! not mathematical truth** (the design rule: bug-for-bug fidelity). A dense
//! Excel-16.0 probe (OXP-226, 1130 points over the corpus argument range) shows
//! Excel's `ERFC` error vs true `erfc` grows as **≈0.5·x² ULP** — the exact
//! signature of forming `x²` in one f64 multiply and calling `exp` **without**
//! Cody's argument-splitting `exp(-y²) = exp(-ysq²)·exp(-del)` refinement
//! (Excel is ~correctly-rounded near 0, but ~11 ULP off true by x≈6 and up to
//! ~60 ULP by x≈8). The earlier kernel *did* apply the split, so it was
//! **more accurate than Excel** — which maximized the *disagreement* that shows
//! up in the corpus (bare `ERFC` cells were up to ~60 ULP from Excel; formulas
//! that difference near-equal `erfc`/`exp` terms amplified it further). So the
//! kernel deliberately uses the naive `(-(x*x)).exp()` to **track Excel's own
//! (un-split) `exp`**: on the OXP-226 grid this lifts 15-sig agreement from
//! 820/1130 → **1057/1130** (identical on Linux/macOS libm), bit-exact
//! 262 → 442. The residual ~73 points are ≤~1-ULP straddles of Excel's own
//! 15-sig storage boundary (dominated by the libm-`exp` implementation gap,
//! not the rational — evaluating `R(x)` in double-double does *not* help), and
//! are within the workbook-wide 15-significant-figure float rule
//! (`TOLERANCES.md`). Do **not** re-introduce the argument split to "improve
//! accuracy": it regresses fidelity against the pinned build.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `ERFC(x)` coerces its one argument via scalar numeric coercion
//!   (ERFC.md §Coercion) and returns `erfc(x)` (ERFC.md §1). A non-coercible
//!   text argument yields `#VALUE!`; an error-valued argument propagates
//!   as-is (ERFC.md §Error behavior).
//!
//! # Domain: whole real line (OXP-215, RUN-2026-07-16-oracle01)
//! Legacy Excel `ERFC` historically **required `x ≥ 0`** and returned `#NUM!`
//! for a negative argument; modern Excel (2010+) documents the whole real line.
//! Which behavior the pinned oracle build exhibits was **not** something we
//! could guess (Recalc Principle 2), so it was deferred to **OXP-215**.
//! **The probe (Excel 16.0) pins the modern behavior**: a negative argument
//! returns `erfc(x) ∈ (1, 2)`, not `#NUM!` — `ERFC(-1) = 1.8427007929497148`
//! (`= 2 − erfc(1)`), `ERFC(-0.5) = 1.5204998778130465`,
//! `ERFC(-3) = 1.9999779095030015`. The `erfc` kernel already evaluates all
//! reals (the sign fix-up `erfc(−x) = 2 − erfc(x)`), so `ERFC` now serves the
//! whole real line directly — the interim `#UNSUPPORTED!` domain guard is
//! removed. The full 8-point value grid is bit-pinned against the OXP-215
//! sidecar in the tests below (at the workbook-wide 15-significant-figure float
//! rule; the Cody kernel agrees with the pinned build to ≈1 ULP).

use xl_value::{Value, to_number};

use crate::args::CallArgs;
use crate::context::EvalContext;

// --- Cody CALERF coefficients (published; kept at as-published precision for
// auditability against the paper — the trailing digits beyond f64's ~17 sig
// figures are intentional). ---
#[allow(clippy::excessive_precision)]
mod coef {
    /// Threshold below which the direct `erf` rational approximation is used.
    pub(super) const THRESH: f64 = 0.46875;
    /// `1/√π`.
    pub(super) const SQRPI: f64 = 0.56418958354775628695;
    /// Argument (magnitude) at/above which `erfc` underflows to `0`.
    pub(super) const XBIG: f64 = 26.543;
    /// Below this magnitude `y·y` is negligible; guards against underflow.
    pub(super) const XSMALL: f64 = 1.11e-16;

    /// Numerator coefficients, `|x| ≤ 0.46875` (erf).
    pub(super) const A: [f64; 5] = [
        3.16112374387056560e00,
        1.13864154151050156e02,
        3.77485237685302021e02,
        3.20937758913846947e03,
        1.85777706184603153e-1,
    ];
    /// Denominator coefficients, `|x| ≤ 0.46875` (erf).
    pub(super) const B: [f64; 4] = [
        2.36012909523441209e01,
        2.44024637934444173e02,
        1.28261652607737228e03,
        2.84423683343917062e03,
    ];
    /// Numerator coefficients, `0.46875 ≤ |x| ≤ 4.0` (erfc).
    pub(super) const C: [f64; 9] = [
        5.64188496988670089e-1,
        8.88314979438837594e00,
        6.61191906371416295e01,
        2.98635138197400131e02,
        8.81952221241769090e02,
        1.71204761263407058e03,
        2.05107837782607147e03,
        1.23033935479799725e03,
        2.15311535474403846e-8,
    ];
    /// Denominator coefficients, `0.46875 ≤ |x| ≤ 4.0` (erfc).
    pub(super) const D: [f64; 8] = [
        1.57449261107098347e01,
        1.17693950891312499e02,
        5.37181101862009858e02,
        1.62138957456669019e03,
        3.29079923573345963e03,
        4.36261909014324716e03,
        3.43936767414372164e03,
        1.23033935480374942e03,
    ];
    /// Numerator coefficients, `|x| > 4.0` (asymptotic erfc).
    pub(super) const P: [f64; 6] = [
        3.05326634961232344e-1,
        3.60344899949804439e-1,
        1.25781726111229246e-1,
        1.60837851487422766e-2,
        6.58749161529837803e-4,
        1.63153871373020978e-2,
    ];
    /// Denominator coefficients, `|x| > 4.0` (asymptotic erfc).
    pub(super) const Q: [f64; 5] = [
        2.56852019228982242e00,
        1.87295284992346047e00,
        5.27905102951428412e-1,
        6.05183413124413191e-2,
        2.33520497626869185e-3,
    ];
}

/// `erfc(|x|)` for `y = |x| > THRESH`, via the two upper Cody regions.
/// Precondition: `y > coef::THRESH`. Returns `erfc` of the **magnitude** (the
/// caller applies the sign fix-up).
fn erfc_pos_tail(y: f64) -> f64 {
    if y <= 4.0 {
        // Middle region: rational approximation in `y`.
        let mut xnum = coef::C[8] * y;
        let mut xden = y;
        for (c, d) in coef::C[..7].iter().zip(coef::D[..7].iter()) {
            xnum = (xnum + c) * y;
            xden = (xden + d) * y;
        }
        let result = (xnum + coef::C[7]) / (xden + coef::D[7]);
        // Excel forms `x²` in one f64 multiply and calls its own `exp` — it does
        // NOT use Cody's argument-splitting `exp(-y²) = exp(-ysq²)·exp(-del)`
        // refinement. Reproducing Excel's naive `exp(-(y·y))` is the bug-for-bug
        // fidelity fix (OXP-226): the split made us *more accurate than Excel*,
        // which paradoxically maximized disagreement (Excel's own error grows as
        // ≈0.5·x² ULP, up to ~60 ULP by x≈8). Match Excel, do not out-compute it.
        (-(y * y)).exp() * result
    } else if y >= coef::XBIG {
        // erfc underflows to 0 beyond XBIG.
        0.0
    } else {
        // Asymptotic region: rational approximation in `1/y²`.
        let zsq = 1.0 / (y * y);
        let mut xnum = coef::P[5] * zsq;
        let mut xden = zsq;
        for (p, q) in coef::P[..4].iter().zip(coef::Q[..4].iter()) {
            xnum = (xnum + p) * zsq;
            xden = (xden + q) * zsq;
        }
        let mut result = zsq * (xnum + coef::P[4]) / (xden + coef::Q[4]);
        result = (coef::SQRPI - result) / y;
        // Naive `exp(-(y·y))` to match Excel bug-for-bug (OXP-226) — see the
        // middle-region note above; the argument-split refinement is omitted on
        // purpose so that our tail tracks Excel's (deliberately un-split) `exp`.
        (-(y * y)).exp() * result
    }
}

/// The complementary error function `erfc(x) = 1 − erf(x)`, over the whole real
/// line, via Cody's `CALERF`. Single source of truth for the kernel; reused by
/// `NORMDIST`'s CDF. See the module docs for the algorithm and its provenance.
pub(crate) fn erfc(x: f64) -> f64 {
    let y = x.abs();
    if y <= coef::THRESH {
        // Central region: evaluate erf directly, then erfc = 1 − erf.
        let ysq = if y > coef::XSMALL { y * y } else { 0.0 };
        let mut xnum = coef::A[4] * ysq;
        let mut xden = ysq;
        for (a, b) in coef::A[..3].iter().zip(coef::B[..3].iter()) {
            xnum = (xnum + a) * ysq;
            xden = (xden + b) * ysq;
        }
        let erf_val = x * (xnum + coef::A[3]) / (xden + coef::B[3]);
        return 1.0 - erf_val;
    }
    let tail = erfc_pos_tail(y);
    if x < 0.0 { 2.0 - tail } else { tail }
}

// NB: a standalone `erf(x)` kernel is intentionally NOT exposed — nothing wires
// it yet (NORMDIST's CDF uses `erfc` for better tail accuracy, and there is no
// ERF() function). "Wire it or cut it": cut until an ERF lever needs it, at
// which point it is a 3-line companion to `erfc`. Tests exercise the erf branch
// through `1 − erfc(x)` (the central region of `erfc` computes erf internally).

/// Evaluate an `ERFC(x)` call over the whole real line. See the module docs
/// (OXP-215 pins the pinned build to the modern whole-real-line domain).
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let x = match to_number(&args.eval_scalar(0)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    // OXP-215 (RUN-2026-07-16-oracle01): the pinned Excel 16.0 build returns
    // `erfc(x) ∈ (1, 2)` for a negative argument (modern domain), not the legacy
    // `#NUM!`. The Cody kernel already applies the `erfc(−x) = 2 − erfc(x)` sign
    // fix-up, so serve every real directly — no domain guard.
    Value::number(erfc(x))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::ErrorKind;

    /// Reference values computed to full double precision from the exact
    /// erf/erfc (independent of the Cody kernel — cross-checked against
    /// published tables / high-precision references). The kernel must
    /// reproduce them to ≤ a few ULP; the assertions use a tight relative
    /// bound because bit-exact oracle pinning is queued (OXP-215) but the
    /// mathematical target is unambiguous.
    fn assert_close(got: Value, want: f64) {
        match got {
            Value::Number(n) => {
                let rel = (n - want).abs() / want.abs().max(1e-300);
                assert!(rel < 1e-14, "got {n}, want {want} (rel {rel:e})");
            }
            other => panic!("expected Number, got {other:?}"),
        }
    }

    /// `erf(x) = 1 − erfc(x)` (the central-region erf path is inside `erfc`).
    fn erf(x: f64) -> f64 {
        1.0 - erfc(x)
    }

    #[test]
    fn erfc_kernel_reference_values() {
        assert!((erfc(0.0) - 1.0).abs() < 1e-15);
        assert!((erf(0.0)).abs() < 1e-15);
        // erf(1) = 0.842700792949714869...
        assert!((erf(1.0) - 0.8427007929497149).abs() < 1e-14);
        // erfc(1) = 0.157299207050285131...
        assert!((erfc(1.0) - 0.15729920705028513).abs() < 1e-14);
        // erfc(0.5) = 0.479500122186953462...
        assert!((erfc(0.5) - 0.4795001221869535).abs() < 1e-14);
        // erfc(2) = 0.004677734981047265...
        assert!((erfc(2.0) - 0.004677734981047266).abs() < 1e-16);
        // erfc(3) = 2.209049699858544e-5
        assert!((erfc(3.0) - 2.2090496998585438e-5).abs() < 1e-18);
    }

    #[test]
    fn erfc_kernel_symmetry_all_reals() {
        // erfc(-x) = 2 - erfc(x). Exercises the sign fix-up the ERFC *function*
        // defers but the kernel must honor (NORMDIST's CDF needs the negative
        // branch).
        for &x in &[0.3, 0.7, 1.5, 2.5, 5.0] {
            assert!((erfc(-x) - (2.0 - erfc(x))).abs() < 1e-14);
        }
    }

    #[test]
    fn erfc_function_nonnegative_core() {
        assert_close(eval_direct(eval, vec![Scalar(num(0.0))]), 1.0);
        assert_close(
            eval_direct(eval, vec![Scalar(num(1.0))]),
            0.15729920705028513,
        );
        assert_close(
            eval_direct(eval, vec![Scalar(num(2.0))]),
            0.004677734981047266,
        );
    }

    #[test]
    fn erfc_function_coerces_numeric_text() {
        assert_close(eval_direct(eval, vec![Scalar(txt("0"))]), 1.0);
    }

    /// OXP-215 (RUN-2026-07-16-oracle01, Excel 16.0) — the full observed value
    /// grid, pinned as the regression oracle. Positive and negative arguments
    /// alike; the negative branch (once deferred) is now served. Values are the
    /// exact sidecar floats; the Cody kernel matches them to ≤ a few ULP, so the
    /// assertion uses the module's tight relative bound (`assert_close`, the
    /// documented 15-sig float rule — bit-exact f64 identity with Excel's own
    /// summation is not claimed, per the module docs).
    #[test]
    fn erfc_function_oxp215_oracle_grid() {
        // Non-negative core.
        assert_close(eval_direct(eval, vec![Scalar(num(0.0))]), 1.0);
        assert_close(
            eval_direct(eval, vec![Scalar(num(0.5))]),
            0.4795001221869535,
        );
        assert_close(
            eval_direct(eval, vec![Scalar(num(1.0))]),
            0.15729920705028513,
        );
        assert_close(
            eval_direct(eval, vec![Scalar(num(2.0))]),
            0.0046777349810472645,
        );
        assert_close(
            eval_direct(eval, vec![Scalar(num(3.0))]),
            2.209049699858544e-05,
        );
        // Negative domain — pinned to the modern whole-real-line behavior
        // (erfc(-x) = 2 - erfc(x)), NOT the legacy #NUM!.
        assert_close(
            eval_direct(eval, vec![Scalar(num(-1.0))]),
            1.8427007929497148,
        );
        assert_close(
            eval_direct(eval, vec![Scalar(num(-0.5))]),
            1.5204998778130465,
        );
        assert_close(
            eval_direct(eval, vec![Scalar(num(-3.0))]),
            1.9999779095030015,
        );
    }

    /// The exact `xl-bench` 15-significant-figure scoring rule (round both to
    /// 15 sig figs, then bit-compare) — mirrors `xl_value::cmp_f64_fuzzy` so the
    /// bug-for-bug pins below are asserted at the *same* resolution the corpus
    /// fidelity gate uses. Kept local (no `xl-value` dep in the test).
    fn round15(x: f64) -> f64 {
        if x == 0.0 || !x.is_finite() {
            return x;
        }
        format!("{x:.14e}").parse().unwrap_or(x)
    }
    fn eq15(x: f64, y: f64) -> bool {
        if x == y {
            return true;
        }
        let diff = (x - y).abs();
        let scale = x.abs().max(y.abs());
        if diff > 1e-13 * scale {
            return false;
        }
        round15(x) == round15(y)
    }

    /// OXP-226 (RUN-2026-07-19-oracle01, Excel 16.0) — a dense 1130-point ERFC
    /// grid over the corpus argument range. Excel computes `erfc(x) =
    /// R(x)·exp(-x²)` with a **naive** `exp(-(x*x))` (no Cody argument split),
    /// so its error vs true `erfc` grows as ≈0.5·x² ULP — near-correctly-rounded
    /// at small `x`, but tens of ULP off true in the tail. The kernel now
    /// reproduces that (drops the split), so it **tracks Excel** rather than the
    /// mathematically-exact value. These pins lock the bug-for-bug contract at
    /// the 15-sig fidelity resolution: each is the value the pinned Excel build
    /// returns, and several are *deliberately far from* correctly-rounded
    /// `erfc` — re-introducing the argument split (making the kernel "more
    /// accurate") would move these back toward truth and **fail** these
    /// assertions, which is the point.
    #[test]
    fn erfc_oxp226_bug_for_bug_tracks_excel_naive_exp() {
        // (x, Excel's returned erfc(x)). Tail points where Excel ≠ correctly-
        // rounded erfc (the comment gives the ULP gap Excel has vs true erfc).
        let pins: &[(f64, f64)] = &[
            (4.0, 1.5417257900280017e-08), // ~1 ULP off true
            (5.0, 1.537459794428034e-12),  // ~5 ULP off true
            (6.0, 2.151973671249892e-17),  // ~2 ULP off true
            (8.0, 1.1224297172982929e-29), // ~2 ULP off true
            // Flagship: Excel is 62 ULP from correctly-rounded erfc(8.29)
            // (= 9.622079532066245e-32). The kernel must return Excel's value.
            (8.29, 9.622079532066177e-32),
            (9.0, 4.137031746513812e-37), // ~2 ULP off true
        ];
        for &(x, excel) in pins {
            let got = match eval_direct(eval, vec![Scalar(num(x))]) {
                Value::Number(n) => n,
                other => panic!("erfc({x}) => {other:?}, expected Number"),
            };
            assert!(
                eq15(got, excel),
                "erfc({x}) = {got:e} not 15-sig-equal to pinned Excel {excel:e}"
            );
        }
        // Make the bug-for-bug intent loud: at x = 8.29 we must NOT return the
        // correctly-rounded value — that is the regression this test guards.
        let got_829 = erfc(8.29);
        assert!(
            !eq15(got_829, 9.622079532066245e-32),
            "erfc(8.29) matched correctly-rounded erfc — the Cody argument split \
             was re-introduced, regressing bug-for-bug Excel fidelity (OXP-226)"
        );
    }

    #[test]
    fn erfc_function_non_numeric_text_is_value_error() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("x"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn erfc_function_error_argument_propagates() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(Value::Error(ErrorKind::Div0))]),
            Value::Error(ErrorKind::Div0)
        );
    }
}
