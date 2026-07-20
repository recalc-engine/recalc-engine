//! Cell-level diff core: computed value vs. oracle value → [`CellStatus`].
//!
//! This module is pure value logic — no I/O, no `xl-io`/`xl-engine` types —
//! so its rules are unit-testable by constructing [`xl_value::Value`]s
//! directly (see the tests below and the plan's Task 10 test list).
//!
//! # Classification order (read this before changing it)
//! 1. **No oracle → [`CellStatus::NoOracle`], unconditionally.** If the
//!    oracle source has nothing for this cell (e.g. a `gridgen` probe
//!    workbook with no cached `<v>`), the harness cannot judge correctness
//!    at all, regardless of what the engine computed — reporting anything
//!    else here would be exactly the "fake pass" `implementation-plan.md`
//!    §0 forbids.
//! 2. **Engine sentinel error → [`CellStatus::EngineUnsupported`].** If
//!    Recalc's own computed value is `#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`
//!    ([`xl_value::ErrorKind::is_recalc_sentinel`]), that is Recalc explicitly
//!    refusing to guess, not a value to score against the oracle. This is
//!    checked whether or not step 1 already fired (step 1 already returned),
//!    and regardless of what the oracle says.
//! 3. **Otherwise, compare computed vs. oracle** by type: numbers get the
//!    bit-exact/ULP treatment below; every other type (text, bool, error)
//!    is exact-or-mismatch, no tolerance.
//!
//! # Text comparison is case-sensitive here
//! `xl_value::compare`/`values_equal` fold text case (Excel's `=` operator
//! semantics), but that is the wrong rule for this harness: Excel's own
//! cached `<v>` is the *literal* string Excel wrote, and Recalc's computed
//! text must match that string byte-for-byte to claim fidelity — a formula
//! that returns `"Total"` when Excel cached `"TOTAL"` is a real
//! (capitalization) bug, not an Excel-equivalent value, even though Excel's
//! own `=` comparison operator would call them equal. So this module compares
//! `Value::Text` payloads with a direct `str` comparison, not
//! `xl_value::text_eq_ci`.
//!
//! # ULP tolerance is human-gated and defaults to strict
//! [`DiffConfig::ulp_tolerance`] defaults to `0` (bit-exact, modulo the
//! `+0.0`/`-0.0` identification IEEE-754 `==` already makes — see this
//! module's private `numbers_bit_exact` helper). Per `TOLERANCES.md`,
//! tolerances are a human
//! decision recorded per function with a rationale; nothing in this crate
//! ever raises the default on its own. Until `TOLERANCES.md` grows real
//! entries, any numeric mismatch surfaces as [`CellStatus::Mismatch`], never
//! a silently-passed [`CellStatus::UlpDiff`].

use xl_value::Value;

/// Per-cell diff configuration. `Default` is the strict, human-gated
/// baseline (`ulp_tolerance: 0`) — see the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DiffConfig {
    /// Maximum ULP (unit-in-the-last-place) distance between computed and
    /// oracle numbers still counted as [`CellStatus::UlpDiff`] rather than
    /// [`CellStatus::Mismatch`]. `0` means bit-exact (modulo `+0.0`/`-0.0`).
    pub ulp_tolerance: u64,
    /// The **ratified 15-significant-figure float scoring tolerance** — see
    /// `TOLERANCES.md` → "Global numeric comparison (15 significant figures)"
    /// (the contract review tech-lead checkpoint 2026-07-13, superseding RFC-0009 §7 clause 6).
    /// When `true`, two numbers equal under Excel's 15-sig-fig comparison
    /// (OXP-182 / [`xl_value::compare_fuzzy`], via [`numbers_fuzzy_equal`]) count
    /// as [`CellStatus::Exact`]. Excel *stores* cached `<v>` at 15 sig figs, so
    /// bit-exact comparison scored Excel's own output-rounding artifact
    /// (`49.300000000000004` vs `49.3`) as a mismatch — noise below the oracle's
    /// storage resolution, not real disagreement. This is the **documented float
    /// tolerance and the basis for the published / M1-gate headline** number.
    ///
    /// Kept **default `false`** deliberately: the zero-config baseline stays
    /// bit-exact as a regression/drift diagnostic, and the strict number is the
    /// conservative floor disclosed alongside the 15-sig headline (dual
    /// disclosure is mandatory at publish time — a separate checkpoint).
    pub fuzzy_15sig: bool,
}

/// The classification of one formula cell's computed value against the
/// oracle. See the module docs for the classification order.
#[derive(Clone, Debug, PartialEq)]
pub enum CellStatus {
    /// Computed value and oracle value agree exactly (case-sensitive text,
    /// bit-exact-or-IEEE-equal numbers, equal bool/error kind).
    Exact,
    /// Both computed and oracle are numbers, not bit-exact, but within
    /// [`DiffConfig::ulp_tolerance`] ULPs of each other.
    UlpDiff {
        /// The measured ULP distance (always `<= ulp_tolerance` for this
        /// variant to have been chosen).
        ulps: u64,
    },
    /// Computed and oracle disagree outside any tolerance: a genuine
    /// fidelity bug candidate. Covers value mismatches, type mismatches, and
    /// an error-kind mismatch between two *real* Excel errors (e.g. computed
    /// `#VALUE!` vs. cached `#DIV/0!` — this is not the explicit-gap bucket,
    /// see [`CellStatus::EngineUnsupported`]).
    Mismatch {
        /// The oracle's recorded value.
        expected: Value,
        /// Recalc's computed value.
        actual: Value,
    },
    /// Recalc's computed value is one of its own sentinel errors
    /// (`#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`,
    /// [`xl_value::ErrorKind::is_recalc_sentinel`]) — the explicit-gap bucket.
    /// Never counted as a failure in [`crate::report::WorkbookSummary`]'s
    /// lenient fidelity percentage; counted as a failure in the strict one.
    EngineUnsupported,
    /// The oracle has no recorded value for this cell at all (see
    /// [`crate::sidecar::OracleRecord::NoOracle`]) — not scored either way.
    NoOracle,
}

impl CellStatus {
    /// A short, stable machine-readable tag (used by the JSON/HTML reports
    /// and by `--quiet` summaries).
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            CellStatus::Exact => "exact",
            CellStatus::UlpDiff { .. } => "ulp_diff",
            CellStatus::Mismatch { .. } => "mismatch",
            CellStatus::EngineUnsupported => "engine_unsupported",
            CellStatus::NoOracle => "no_oracle",
        }
    }

    /// `true` for [`CellStatus::Mismatch`] — the only status that makes a
    /// workbook run "WRONG" for the CLI's exit-code contract (see
    /// `xl-bench/src/bin/recalc.rs`).
    #[must_use]
    pub fn is_mismatch(&self) -> bool {
        matches!(self, CellStatus::Mismatch { .. })
    }
}

/// `true` if `a` and `b` should be treated as the same IEEE double for
/// "exact" purposes: bit-identical, OR IEEE `==`-equal (which additionally
/// identifies `+0.0` and `-0.0` — Excel does not distinguish signed zero in
/// any cell display, and neither should this harness's "exact" bucket).
#[must_use]
fn numbers_bit_exact(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || a == b
}

/// `true` if `a` and `b` are equal under Excel's 15-significant-figure
/// comparison (algorithm A: round both to 15 sig figs, then compare — OXP-182,
/// via [`xl_value::compare_fuzzy`]). This is the ratified float scoring
/// tolerance (see [`DiffConfig::fuzzy_15sig`] and `TOLERANCES.md`): Excel's
/// cached `<v>` carries only 15 significant figures, so this compares recalc's
/// full-precision result against Excel's value **at Excel's own storage
/// resolution**. Reuses the engine's `compare_fuzzy` as the (independently
/// justified, coincidentally identical) round-to-15-sig algorithm — NOT as an
/// appeal to operator `=` semantics (RFC-0009 §7 clause 6 reconciliation).
#[must_use]
fn numbers_fuzzy_equal(a: f64, b: f64) -> bool {
    matches!(
        xl_value::compare_fuzzy(&Value::number(a), &Value::number(b)),
        Ok(std::cmp::Ordering::Equal)
    )
}

/// Standard monotonic "ULP key" mapping: reinterprets an `f64`'s bits as a
/// signed integer, then remaps the negative range so the resulting key is
/// monotonically increasing in real-number order across the zero crossing
/// (twos-complement bit patterns are *not* naturally ordered that way for
/// negative floats). `f64` values here are always finite — Recalc's
/// [`Value::Number`] invariant forbids NaN/Inf — so there is no NaN case to
/// special-case.
#[must_use]
fn ulp_key(x: f64) -> i64 {
    let bits = x.to_bits() as i64;
    if bits < 0 {
        i64::MIN.wrapping_sub(bits)
    } else {
        bits
    }
}

/// The ULP distance between two finite `f64`s (0 for bit-identical values,
/// and — via the private `ulp_key` remapping above — also 0 for `+0.0`
/// vs. `-0.0`).
#[must_use]
pub fn ulp_distance(a: f64, b: f64) -> u64 {
    ulp_key(a).abs_diff(ulp_key(b))
}

/// Classifies one formula cell's computed value against its oracle record.
/// See the module docs for the full rule.
#[must_use]
pub fn classify(
    computed: &Value,
    oracle: &crate::sidecar::OracleRecord,
    cfg: DiffConfig,
) -> CellStatus {
    use crate::sidecar::OracleRecord;

    let expected = match oracle {
        OracleRecord::NoOracle => return CellStatus::NoOracle,
        OracleRecord::Value(v) => v,
    };

    if let Value::Error(kind) = computed
        && kind.is_recalc_sentinel()
    {
        return CellStatus::EngineUnsupported;
    }

    match (computed, expected) {
        (Value::Number(a), Value::Number(b)) => {
            if numbers_bit_exact(*a, *b) || (cfg.fuzzy_15sig && numbers_fuzzy_equal(*a, *b)) {
                CellStatus::Exact
            } else {
                let ulps = ulp_distance(*a, *b);
                if ulps <= cfg.ulp_tolerance {
                    CellStatus::UlpDiff { ulps }
                } else {
                    mismatch(expected, computed)
                }
            }
        }
        (Value::Text(a), Value::Text(b)) => {
            // Case-sensitive by design — see module docs.
            if a.as_str() == b.as_str() {
                CellStatus::Exact
            } else {
                mismatch(expected, computed)
            }
        }
        (Value::Bool(a), Value::Bool(b)) => {
            if a == b {
                CellStatus::Exact
            } else {
                mismatch(expected, computed)
            }
        }
        (Value::Error(a), Value::Error(b)) => {
            // `computed`'s sentinel case already returned above; both kinds
            // here are genuine Excel errors (or the oracle recorded one of
            // Recalc's own sentinels somehow, which real Excel never
            // writes — still a mismatch, not a free pass).
            if a == b {
                CellStatus::Exact
            } else {
                mismatch(expected, computed)
            }
        }
        (Value::Blank, Value::Blank) => CellStatus::Exact,
        // BC-6 (RFC-0012): a lambda is engine-internal and never a scorable
        // Excel value (a bare lambda cell is `#CALC!` in Excel). Classify it as
        // an engine-unsupported result, NOT a silent `Mismatch` — a `Mismatch`
        // would both hide the new variant and pollute the mismatch bucket with
        // a false fidelity failure (`Lambda == Lambda` under-claimed). Oracle-
        // side lambdas are filtered to `NoOracle` upstream in `sidecar.rs`, so
        // in practice only `computed` can be a lambda here; both sides are
        // covered for robustness.
        (Value::Lambda(_), _) | (_, Value::Lambda(_)) => CellStatus::EngineUnsupported,
        // Any other pairing (type mismatch, or Array/Ref — out of v0 scope for
        // scalar formula-cell diffing) is a mismatch: never guess an
        // equivalence the plan hasn't specified.
        //
        // NB: this is a genuine catch-all — it silently ABSORBS any future
        // `Value` variant as a `Mismatch`; the named bindings do NOT make it a
        // BC-6 compile-forcing guard (a pair catch-all matches everything, new
        // variant or not). BC-6 exhaustiveness is enforced where it can be — the
        // scalar matches in `xl-value` (`witness_every_value_variant` +
        // `tools/check-no-value-wildcard.py`). If a new variant needs distinct
        // diff handling, add an explicit arm ABOVE this line; nothing here will
        // remind you to.
        (actual, expect) => mismatch(expect, actual),
    }
}

fn mismatch(expected: &Value, actual: &Value) -> CellStatus {
    CellStatus::Mismatch {
        expected: expected.clone(),
        actual: actual.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecar::OracleRecord;
    use xl_value::ErrorKind;

    fn num(x: f64) -> Value {
        Value::number(x)
    }

    #[test]
    fn no_oracle_wins_regardless_of_computed() {
        assert_eq!(
            classify(&num(1.0), &OracleRecord::NoOracle, DiffConfig::default()),
            CellStatus::NoOracle
        );
        assert_eq!(
            classify(
                &Value::Error(ErrorKind::Unsupported),
                &OracleRecord::NoOracle,
                DiffConfig::default()
            ),
            CellStatus::NoOracle
        );
    }

    #[test]
    fn computed_lambda_is_engine_unsupported_not_mismatch() {
        // BC-6 (RFC-0012): a computed lambda reaching `classify` is engine-
        // internal and never scorable — `EngineUnsupported`, NOT a silent
        // `Mismatch` (which would pollute the fidelity mismatch bucket) and
        // NEVER `Exact`.
        let status = classify(
            &Value::test_lambda(),
            &OracleRecord::Value(num(1.0)),
            DiffConfig::default(),
        );
        assert_eq!(status, CellStatus::EngineUnsupported);
        assert!(!status.is_mismatch());
        assert_ne!(status, CellStatus::Exact);
    }

    #[test]
    fn engine_sentinel_is_unsupported_even_with_oracle_present() {
        for kind in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            let status = classify(
                &Value::Error(kind),
                &OracleRecord::Value(num(42.0)),
                DiffConfig::default(),
            );
            assert_eq!(status, CellStatus::EngineUnsupported);
        }
    }

    #[test]
    fn exact_number_match() {
        let status = classify(
            &num(5.0),
            &OracleRecord::Value(num(5.0)),
            DiffConfig::default(),
        );
        assert_eq!(status, CellStatus::Exact);
    }

    #[test]
    fn signed_zero_is_exact() {
        let status = classify(
            &num(0.0),
            &OracleRecord::Value(num(-0.0)),
            DiffConfig::default(),
        );
        assert_eq!(status, CellStatus::Exact);
    }

    #[test]
    fn number_mismatch_at_zero_tolerance() {
        let status = classify(
            &num(5.0),
            &OracleRecord::Value(num(5.0000001)),
            DiffConfig::default(),
        );
        assert!(matches!(status, CellStatus::Mismatch { .. }));
    }

    #[test]
    fn ulp_diff_within_configured_tolerance() {
        let a = 1.0_f64;
        let b = f64::from_bits(a.to_bits() + 2);
        let cfg = DiffConfig {
            ulp_tolerance: 2,
            ..Default::default()
        };
        let status = classify(&num(a), &OracleRecord::Value(num(b)), cfg);
        assert_eq!(status, CellStatus::UlpDiff { ulps: 2 });
    }

    #[test]
    fn ulp_diff_outside_tolerance_is_mismatch() {
        let a = 1.0_f64;
        let b = f64::from_bits(a.to_bits() + 5);
        let cfg = DiffConfig {
            ulp_tolerance: 2,
            ..Default::default()
        };
        let status = classify(&num(a), &OracleRecord::Value(num(b)), cfg);
        assert!(matches!(status, CellStatus::Mismatch { .. }));
    }

    #[test]
    fn fuzzy_15sig_flips_ieee_noise_to_exact_but_keeps_real_divergence() {
        // TOLERANCES.md "Global numeric comparison (15 sig figs)": Excel stores
        // cached <v> at 15 sig figs, so recalc's 49.300000000000004 vs Excel's
        // 49.3 (different doubles, identical to 15 sig figs) is real agreement at
        // Excel's storage precision — not a bug.
        let recalc = 49.300000000000004_f64;
        let excel_cache = 49.3_f64; // nearest double to the 15-sig cache "49.3"
        assert_ne!(
            recalc.to_bits(),
            excel_cache.to_bits(),
            "test premise: must be different doubles"
        );
        // Strict (default) scores the sub-storage-resolution IEEE noise as a Mismatch.
        assert!(matches!(
            classify(
                &num(recalc),
                &OracleRecord::Value(num(excel_cache)),
                DiffConfig::default()
            ),
            CellStatus::Mismatch { .. }
        ));
        // 15-sig-fig tolerance: agreement at Excel's storage precision -> Exact.
        let fuzzy = DiffConfig {
            fuzzy_15sig: true,
            ..Default::default()
        };
        assert_eq!(
            classify(&num(recalc), &OracleRecord::Value(num(excel_cache)), fuzzy),
            CellStatus::Exact
        );
        // SAFEGUARD — genuine divergences (>> 15 sig figs) STAY Mismatch under
        // fuzzy: no false pass. A gross error (5.0 vs 5.1) and a ~1e-9 error.
        assert!(matches!(
            classify(&num(5.0), &OracleRecord::Value(num(5.1)), fuzzy),
            CellStatus::Mismatch { .. }
        ));
        assert!(matches!(
            classify(&num(1.0), &OracleRecord::Value(num(1.0 + 1e-9)), fuzzy),
            CellStatus::Mismatch { .. }
        ));
    }

    #[test]
    fn text_compare_is_case_sensitive() {
        let status = classify(
            &Value::text("Total"),
            &OracleRecord::Value(Value::text("TOTAL")),
            DiffConfig::default(),
        );
        assert!(matches!(status, CellStatus::Mismatch { .. }));

        let status = classify(
            &Value::text("Total"),
            &OracleRecord::Value(Value::text("Total")),
            DiffConfig::default(),
        );
        assert_eq!(status, CellStatus::Exact);
    }

    #[test]
    fn bool_exact_and_mismatch() {
        assert_eq!(
            classify(
                &Value::bool(true),
                &OracleRecord::Value(Value::bool(true)),
                DiffConfig::default()
            ),
            CellStatus::Exact
        );
        assert!(matches!(
            classify(
                &Value::bool(true),
                &OracleRecord::Value(Value::bool(false)),
                DiffConfig::default()
            ),
            CellStatus::Mismatch { .. }
        ));
    }

    #[test]
    fn error_kind_mismatch_is_mismatch_not_unsupported() {
        let status = classify(
            &Value::Error(ErrorKind::Value),
            &OracleRecord::Value(Value::Error(ErrorKind::Div0)),
            DiffConfig::default(),
        );
        assert!(matches!(status, CellStatus::Mismatch { .. }));
    }

    #[test]
    fn error_kind_match_is_exact() {
        let status = classify(
            &Value::Error(ErrorKind::Div0),
            &OracleRecord::Value(Value::Error(ErrorKind::Div0)),
            DiffConfig::default(),
        );
        assert_eq!(status, CellStatus::Exact);
    }

    #[test]
    fn type_mismatch_is_mismatch() {
        let status = classify(
            &num(1.0),
            &OracleRecord::Value(Value::text("1")),
            DiffConfig::default(),
        );
        assert!(matches!(status, CellStatus::Mismatch { .. }));
    }
}
