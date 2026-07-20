//! Shared plumbing for the date functions (`YEAR`/`MONTH`/`DAY`/`DATE`/
//! `EOMONTH`): argument coercion (deferred entirely to `xl-value`) and the
//! mapping from a [`DateError`] to an Excel error value.
//!
//! All numeric coercion here goes through [`xl_value::to_number`] (scalar
//! context: `TRUE`/`FALSE` → 1/0, numeric text → its number, blank → 0). The
//! date functions never reimplement coercion — the coercion contract is frozen
//! in `xl-value` (a Recalc design rule). In particular a **date-literal text string** like
//! `"5/8/2008"` is *not* parsed to a serial here: `to_number` reports it as
//! `#VALUE!`, and the eventual Excel behavior (which formats it accepts) is
//! tracked by `OXP-001` in `docs/oracle-experiments.md`, not guessed.

use xl_value::{ErrorKind, Value, to_number};

use crate::datecore::DateError;

/// Map a [`DateError`] onto the Excel error the spec prescribes:
/// out-of-range → `#NUM!`; the "January 0, 1900" special → `#UNSUPPORTED!`.
///
/// `OXP-090` (RUN-2026-07-11-oracle01) pinned serial 0's calendar identity, and
/// the *probed* functions `YEAR`/`MONTH`/`DAY`/`DATE` resolve it themselves
/// (they intercept [`DateError::JanuaryZero`] before it reaches here). This
/// mapping therefore only fires for the **unprobed** date functions
/// (`WEEKDAY`/`EDATE`/`EOMONTH`/`DAYS360`), which keep deferring rather than
/// guessing their day-0 behavior.
pub(crate) fn map_date_error(err: DateError) -> ErrorKind {
    match err {
        DateError::OutOfRange => ErrorKind::Num,
        DateError::JanuaryZero => ErrorKind::Unsupported,
    }
}

/// The largest `f64` magnitude a date argument may have before integer
/// truncation loses precision; beyond it no valid Excel date is reachable, so
/// the result is `#NUM!`. (Excel's real domain is far tighter; this is only a
/// safety rail keeping the downstream integer math well-defined.)
const ARG_MAGNITUDE_LIMIT: f64 = 1e15;

/// Coerce a **serial-number** argument (`YEAR`/`MONTH`/`DAY`/`EOMONTH`'s
/// `start_date`): scalar numeric coercion, then floor to a whole day.
///
/// Fractional serials floor to the day (per the task/spec: a time-of-day
/// component is dropped). An error value propagates; a magnitude past the
/// safety rail is `#NUM!`.
pub(crate) fn coerce_serial(value: &Value) -> Result<i64, ErrorKind> {
    let n = to_number(value)?;
    if !n.is_finite() || n.abs() >= ARG_MAGNITUDE_LIMIT {
        return Err(ErrorKind::Num);
    }
    // Floor: 2.9 → day 2, and negatives floor downward (−0.5 → −1, later
    // rejected as out-of-range).
    Ok(n.floor() as i64)
}

/// Coerce a **floored** integer date argument (`DATE`'s year/month/day): scalar
/// numeric coercion, then round a non-integer **toward negative infinity**
/// (`INT`/floor).
///
/// The exact truncation direction was resolved by the oracle
/// (RUN-2026-07-11-oracle01, `OXP-091`): Excel's `DATE` **floors** each of
/// year/month/day. `DATE(2020,-1.5,1)` uses month `-2` (→ 2019-10-01, serial
/// 43739) and `DATE(2020,1,-1.5)` uses day `-2` (serial 43828) — both floor,
/// not truncate-toward-zero (which would give `-1`) and not round (which would
/// turn `DATE(2020.9,1,1)`'s year into 2021 — the observed serial is 43831 =
/// 2020-01-01, so the year floors to 2020). A magnitude past the safety rail is
/// `#NUM!`; an error value propagates.
pub(crate) fn coerce_int_floor(value: &Value) -> Result<i64, ErrorKind> {
    let n = to_number(value)?;
    if !n.is_finite() || n.abs() >= ARG_MAGNITUDE_LIMIT {
        return Err(ErrorKind::Num);
    }
    Ok(n.floor() as i64)
}

/// Coerce a **truncated-toward-zero** integer month-offset argument
/// (`EOMONTH`/`EDATE`'s `months`, `WORKDAY`'s `days`): scalar numeric coercion,
/// then round a non-integer **toward zero** (`TRUNC`).
///
/// Resolved by the oracle (RUN-2026-07-11-oracle01): `EOMONTH(...,-1.9)` uses
/// months `-1` (`OXP-092`, serial 43830 = 2019-12-31) and `EDATE("2020-01-15",
/// -1.5)` uses months `-1` (`OXP-136`, serial 43814 = 2019-12-15) — truncate
/// toward zero, **not** floor (which would give `-2`). This is the opposite
/// direction from `DATE` (see [`coerce_int_floor`]); the two Excel code paths
/// genuinely differ, so each is pinned from its own probe rather than assumed
/// consistent. `WORKDAY`'s `days` truncation (`OXP-137`) was probed only for a
/// positive value (`1.9` → `1`, where floor and truncate agree) and follows the
/// same month-offset family. A magnitude past the safety rail is `#NUM!`; an
/// error value propagates.
pub(crate) fn coerce_int_trunc(value: &Value) -> Result<i64, ErrorKind> {
    let n = to_number(value)?;
    if !n.is_finite() || n.abs() >= ARG_MAGNITUDE_LIMIT {
        return Err(ErrorKind::Num);
    }
    Ok(n.trunc() as i64)
}
