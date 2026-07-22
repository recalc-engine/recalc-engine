"""Cell-level classifier: a faithful port of `xl-bench`'s `diff.rs::classify`.

This is THE fidelity-of-comparison core. Every engine (recalc and each
competitor) is scored through this one function against the same oracle, so the
funnel classes are identical by construction. The only thing that differs
between engines is the *computed* value fed in.

Mapping to `diff.rs`:
  * `CellStatus::NoOracle`        -> "no_oracle"
  * `CellStatus::EngineUnsupported` (recalc sentinel) -> "declined"
        For a competitor, "declined" is the exact analog: the engine produced
        no comparable value (function unimplemented, per-cell exception, or the
        whole workbook crashed/timed out). It is excluded from the LENIENT
        denominator and counted as a failure in the STRICT one — identical to
        how recalc's own `#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!` cells are
        treated.
  * `CellStatus::Exact`           -> "exact"
  * `CellStatus::UlpDiff{..}`     -> "ulp_diff"  (only when ulp_tolerance > 0)
  * `CellStatus::Mismatch{..}`    -> "mismatch"

Classification ORDER is preserved exactly (diff.rs module docs):
  1. NoOracle wins unconditionally (no expected value -> unjudgeable).
  2. Engine "declined" sentinel next (declared gap, whatever the oracle says).
  3. Otherwise compare by type.

Number comparison: bit-exact by default (modulo +0/-0), OR — when `fuzzy_15sig`
is on — equal under Excel's 15-significant-figure rule (`numbers_fuzzy_equal`,
a line-for-line port of `xl_value::cmp_f64_fuzzy` at sig=15). Text is
case-SENSITIVE (diff.rs deliberately departs from Excel's `=` operator here).
Bool and error compare exactly.
"""
from __future__ import annotations

import math
import struct

from oracle import BLANK, BOOL, ERROR, NUMBER, TEXT, OVal

# Status tags — same spellings as CellStatus::tag().
EXACT = "exact"
ULP_DIFF = "ulp_diff"
MISMATCH = "mismatch"
DECLINED = "declined"  # the competitor analog of engine_unsupported
NO_ORACLE = "no_oracle"

# A sentinel computed value meaning "the engine produced nothing comparable"
# (its analog of a recalc sentinel error). Runners emit this for unimplemented
# functions / per-cell exceptions; the harness substitutes it for every cell of
# a crashed/timed-out workbook.
DECLINED_COMPUTED = OVal("__declined__", None)


def _bits(x: float) -> int:
    return struct.unpack("<q", struct.pack("<d", x))[0]


def numbers_bit_exact(a: float, b: float) -> bool:
    """Port of diff.rs::numbers_bit_exact: bit-identical OR IEEE ==-equal
    (the latter additionally identifies +0.0 and -0.0)."""
    return _bits(a) == _bits(b) or a == b


def _round_to_sig(x: float, sig: int = 15) -> float:
    """Port of xl_value::round_to_sig: scientific round-trip to `sig`
    significant decimal digits. `format!("{x:.14e}")` <-> f"{x:.14e}"."""
    if x == 0.0 or not math.isfinite(x):
        return x
    digits = sig - 1
    return float(f"{x:.{digits}e}")


def numbers_fuzzy_equal(a: float, b: float) -> bool:
    """Port of xl_value::cmp_f64_fuzzy == Equal at sig=15 (the ratified 15-sig
    float scoring tolerance, TOLERANCES.md / OXP-182)."""
    if a == b:
        return True
    if not math.isfinite(a) or not math.isfinite(b):
        return a == b  # exact order; a != b here so False
    diff = abs(a - b)
    scale = max(abs(a), abs(b))
    if diff > 1e-13 * scale:
        return False
    rx = _round_to_sig(a, 15)
    ry = _round_to_sig(b, 15)
    if not math.isfinite(rx) or not math.isfinite(ry):
        return a == b
    return rx == ry


def ulp_distance(a: float, b: float) -> int:
    """Port of diff.rs::ulp_distance (monotonic ULP-key remap across zero)."""

    def key(x: float) -> int:
        bits = _bits(x)
        if bits < 0:
            # i64::MIN.wrapping_sub(bits)
            return ((-(1 << 63)) - bits) & 0xFFFFFFFFFFFFFFFF
        return bits

    ka, kb = key(a), key(b)
    # interpret as unsigned then abs-diff
    return abs(ka - kb)


def classify(
    computed,  # OVal or DECLINED_COMPUTED
    oracle,  # OVal or None (None == NoOracle)
    *,
    fuzzy_15sig: bool = False,
    ulp_tolerance: int = 0,
) -> str:
    # 1. No oracle wins unconditionally.
    if oracle is None:
        return NO_ORACLE

    # 2. Engine declined (its declared-gap sentinel).
    if computed is DECLINED_COMPUTED or computed.kind == "__declined__":
        return DECLINED

    ck, ok = computed.kind, oracle.kind

    if ck == NUMBER and ok == NUMBER:
        a, b = computed.payload, oracle.payload
        if numbers_bit_exact(a, b) or (fuzzy_15sig and numbers_fuzzy_equal(a, b)):
            return EXACT
        ulps = ulp_distance(a, b)
        if ulps <= ulp_tolerance:
            return ULP_DIFF
        return MISMATCH

    if ck == TEXT and ok == TEXT:
        # Case-SENSITIVE by design (diff.rs module docs).
        return EXACT if computed.payload == oracle.payload else MISMATCH

    if ck == BOOL and ok == BOOL:
        return EXACT if computed.payload == oracle.payload else MISMATCH

    if ck == ERROR and ok == ERROR:
        # ErrorKind equality — canonical Excel error strings compare directly.
        return EXACT if computed.payload == oracle.payload else MISMATCH

    if ck == BLANK and ok == BLANK:
        # Unreachable via the oracle path (BLANK oracle -> NoOracle upstream),
        # kept for parity with diff.rs's (Blank, Blank) => Exact arm.
        return EXACT

    # Any other pairing (type mismatch) is a mismatch — never guess an
    # equivalence the spec has not pinned.
    return MISMATCH
