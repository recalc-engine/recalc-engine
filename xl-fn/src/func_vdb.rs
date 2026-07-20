//! `VDB` — depreciation of an asset for any period (including partial periods)
//! using the variable-declining-balance method, switching to straight-line when
//! that yields a larger deduction.
//!
//! # Provenance
//! Behavior contract: `docs/specs/VDB.md` (which cites the Microsoft Learn VDB
//! function page, verified 2026-07-11:
//! `https://support.microsoft.com/en-us/office/vdb-function-dde4e207-f3fa-488d-91d2-66d55e861d73`).
//! Numerics farm-pinned by `RUN-2026-07-11-oracle01`, experiment **OXP-158**;
//! domain-error kind (`#NUM!`) and the validity of `factor = 0` pinned by the
//! same run, experiment **OXP-159**.
//! Coercion via `xl-value`'s [`to_number`]; the optional-argument handling
//! mirrors [`crate::func_pmt`]'s scalar-first pattern, except `factor` cannot
//! default through `Blank -> 0` (its default is `2`, not `0`) so an omitted
//! `factor`/`no_switch` is detected by [`CallArgs::shape`] rather than coerced.
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - **Signature.** `VDB(cost, salvage, life, start_period, end_period,
//!   [factor], [no_switch])` (VDB.md §Signature). The first five are required;
//!   `factor` defaults to `2` (double-declining balance) and `no_switch`
//!   defaults to `FALSE`.
//! - **Method.** Declining-balance depreciation at rate `factor / life` is
//!   accrued period by period from period 0. When `no_switch` is `FALSE`
//!   (the default), each not-yet-switched period also computes the straight-line
//!   depreciation of the *remaining* book value over the *remaining* life; the
//!   first period where straight-line gives a **larger** deduction than
//!   declining-balance switches permanently to straight-line for the rest of the
//!   asset's life. `no_switch = TRUE` stays on declining-balance throughout
//!   (VDB.md §Semantics). Excel reads `no_switch` as a boolean flag: any nonzero
//!   value is `TRUE`.
//! - **Never below salvage.** Each period's deduction is floored so the book
//!   value never drops below `salvage` (`dep = min(dep, book - salvage)`). In the
//!   pinned grid it is this floor — not the straight-line switch — that produces
//!   the final period's `296` (`VDB(10000,1000,5,4,5)`): the raw declining
//!   balance `518.4` is capped at `book - salvage = 296` (VDB.md §Never below
//!   salvage).
//! - **Fractional periods.** `start_period` / `end_period` may be fractional; the
//!   boundary period is pro-rated by the fraction of it lying within
//!   `(start_period, end_period]`, e.g. `VDB(10000,1000,5,0,0.5)` is half the
//!   first year's `4000` = `2000` (VDB.md §Fractional periods).
//! - **Result.** The sum of per-period depreciation over the half-open interval
//!   `(start_period, end_period]`. It is additive — `VDB(0,1)+VDB(1,2) =
//!   VDB(0,2)` — which the whole-period pins confirm (`4000+2400 = 6400`).
//! - **Coercion / error propagation.** Every argument is coerced with
//!   [`to_number`] (number passes through; `TRUE`/`FALSE` -> `1`/`0`; numeric
//!   text -> its number; blank -> `0`). A non-coercible text argument -> `#VALUE!`;
//!   an error-valued argument propagates as-is, in left-to-right order (VDB.md
//!   §Coercion, §Error behavior).
//!
//! # Domain violations — #NUM! (OXP-159, RESOLVED)
//! The MS Learn page states "All arguments except no_switch must be positive
//! numbers" but does **not** document the error *kind* returned for a violation.
//! `RUN-2026-07-11-oracle01` (**OXP-159**) has now pinned it: a
//! positivity/ordering violation returns **`#NUM!`**. So an out-of-domain call —
//! `cost < 0`, `salvage < 0`, `life <= 0`, `start_period < 0`,
//! `end_period < start_period`, or `end_period > life` — returns
//! [`ErrorKind::Num`] (VDB.md §Domain violations). `end_period == life` stays
//! in-domain (the whole-life pin uses it).
//!
//! **`factor = 0` is VALID** (also OXP-159, `VDB(100,10,5,0,1,0)` -> `18`): a
//! zero declining-balance rate (`rate = factor / life = 0`) means the
//! straight-line switch fires in period 1, so the result is the straight-line
//! first-period deduction `(100 - 10) / 5` = **18**, not an error — the existing
//! computation handles it without a special case. Only a **negative** `factor`
//! remains unpinned by the farm; it stays deferred to a distinguishable
//! [`ErrorKind::Unsupported`] (fail loudly, never guess). Every OXP-158
//! farm-pinned target is in-domain and unaffected.

use xl_value::{ErrorKind, Value, to_number};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Evaluate a `VDB(cost, salvage, life, start_period, end_period, [factor],
/// [no_switch])` call. See the module docs for the semantics and provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // Coerce every argument to a scalar f64, propagating the first error in
    // left-to-right order. The five required operands first.
    let cost = match to_number(&args.eval_scalar(0)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let salvage = match to_number(&args.eval_scalar(1)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let life = match to_number(&args.eval_scalar(2)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let start_period = match to_number(&args.eval_scalar(3)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };
    let end_period = match to_number(&args.eval_scalar(4)) {
        Ok(v) => v,
        Err(k) => return Value::Error(k),
    };

    // `factor` is optional with default 2 (double-declining). Unlike PMT's
    // optionals it CANNOT lean on `Blank -> 0`, since 0 is not its default, so an
    // omitted position is detected by shape (out-of-range also classifies as
    // Omitted) rather than coerced.
    let factor = if args.shape(5) == ArgShape::Omitted {
        2.0
    } else {
        match to_number(&args.eval_scalar(5)) {
            Ok(v) => v,
            Err(k) => return Value::Error(k),
        }
    };

    // `no_switch` is optional with default FALSE, read as a boolean flag: any
    // nonzero coerced value is TRUE (an omitted position is FALSE).
    let no_switch = if args.shape(6) == ArgShape::Omitted {
        false
    } else {
        match to_number(&args.eval_scalar(6)) {
            Ok(v) => v != 0.0,
            Err(k) => return Value::Error(k),
        }
    };

    // Domain guard (OXP-159, RESOLVED): MS Learn requires all non-`no_switch`
    // arguments to be positive, plus the implicit ordering start <= end <= life.
    // RUN-2026-07-11-oracle01 pinned Excel's error kind for a violation to
    // `#NUM!` (see module docs). `end_period == life` is in-domain.
    let in_domain = cost >= 0.0
        && salvage >= 0.0
        && life > 0.0
        && start_period >= 0.0
        && end_period >= start_period
        && end_period <= life;
    if !in_domain {
        return Value::Error(ErrorKind::Num);
    }

    // `factor = 0` is VALID (OXP-159): a zero declining-balance rate switches to
    // straight-line in period 1, which the computation below yields naturally.
    // Only a *negative* factor is un-farm-pinned; keep it deferred to a
    // distinguishable `#UNSUPPORTED!` rather than guessing its kind.
    if factor < 0.0 {
        return Value::Error(ErrorKind::Unsupported);
    }

    Value::number(vdb(
        cost,
        salvage,
        life,
        start_period,
        end_period,
        factor,
        no_switch,
    ))
}

/// The variable-declining-balance depreciation accrued over `(start, end]`.
///
/// Walks the whole periods `1..=ceil(end)` (period `per` spans `[per-1, per]`),
/// accruing declining-balance depreciation with an optional permanent switch to
/// straight-line, flooring each deduction at `book - salvage`, and pro-rating the
/// boundary periods by their overlap with `(start, end]`. Callers guarantee the
/// arguments are in-domain (see [`eval`]), so `rate` is finite and `ceil(end)`
/// is a small non-negative count.
fn vdb(
    cost: f64,
    salvage: f64,
    life: f64,
    start: f64,
    end: f64,
    factor: f64,
    no_switch: bool,
) -> f64 {
    let rate = factor / life;
    // Cumulative full-period depreciation from period 0 (drives the book value).
    let mut accum = 0.0;
    // Depreciation accrued within the requested interval (start, end].
    let mut result = 0.0;
    // Once the straight-line switch fires it is permanent; `sl_dep` is the fixed
    // per-period straight-line amount captured at the switch.
    let mut switched = false;
    let mut sl_dep = 0.0;

    let n_periods = end.ceil() as i64;
    for per in 1..=n_periods {
        let book = cost - accum;

        // Declining-balance deduction for this whole period (un-floored).
        let db = book * rate;

        // Decide this period's deduction: straight-line if we have switched, else
        // declining-balance unless the straight-line of the remaining book value
        // over the remaining life is larger (which triggers the permanent switch).
        let mut dep = if switched {
            sl_dep
        } else {
            let remaining_life = life - (per as f64 - 1.0);
            let sl = if remaining_life > 0.0 {
                (book - salvage) / remaining_life
            } else {
                0.0
            };
            if !no_switch && sl > db {
                switched = true;
                sl_dep = sl;
                sl
            } else {
                db
            }
        };

        // Never depreciate below salvage.
        let floor = book - salvage;
        if dep > floor {
            dep = floor;
        }
        if dep < 0.0 {
            dep = 0.0;
        }

        // Pro-rate by the fraction of this whole period lying within (start, end].
        let p_lo = (per - 1) as f64;
        let p_hi = per as f64;
        let lo = start.max(p_lo);
        let hi = end.min(p_hi);
        if hi > lo {
            result += dep * (hi - lo);
        }

        accum += dep;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    /// Pull the `f64` out of a numeric result (panics on anything else).
    fn as_num(v: Value) -> f64 {
        match v {
            Value::Number(n) => n,
            other => panic!("expected a number, got {other:?}"),
        }
    }

    // Every farm target is a clean round number; VDB is a deterministic closed
    // computation, so they reproduce bit-exact. TIGHT guards against any ULP
    // drift in the declining-balance products without needing a tolerance
    // (a deterministic <=1e-9 match requires no TOLERANCES.md entry).
    const TIGHT: f64 = 1e-9;

    /// The shared cost/salvage/life used by every OXP-158 farm target.
    fn base(start: f64, end: f64) -> Vec<crate::test_support::TestArg> {
        vec![
            Scalar(num(10000.0)),
            Scalar(num(1000.0)),
            Scalar(num(5.0)),
            Scalar(num(start)),
            Scalar(num(end)),
        ]
    }

    // ---- OXP-158 farm-pinned targets (RUN-2026-07-11-oracle01) ----

    #[test]
    fn oxp158_period_0_to_1_double_declining_default_factor() {
        // 2/5 * 10000 = 4000 (default factor = 2).
        assert_eq!(eval_direct(eval, base(0.0, 1.0)), num(4000.0));
    }

    #[test]
    fn oxp158_period_1_to_2() {
        // 2/5 * (10000 - 4000) = 2400.
        assert_eq!(eval_direct(eval, base(1.0, 2.0)), num(2400.0));
    }

    #[test]
    fn oxp158_period_2_to_3() {
        assert_eq!(eval_direct(eval, base(2.0, 3.0)), num(1440.0));
    }

    #[test]
    fn oxp158_period_3_to_4() {
        assert_eq!(eval_direct(eval, base(3.0, 4.0)), num(864.0));
    }

    #[test]
    fn oxp158_period_4_to_5_salvage_floor_binds() {
        // Raw DB would be 518.4, dropping book below salvage; the salvage floor
        // caps it at book - salvage = 1296 - 1000 = 296.
        let got = as_num(eval_direct(eval, base(4.0, 5.0)));
        assert!((got - 296.0).abs() <= TIGHT, "got {got}, want 296.0");
    }

    #[test]
    fn oxp158_whole_life_equals_cost_minus_salvage() {
        // The whole life depreciates exactly cost - salvage = 9000.
        let got = as_num(eval_direct(eval, base(0.0, 5.0)));
        assert!((got - 9000.0).abs() <= TIGHT, "got {got}, want 9000.0");
    }

    #[test]
    fn oxp158_factor_one_and_a_half() {
        // factor = 1.5: 1.5/5 * 10000 = 3000.
        let mut args = base(0.0, 1.0);
        args.push(Scalar(num(1.5)));
        assert_eq!(eval_direct(eval, args), num(3000.0));
    }

    #[test]
    fn oxp158_no_switch_true() {
        // no_switch = TRUE, (0,3]: 4000 + 2400 + 1440 = 7840 (pure declining
        // balance — the switch never binds here anyway).
        let mut args = base(0.0, 3.0);
        args.push(Scalar(num(2.0))); // factor
        args.push(Scalar(Value::Bool(true))); // no_switch = TRUE
        let got = as_num(eval_direct(eval, args));
        assert!((got - 7840.0).abs() <= TIGHT, "got {got}, want 7840.0");
    }

    #[test]
    fn oxp158_no_switch_false_same_result() {
        // no_switch = FALSE over (0,3]: identical to TRUE — the straight-line
        // switch does not bind in this interval.
        let mut args = base(0.0, 3.0);
        args.push(Scalar(num(2.0))); // factor
        args.push(Scalar(Value::Bool(false))); // no_switch = FALSE
        let got = as_num(eval_direct(eval, args));
        assert!((got - 7840.0).abs() <= TIGHT, "got {got}, want 7840.0");
    }

    #[test]
    fn oxp158_fractional_end_period_half() {
        // (0, 0.5]: half of the first year's 4000 = 2000.
        assert_eq!(eval_direct(eval, base(0.0, 0.5)), num(2000.0));
    }

    /// All ten OXP-158 targets in one lockstep table, guarding the additive
    /// relationship VDB(0,1)+VDB(1,2)+... = VDB(0,5) alongside the individual
    /// values.
    #[test]
    fn oxp158_full_grid() {
        let cases: &[(f64, f64, f64)] = &[
            (0.0, 1.0, 4000.0),
            (1.0, 2.0, 2400.0),
            (2.0, 3.0, 1440.0),
            (3.0, 4.0, 864.0),
            (4.0, 5.0, 296.0),
            (0.0, 5.0, 9000.0),
            (0.0, 0.5, 2000.0),
        ];
        for &(s, e, want) in cases {
            let got = as_num(eval_direct(eval, base(s, e)));
            assert!(
                (got - want).abs() <= TIGHT,
                "VDB(..,{s},{e}) = {got}, want {want}"
            );
        }
    }

    // ---- MS Learn documented worked examples (cross-check, not farm pins) ----

    #[test]
    fn ms_learn_example_first_year_whole() {
        // MS Learn: =VDB(2400, 300, 10, 0, 1) -> $480.00.
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(2400.0)),
                Scalar(num(300.0)),
                Scalar(num(10.0)),
                Scalar(num(0.0)),
                Scalar(num(1.0)),
            ],
        ));
        assert!((got - 480.0).abs() <= TIGHT, "got {got}, want 480.0");
    }

    #[test]
    fn ms_learn_example_first_month_whole() {
        // MS Learn: =VDB(2400, 300, 10*12, 0, 1) -> $40.00.
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(2400.0)),
                Scalar(num(300.0)),
                Scalar(num(120.0)),
                Scalar(num(0.0)),
                Scalar(num(1.0)),
            ],
        ));
        assert!((got - 40.0).abs() <= TIGHT, "got {got}, want 40.0");
    }

    #[test]
    fn ms_learn_example_first_day_display_rounded() {
        // MS Learn: =VDB(2400, 300, 10*365, 0, 1) -> $1.32 (display-rounded).
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(num(2400.0)),
                Scalar(num(300.0)),
                Scalar(num(3650.0)),
                Scalar(num(0.0)),
                Scalar(num(1.0)),
            ],
        ));
        assert!((got - 1.32).abs() < 0.005, "got {got}, want ~1.32");
    }

    // ---- Coercion & argument handling ----

    #[test]
    fn default_factor_is_two_when_omitted() {
        // Omitting factor must default to 2 (double-declining), NOT Blank -> 0.
        // With factor = 0 the first period would straight-line to 1800, not the
        // 4000 a correct double-declining default gives.
        assert_eq!(eval_direct(eval, base(0.0, 1.0)), num(4000.0));
        // Explicitly Omitted trailing position behaves identically.
        let mut args = base(0.0, 1.0);
        args.push(Omitted);
        assert_eq!(eval_direct(eval, args), num(4000.0));
    }

    #[test]
    fn no_switch_nonzero_is_true() {
        // no_switch is a boolean flag: a nonzero number reads as TRUE, matching
        // the FALSE result here since the switch does not bind over (0,3].
        let mut args = base(0.0, 3.0);
        args.push(Scalar(num(2.0)));
        args.push(Scalar(num(2.0))); // nonzero -> TRUE
        let got = as_num(eval_direct(eval, args));
        assert!((got - 7840.0).abs() <= TIGHT, "got {got}, want 7840.0");
    }

    #[test]
    fn numeric_text_and_logical_coerce() {
        // "10000", "1000", "5", "0", "1" coerce through to_number -> 4000.
        let got = as_num(eval_direct(
            eval,
            vec![
                Scalar(txt("10000")),
                Scalar(txt("1000")),
                Scalar(txt("5")),
                Scalar(txt("0")),
                Scalar(txt("1")),
            ],
        ));
        assert!((got - 4000.0).abs() <= TIGHT, "got {got}, want 4000.0");
    }

    #[test]
    fn arg_error_propagates_left_to_right() {
        // cost's error wins over a later erroring argument.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Error(ErrorKind::Ref)),
                    Scalar(Value::Error(ErrorKind::Div0)),
                    Scalar(num(5.0)),
                    Scalar(num(0.0)),
                    Scalar(num(1.0)),
                ],
            ),
            Value::Error(ErrorKind::Ref)
        );
        // A non-coercible text argument -> #VALUE!.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(10000.0)),
                    Scalar(num(1000.0)),
                    Scalar(txt("abc")),
                    Scalar(num(0.0)),
                    Scalar(num(1.0)),
                ],
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    // ---- OXP-159 farm-pinned domain violations -> #NUM! ----
    // RUN-2026-07-11-oracle01 / OXP-159 pinned Excel's error kind for each
    // positivity/ordering violation to #NUM! (cost = 100, salvage = 10, life = 5
    // throughout, matching the probed grid).

    /// Build a 5-required-arg VDB call at the OXP-159 grid's scale.
    fn dom(
        cost: f64,
        salvage: f64,
        life: f64,
        start: f64,
        end: f64,
    ) -> Vec<crate::test_support::TestArg> {
        vec![
            Scalar(num(cost)),
            Scalar(num(salvage)),
            Scalar(num(life)),
            Scalar(num(start)),
            Scalar(num(end)),
        ]
    }

    #[test]
    fn oxp159_negative_cost_is_num() {
        // VDB(-100,10,5,0,1) -> #NUM! (RUN-2026-07-11-oracle01 / OXP-159).
        assert_eq!(
            eval_direct(eval, dom(-100.0, 10.0, 5.0, 0.0, 1.0)),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn oxp159_negative_salvage_is_num() {
        // VDB(100,-10,5,0,1) -> #NUM! (RUN-2026-07-11-oracle01 / OXP-159).
        assert_eq!(
            eval_direct(eval, dom(100.0, -10.0, 5.0, 0.0, 1.0)),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn oxp159_zero_life_is_num() {
        // VDB(100,10,0,0,1) -> #NUM! (RUN-2026-07-11-oracle01 / OXP-159).
        assert_eq!(
            eval_direct(eval, dom(100.0, 10.0, 0.0, 0.0, 1.0)),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn oxp159_start_after_end_is_num() {
        // VDB(100,10,5,3,2) -> #NUM! (RUN-2026-07-11-oracle01 / OXP-159).
        // (Would otherwise silently sum to 0 — a deceptive value.)
        assert_eq!(
            eval_direct(eval, dom(100.0, 10.0, 5.0, 3.0, 2.0)),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn oxp159_end_beyond_life_is_num() {
        // VDB(100,10,5,0,6) -> #NUM! (RUN-2026-07-11-oracle01 / OXP-159).
        assert_eq!(
            eval_direct(eval, dom(100.0, 10.0, 5.0, 0.0, 6.0)),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn oxp159_negative_start_is_num() {
        // VDB(100,10,5,-1,1) -> #NUM! (RUN-2026-07-11-oracle01 / OXP-159).
        assert_eq!(
            eval_direct(eval, dom(100.0, 10.0, 5.0, -1.0, 1.0)),
            Value::Error(ErrorKind::Num)
        );
    }

    #[test]
    fn oxp159_factor_zero_is_valid_straight_line() {
        // VDB(100,10,5,0,1,0) -> 18.0, NOT an error (RUN-2026-07-11-oracle01 /
        // OXP-159). factor = 0 => DB rate 0 => immediate straight-line switch,
        // so the first period is (100 - 10) / 5 = 18.
        let mut args = dom(100.0, 10.0, 5.0, 0.0, 1.0);
        args.push(Scalar(num(0.0))); // factor = 0
        let got = as_num(eval_direct(eval, args));
        assert!((got - 18.0).abs() <= TIGHT, "got {got}, want 18.0");
    }

    #[test]
    fn negative_factor_stays_deferred_unsupported() {
        // A *negative* factor was not farm-pinned by OXP-159; keep it deferred to
        // a distinguishable #UNSUPPORTED! rather than guessing its error kind.
        let mut args = dom(100.0, 10.0, 5.0, 0.0, 1.0);
        args.push(Scalar(num(-2.0))); // factor < 0
        assert_eq!(
            eval_direct(eval, args),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}
