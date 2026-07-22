"""Oracle-extraction tests against the vendored xl-bench fixtures, cross-checked
against the exact funnel `xl-bench`'s own integration test asserts
(cached_fixture.rs): 4 formula cells -> exact 1, mismatch 1, unsupported 1,
no_oracle 1."""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from oracle import (  # noqa: E402
    BLANK,
    NUMBER,
    a1_ref,
    col_to_letters,
    extract_oracle,
    parse_a1,
)

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
FIX = os.path.join(REPO, "xl-bench", "tests", "fixtures")


class TestOracle(unittest.TestCase):
    def test_a1_roundtrip(self):
        for col in [0, 1, 25, 26, 27, 51, 700, 701, 702, 16383]:
            label = f"{col_to_letters(col)}1"
            self.assertEqual(parse_a1(label), (0, col))
        self.assertEqual(parse_a1("B7"), (6, 1))
        self.assertEqual(a1_ref(6, 1), "B7")
        self.assertEqual(col_to_letters(16383), "XFD")

    def test_cached_values_fixture(self):
        cells = extract_oracle(os.path.join(FIX, "cached_values.xlsx"))
        # A3,A4,A5,A6 are formula cells (A1,A2 are literals -> excluded).
        self.assertEqual(len(cells), 4)
        by_ref = {a1_ref(c.row, c.col): c for c in cells}
        self.assertEqual(set(by_ref), {"A3", "A4", "A5", "A6"})
        # A3 cached 5, A4 cached 999, A5 cached 42 -> all numbers with oracle.
        self.assertEqual(by_ref["A3"].oracle.kind, NUMBER)
        self.assertEqual(by_ref["A3"].oracle.payload, 5.0)
        self.assertEqual(by_ref["A4"].oracle.payload, 999.0)
        self.assertEqual(by_ref["A5"].oracle.payload, 42.0)
        # A6 has an <f> but no <v> -> NoOracle (sidecar rule).
        self.assertIsNone(by_ref["A6"].oracle)

    def test_clean_values_fixture(self):
        cells = extract_oracle(os.path.join(FIX, "clean_values.xlsx"))
        self.assertEqual(len(cells), 1)
        self.assertEqual(cells[0].oracle.kind, NUMBER)
        self.assertEqual(cells[0].oracle.payload, 5.0)


if __name__ == "__main__":
    unittest.main()
