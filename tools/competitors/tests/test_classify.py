"""Classifier parity tests — mirror `xl-bench/src/diff.rs`'s unit tests so the
Python port is provably the same rule as the Rust one it stands in for."""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from classify import (  # noqa: E402
    DECLINED,
    DECLINED_COMPUTED,
    EXACT,
    MISMATCH,
    NO_ORACLE,
    ULP_DIFF,
    classify,
    numbers_bit_exact,
    numbers_fuzzy_equal,
    ulp_distance,
)
from oracle import OVal  # noqa: E402


def num(x):
    return OVal.number(x)


class TestClassify(unittest.TestCase):
    def test_no_oracle_wins_regardless_of_computed(self):
        self.assertEqual(classify(num(1.0), None), NO_ORACLE)
        self.assertEqual(classify(DECLINED_COMPUTED, None), NO_ORACLE)

    def test_declined_is_not_mismatch(self):
        st = classify(DECLINED_COMPUTED, num(1.0))
        self.assertEqual(st, DECLINED)

    def test_exact_number_match(self):
        self.assertEqual(classify(num(5.0), num(5.0)), EXACT)

    def test_signed_zero_is_exact(self):
        self.assertEqual(classify(num(0.0), num(-0.0)), EXACT)

    def test_number_mismatch_at_zero_tolerance(self):
        self.assertEqual(classify(num(5.0), num(5.0000001)), MISMATCH)

    def test_ulp_diff_within_configured_tolerance(self):
        import struct
        a = 1.0
        b = struct.unpack("<d", struct.pack("<q", struct.unpack("<q", struct.pack("<d", a))[0] + 2))[0]
        self.assertEqual(classify(num(a), num(b), ulp_tolerance=2), ULP_DIFF)

    def test_ulp_diff_outside_tolerance_is_mismatch(self):
        import struct
        a = 1.0
        b = struct.unpack("<d", struct.pack("<q", struct.unpack("<q", struct.pack("<d", a))[0] + 5))[0]
        self.assertEqual(classify(num(a), num(b), ulp_tolerance=2), MISMATCH)

    def test_fuzzy_15sig_flips_ieee_noise_to_exact_but_keeps_divergence(self):
        recalc = 49.300000000000004
        excel_cache = 49.3
        self.assertNotEqual(recalc, excel_cache)  # different doubles
        # strict: sub-storage IEEE noise scores as mismatch
        self.assertEqual(classify(num(recalc), num(excel_cache)), MISMATCH)
        # fuzzy: agreement at Excel's storage precision -> exact
        self.assertEqual(classify(num(recalc), num(excel_cache), fuzzy_15sig=True), EXACT)
        # genuine divergences stay mismatch under fuzzy
        self.assertEqual(classify(num(5.0), num(5.1), fuzzy_15sig=True), MISMATCH)
        self.assertEqual(classify(num(1.0), num(1.0 + 1e-9), fuzzy_15sig=True), MISMATCH)

    def test_text_compare_is_case_sensitive(self):
        self.assertEqual(classify(OVal.text("Total"), OVal.text("TOTAL")), MISMATCH)
        self.assertEqual(classify(OVal.text("Total"), OVal.text("Total")), EXACT)

    def test_bool_exact_and_mismatch(self):
        self.assertEqual(classify(OVal.boolean(True), OVal.boolean(True)), EXACT)
        self.assertEqual(classify(OVal.boolean(True), OVal.boolean(False)), MISMATCH)

    def test_error_kind_mismatch_is_mismatch(self):
        self.assertEqual(classify(OVal.error("#VALUE!"), OVal.error("#DIV/0!")), MISMATCH)

    def test_error_kind_match_is_exact(self):
        self.assertEqual(classify(OVal.error("#DIV/0!"), OVal.error("#DIV/0!")), EXACT)

    def test_type_mismatch_is_mismatch(self):
        self.assertEqual(classify(num(1.0), OVal.text("1")), MISMATCH)

    def test_numbers_helpers(self):
        self.assertTrue(numbers_bit_exact(0.0, -0.0))
        self.assertTrue(numbers_bit_exact(5.0, 5.0))
        self.assertFalse(numbers_bit_exact(5.0, 5.0000001))
        self.assertTrue(numbers_fuzzy_equal(0.1 + 0.2, 0.3))
        self.assertFalse(numbers_fuzzy_equal(1.0, 1.0 + 1e-9))
        self.assertEqual(ulp_distance(0.0, -0.0), 0)


if __name__ == "__main__":
    unittest.main()
