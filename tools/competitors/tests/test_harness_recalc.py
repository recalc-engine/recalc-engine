"""End-to-end apples-to-apples proof: the harness, driving recalc through its
own `verify --json`, must reproduce recalc `verify`'s funnel counts exactly.

If this passes, the harness's independent oracle extraction + classify port +
funnel math agree with `xl-bench`'s Rust path cell-for-cell on the fixtures —
which is exactly the fidelity-of-comparison guarantee the competitor numbers
rest on. Skips cleanly if the recalc binary is not built."""
import os
import shutil
import subprocess
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from funnel import Funnel  # noqa: E402
from harness import measure_workbook  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
FIX = os.path.join(REPO, "xl-bench", "tests", "fixtures")


def _recalc_bin():
    for cand in [
        os.path.join(REPO, "target", "debug", "recalc"),
        os.path.join(REPO, "target", "release", "recalc"),
        shutil.which("recalc"),
    ]:
        if cand and os.path.exists(cand):
            return cand
    return None


class TestHarnessReproducesRecalc(unittest.TestCase):
    def setUp(self):
        self.recalc = _recalc_bin()
        if not self.recalc:
            self.skipTest("recalc binary not built (cargo build -p xl-bench)")

    def _measure(self, wb, fuzzy=False):
        f = Funnel(engine="recalc", scoring_mode="15-sig-fig" if fuzzy else "bit-exact")
        measure_workbook("recalc", sys.executable, wb, 120.0, fuzzy,
                         ["--recalc-bin", self.recalc], f)
        return f

    def test_poisoned_fixture_funnel_matches_verify(self):
        # cached_fixture.rs asserts: total 4, exact 1, mismatch 1,
        # engine_unsupported 1, no_oracle 1.
        f = self._measure(os.path.join(FIX, "cached_values.xlsx"))
        self.assertEqual(f.total_formula_cells, 4)
        self.assertEqual(f.exact, 1)
        self.assertEqual(f.mismatch, 1)
        self.assertEqual(f.declined, 1)  # engine_unsupported analog
        self.assertEqual(f.no_oracle, 1)
        # strict = (exact+ulp)/(total-no_oracle) = 1/3
        self.assertAlmostEqual(f.strict_pct(), 100.0 / 3.0, places=6)
        # lenient = 1/(4-1-1) = 50%
        self.assertAlmostEqual(f.lenient_pct(), 50.0, places=6)

    def test_clean_fixture_all_exact(self):
        f = self._measure(os.path.join(FIX, "clean_values.xlsx"))
        self.assertEqual(f.total_formula_cells, 1)
        self.assertEqual(f.exact, 1)
        self.assertEqual(f.mismatch, 0)
        self.assertEqual(f.strict_pct(), 100.0)


if __name__ == "__main__":
    unittest.main()
