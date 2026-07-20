//! Serial-date core: Excel serial number ↔ `(year, month, day)`, including the
//! infamous 1900 leap-year bug.
//!
//! # Provenance
//! Behavior contract: `docs/specs/DATE.md` (and the YEAR/MONTH/DAY/EOMONTH
//! specs), which cite the Microsoft Learn date-function pages and Microsoft's
//! documented Lotus-1-2-3-compatibility leap-year quirk
//! (<https://learn.microsoft.com/office/troubleshoot/excel/wrongly-assumes-1900-is-leap-year>).
//! Pure integer calendar math — no `chrono`, no external deps (the Recalc design rules
//! zero-dependency rule).
//!
//! # The two date systems (workbook state, not a function argument)
//! Excel stores a date as an integer *serial number*. Which calendar the serial
//! denotes depends on the workbook-level 1900/1904 flag ([`DateSystem`], read
//! from `xl-io`'s `workbookPr/@date1904` and threaded through
//! [`crate::EvalContext`]):
//!
//! - **1900 system** (Windows default): serial `1` = 1900-01-01. Excel treats
//!   1900 as a leap year (it is **not**, under the true proleptic Gregorian
//!   calendar) for Lotus 1-2-3 compatibility, so serial `60` denotes the
//!   *fictitious* "1900-02-29". Concretely: `1`→1900-01-01, `59`→1900-02-28,
//!   `60`→(fake) 1900-02-29, `61`→1900-03-01. Every date from 1900-03-01 on is
//!   therefore offset by exactly one day from the phantom-free proleptic count.
//! - **1904 system** (legacy Mac default): serial `0` = 1904-01-01, no leap
//!   bug (it never touches 1900).
//!
//! # How the phantom day is reproduced
//! We anchor the 1900 system at the *proleptic* ordinal of 1899-12-31 (the day
//! before serial 1) and split at the seam:
//! - serials `1..=59` are `anchor + serial` (real dates 1900-01-01..1900-02-28),
//! - serial `60` is returned as the literal `(1900, 2, 29)` — a date the
//!   proleptic calendar does not contain,
//! - serials `61..` are `anchor + serial - 1` (the phantom day has consumed one
//!   serial, so everything after shifts back by one to land on the real date).
//!
//! The forward direction (calendar → serial) mirrors this: a real proleptic
//! date whose phantom-free "base" serial is `≤ 59` keeps that serial; `≥ 60`
//! gets `+1`. Crucially, `DATE`/`EOMONTH` build a result by taking the serial of
//! the *first of a month* (always a real day-1 date) and adding a whole-day
//! offset **in serial space**, so the phantom day is crossed automatically:
//! `DATE(1900,2,1)` = serial 32, and `+ (29-1)` days lands on serial 60, exactly
//! Excel's fake 1900-02-29.
//!
//! The civil↔days conversions are Howard Hinnant's well-known public-domain
//! `days_from_civil`/`civil_from_days` algorithms (a standard, widely-published
//! calendar identity — not read from any GPL source).

/// The workbook's active date system (`workbookPr/@date1904`, ECMA-376
/// §18.2.28). Mirrors `xl_io::DateSystem`; `xl-fn` keeps its own copy because it
/// must not depend on `xl-io` (the dependency runs the other way, through
/// `xl-engine`). Defaults to the 1900 system, matching Excel on Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DateSystem {
    /// Serial 1 = 1900-01-01, with the fictitious 1900-02-29 at serial 60.
    #[default]
    Excel1900,
    /// Serial 0 = 1904-01-01; no 1900 leap-year bug.
    Excel1904,
}

/// Why a serial cannot be resolved to a calendar date (or a calendar date to a
/// serial). The function layer maps these onto Excel error values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DateError {
    /// The serial is outside the representable range for its date system
    /// (negative, or past 9999-12-31) → `#NUM!`.
    OutOfRange,
    /// Serial 0 in the **1900** system — Excel's special "January 0, 1900".
    ///
    /// Its calendar identity was pinned by the oracle (`OXP-090`,
    /// RUN-2026-07-11-oracle01) as `YEAR`=1900, `MONTH`=1, `DAY`=0, and the
    /// four *probed* functions resolve it directly — `YEAR`/`MONTH`/`DAY`
    /// intercept this variant to return `1900`/`1`/`0`, and `DATE` maps a
    /// serial-0 result to the plain number `0` (`=DATE(1900,1,0)` = `0`). This
    /// variant is retained as the shared signal so the **unprobed** date
    /// functions (`WEEKDAY`/`EDATE`/`EOMONTH`/`DAYS360`, whose day-0 arithmetic
    /// this run did not measure) keep surfacing `#UNSUPPORTED!` via
    /// [`crate::date_common::map_date_error`] rather than guessing.
    JanuaryZero,
}

/// Days from 1970-01-01 to the proleptic-Gregorian date `(y, m, d)`
/// (Howard Hinnant, public domain). `m` in `1..=12`, `d` in `1..=31`.
const fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Inverse of [`days_from_civil`]: proleptic-Gregorian `(year, month, day)` for
/// a count of days from 1970-01-01 (Howard Hinnant, public domain).
const fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// Proleptic ordinal of 1899-12-31 — the day *before* 1900 serial 1.
const ANCHOR_1900: i64 = days_from_civil(1899, 12, 31);
/// Proleptic ordinal of 1904-01-01 — 1904 serial 0.
const ANCHOR_1904: i64 = days_from_civil(1904, 1, 1);
/// Largest 1900-system serial: 9999-12-31. The `+1` is the phantom-day shift
/// (this date is well past the 1900-03-01 seam). Asserted `== 2_958_465` in the
/// unit tests, matching Microsoft's documented maximum.
const MAX_SERIAL_1900: i64 = days_from_civil(9999, 12, 31) - ANCHOR_1900 + 1;
/// Largest 1904-system serial: 9999-12-31 (no phantom-day shift).
const MAX_SERIAL_1904: i64 = days_from_civil(9999, 12, 31) - ANCHOR_1904;

/// A sane guard band for a normalized year before it reaches
/// [`days_from_civil`], keeping the era arithmetic far from `i64` overflow.
/// Any year outside this band cannot be a valid Excel date, so the caller maps
/// it to out-of-range.
const YEAR_GUARD: std::ops::RangeInclusive<i64> = -1..=11000;

/// Resolve a whole-day serial number to `(year, month, day)` in the given date
/// system, or a [`DateError`] describing why it cannot be.
///
/// The 1900 leap bug lives here: `serial_to_ymd(60, Excel1900)` is
/// `Ok((1900, 2, 29))`.
pub(crate) fn serial_to_ymd(serial: i64, system: DateSystem) -> Result<(i32, u32, u32), DateError> {
    match system {
        DateSystem::Excel1900 => {
            if serial == 0 {
                return Err(DateError::JanuaryZero);
            }
            if !(1..=MAX_SERIAL_1900).contains(&serial) {
                return Err(DateError::OutOfRange);
            }
            if serial == 60 {
                // The fictitious 1900-02-29 — the headline bug.
                return Ok((1900, 2, 29));
            }
            // Before the seam serials line up with the proleptic count; after
            // it, the phantom day has consumed one serial.
            let ordinal = if serial <= 59 {
                ANCHOR_1900 + serial
            } else {
                ANCHOR_1900 + serial - 1
            };
            let (y, m, d) = civil_from_days(ordinal);
            Ok((y as i32, m, d))
        }
        DateSystem::Excel1904 => {
            if !(0..=MAX_SERIAL_1904).contains(&serial) {
                return Err(DateError::OutOfRange);
            }
            let (y, m, d) = civil_from_days(ANCHOR_1904 + serial);
            Ok((y as i32, m, d))
        }
    }
}

/// Serial number of the first day of `(year, month)` (month `1..=12`, year an
/// already-resolved real year). Returns [`DateError::OutOfRange`] if the year is
/// outside the guard band. This is the building block `DATE`/`EOMONTH` add a
/// whole-day offset onto **in serial space**, which is what makes the phantom
/// 1900-02-29 fall out for free.
fn first_of_month_serial(year: i64, month: u32, system: DateSystem) -> Result<i64, DateError> {
    if !YEAR_GUARD.contains(&year) {
        return Err(DateError::OutOfRange);
    }
    let ordinal = days_from_civil(year, month as i64, 1);
    Ok(match system {
        DateSystem::Excel1900 => {
            // A first-of-month is never the phantom Feb-29, so the seam test is
            // a clean `≤ 59` split.
            let base = ordinal - ANCHOR_1900;
            if base <= 59 { base } else { base + 1 }
        }
        DateSystem::Excel1904 => ordinal - ANCHOR_1904,
    })
}

/// Range-check a computed serial against its date system's representable domain,
/// mapping serial 0 (1900) to the same "January 0, 1900" deferral as
/// [`serial_to_ymd`].
fn check_serial_domain(serial: i128, system: DateSystem) -> Result<i64, DateError> {
    let (min, max, jan_zero) = match system {
        DateSystem::Excel1900 => (0i128, MAX_SERIAL_1900 as i128, true),
        DateSystem::Excel1904 => (0i128, MAX_SERIAL_1904 as i128, false),
    };
    if jan_zero && serial == 0 {
        return Err(DateError::JanuaryZero);
    }
    // For 1900 the valid floor is serial 1 (serial 0 handled above); for 1904 it
    // is serial 0.
    let floor = if jan_zero { 1 } else { min };
    if !(floor..=max).contains(&serial) {
        return Err(DateError::OutOfRange);
    }
    Ok(serial as i64)
}

/// Construct a serial from a normalized/overflowed `(year, month, day)` triple,
/// as `DATE` does: normalize `month` into `year`, take the serial of the first
/// of that month, then add `day - 1` days in serial space.
///
/// `year` here is the already-year-resolved value (the 0..1899 → +1900 rule is
/// applied by the caller); `month` and `day` are the raw integer arguments
/// (may be `≤ 0` or overflow, which this normalizes).
pub(crate) fn date_to_serial(
    year: i64,
    month: i64,
    day: i64,
    system: DateSystem,
) -> Result<i64, DateError> {
    // Normalize month into the year with Euclidean (floor) division so negative
    // and zero months roll back correctly: DATE(2020,0,1) → Dec 2019,
    // DATE(2020,14,1) → Feb 2021. `total` is 0-based months since year 0.
    let total = year
        .checked_mul(12)
        .and_then(|ym| ym.checked_add(month - 1))
        .ok_or(DateError::OutOfRange)?;
    let ynorm = total.div_euclid(12);
    let mnorm = (total.rem_euclid(12) + 1) as u32; // 1..=12

    let first = first_of_month_serial(ynorm, mnorm, system)?;
    // The day offset is applied in serial space; use i128 so a huge `day`
    // cannot overflow before the domain check rejects it.
    let serial = first as i128 + (day as i128 - 1);
    check_serial_domain(serial, system)
}

/// Serial of the last day of the month `months` before/after the month
/// containing `start_serial` — the core of `EOMONTH`. Computed as "first of the
/// following month, minus one day" in serial space (so Feb 1900's length of 29
/// days falls out of the phantom day automatically).
pub(crate) fn eomonth_serial(
    start_serial: i64,
    months: i64,
    system: DateSystem,
) -> Result<i64, DateError> {
    let (y, m, _d) = serial_to_ymd(start_serial, system)?;
    // 0-based month index of the *following* month after the shift.
    let total = (y as i64)
        .checked_mul(12)
        .and_then(|ym| ym.checked_add(m as i64 - 1))
        .and_then(|t| t.checked_add(months))
        .and_then(|t| t.checked_add(1))
        .ok_or(DateError::OutOfRange)?;
    let ynext = total.div_euclid(12);
    let mnext = (total.rem_euclid(12) + 1) as u32;
    let first_next = first_of_month_serial(ynext, mnext, system)?;
    check_serial_domain(first_next as i128 - 1, system)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- the 1900 leap-year bug seam (DATE.md §5, YEAR/MONTH/DAY.md) --------

    #[test]
    fn leap_bug_seam_serials_59_60_61() {
        assert_eq!(serial_to_ymd(59, DateSystem::Excel1900), Ok((1900, 2, 28)));
        // THE BUG: serial 60 is the fictitious 1900-02-29.
        assert_eq!(serial_to_ymd(60, DateSystem::Excel1900), Ok((1900, 2, 29)));
        assert_eq!(serial_to_ymd(61, DateSystem::Excel1900), Ok((1900, 3, 1)));
    }

    #[test]
    fn serial_one_is_1900_01_01() {
        assert_eq!(serial_to_ymd(1, DateSystem::Excel1900), Ok((1900, 1, 1)));
    }

    #[test]
    fn max_serial_1900_is_2958465_and_is_9999_12_31() {
        assert_eq!(MAX_SERIAL_1900, 2_958_465);
        assert_eq!(
            serial_to_ymd(2_958_465, DateSystem::Excel1900),
            Ok((9999, 12, 31))
        );
    }

    #[test]
    fn out_of_range_and_january_zero() {
        assert_eq!(
            serial_to_ymd(-1, DateSystem::Excel1900),
            Err(DateError::OutOfRange)
        );
        assert_eq!(
            serial_to_ymd(0, DateSystem::Excel1900),
            Err(DateError::JanuaryZero)
        );
        assert_eq!(
            serial_to_ymd(2_958_466, DateSystem::Excel1900),
            Err(DateError::OutOfRange)
        );
    }

    // ---- 1904 system (DATE.md §6) ------------------------------------------

    #[test]
    fn system_1904_epoch_and_no_leap_bug() {
        assert_eq!(serial_to_ymd(0, DateSystem::Excel1904), Ok((1904, 1, 1)));
        // 1904 is a real leap year; there is no phantom day. Serial 59 in 1904
        // is 1904-02-29 (a *real* leap day), 60 is 1904-03-01.
        assert_eq!(serial_to_ymd(59, DateSystem::Excel1904), Ok((1904, 2, 29)));
        assert_eq!(serial_to_ymd(60, DateSystem::Excel1904), Ok((1904, 3, 1)));
        assert_eq!(
            serial_to_ymd(-1, DateSystem::Excel1904),
            Err(DateError::OutOfRange)
        );
    }

    #[test]
    fn max_serial_1904_is_9999_12_31() {
        assert_eq!(
            serial_to_ymd(MAX_SERIAL_1904, DateSystem::Excel1904),
            Ok((9999, 12, 31))
        );
        assert_eq!(
            serial_to_ymd(MAX_SERIAL_1904 + 1, DateSystem::Excel1904),
            Err(DateError::OutOfRange)
        );
    }

    // ---- round-trips across the whole range --------------------------------

    #[test]
    fn round_trip_1900_every_serial() {
        // serial → (y,m,d) → serial must be the identity across the domain,
        // including the phantom day at 60. (Sampled densely near the seam,
        // strided elsewhere for runtime.)
        let check = |s: i64| {
            let (y, m, d) = serial_to_ymd(s, DateSystem::Excel1900).unwrap();
            let back = date_to_serial(y as i64, m as i64, d as i64, DateSystem::Excel1900).unwrap();
            assert_eq!(back, s, "round-trip failed at serial {s} → {y}-{m}-{d}");
        };
        for s in 1..=400 {
            check(s);
        }
        let mut s = 400;
        while s <= MAX_SERIAL_1900 {
            check(s);
            s += 997; // a prime stride to hit varied month/day/year positions
        }
        check(MAX_SERIAL_1900);
    }

    #[test]
    fn round_trip_1904_strided() {
        let mut s = 0;
        while s <= MAX_SERIAL_1904 {
            let (y, m, d) = serial_to_ymd(s, DateSystem::Excel1904).unwrap();
            let back = date_to_serial(y as i64, m as i64, d as i64, DateSystem::Excel1904).unwrap();
            assert_eq!(back, s, "1904 round-trip failed at serial {s}");
            s += 1009;
        }
    }

    // ---- DATE construction incl. the phantom day (DATE.md §5) --------------

    #[test]
    fn date_1900_02_29_is_serial_60() {
        // The forward proof of the bug: DATE(1900,2,29) must be serial 60, even
        // though 1900 is not proleptically a leap year.
        assert_eq!(date_to_serial(1900, 2, 29, DateSystem::Excel1900), Ok(60));
        assert_eq!(date_to_serial(1900, 3, 1, DateSystem::Excel1900), Ok(61));
        assert_eq!(date_to_serial(1900, 1, 1, DateSystem::Excel1900), Ok(1));
    }

    #[test]
    fn date_month_and_day_overflow_normalization() {
        // DATE.md §3/§4 documented examples (all reduced to serials).
        // DATE(2020,13,1) = 2021-01-01
        assert_eq!(
            date_to_serial(2020, 13, 1, DateSystem::Excel1900),
            date_to_serial(2021, 1, 1, DateSystem::Excel1900)
        );
        // DATE(2020,14,1) = 2021-02-01
        assert_eq!(
            date_to_serial(2020, 14, 1, DateSystem::Excel1900),
            date_to_serial(2021, 2, 1, DateSystem::Excel1900)
        );
        // DATE(2020,0,1) = 2019-12-01
        assert_eq!(
            date_to_serial(2020, 0, 1, DateSystem::Excel1900),
            date_to_serial(2019, 12, 1, DateSystem::Excel1900)
        );
        // DATE(2020,1,32) = 2020-02-01
        assert_eq!(
            date_to_serial(2020, 1, 32, DateSystem::Excel1900),
            date_to_serial(2020, 2, 1, DateSystem::Excel1900)
        );
        // DATE(2020,1,0) = 2019-12-31 (day underflow)
        assert_eq!(
            date_to_serial(2020, 1, 0, DateSystem::Excel1900),
            date_to_serial(2019, 12, 31, DateSystem::Excel1900)
        );
    }

    #[test]
    fn date_out_of_range_is_error() {
        // Well past 9999-12-31.
        assert_eq!(
            date_to_serial(10000, 1, 1, DateSystem::Excel1900),
            Err(DateError::OutOfRange)
        );
        // Extreme negative rollback → before the epoch.
        assert_eq!(
            date_to_serial(2020, -100000, 1, DateSystem::Excel1900),
            Err(DateError::OutOfRange)
        );
        // Serial 0 result ("January 0, 1900") defers, not #NUM!.
        assert_eq!(
            date_to_serial(1900, 1, 0, DateSystem::Excel1900),
            Err(DateError::JanuaryZero)
        );
    }

    // ---- EOMONTH incl. Feb-1900 = 29 days (EOMONTH.md §6) ------------------

    #[test]
    fn eomonth_feb_1900_is_serial_60() {
        // EOMONTH(DATE(1900,1,1), 1) → last day of Feb 1900 → serial 60.
        assert_eq!(eomonth_serial(1, 1, DateSystem::Excel1900), Ok(60));
        // EOMONTH(DATE(1900,2,1), 0) → serial 60 too.
        let feb1 = date_to_serial(1900, 2, 1, DateSystem::Excel1900).unwrap();
        assert_eq!(eomonth_serial(feb1, 0, DateSystem::Excel1900), Ok(60));
    }

    #[test]
    fn eomonth_basic_forward_and_backward() {
        // EOMONTH(2020-01-15, 0) = 2020-01-31.
        let jan15 = date_to_serial(2020, 1, 15, DateSystem::Excel1900).unwrap();
        assert_eq!(
            eomonth_serial(jan15, 0, DateSystem::Excel1900),
            date_to_serial(2020, 1, 31, DateSystem::Excel1900)
        );
        // +1 → 2020-02-29 (2020 is a real leap year).
        assert_eq!(
            eomonth_serial(jan15, 1, DateSystem::Excel1900),
            date_to_serial(2020, 2, 29, DateSystem::Excel1900)
        );
        // -1 → 2019-12-31.
        assert_eq!(
            eomonth_serial(jan15, -1, DateSystem::Excel1900),
            date_to_serial(2019, 12, 31, DateSystem::Excel1900)
        );
    }

    #[test]
    fn eomonth_out_of_range_is_error() {
        let end = date_to_serial(9999, 12, 1, DateSystem::Excel1900).unwrap();
        assert_eq!(
            eomonth_serial(end, 1, DateSystem::Excel1900),
            Err(DateError::OutOfRange)
        );
    }
}
