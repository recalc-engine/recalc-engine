"""Crash/timeout robustness: a competitor engine that hangs or dies must have
every oracle-bearing cell counted as `declined` (never silently skipped), with
`no_oracle` cells preserved (NoOracle wins first) — the "no silent caps" rule."""
import os
import sys
import tempfile
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from funnel import Funnel  # noqa: E402
from harness import measure_workbook  # noqa: E402
from runner_common import ENGINE_CRASH, ENGINE_TIMEOUT, OK, run_engine  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
# mixed_small.xlsx: 10 formula cells, 1 of which is no_oracle (B9 has no cache).
MIXED = os.path.join(REPO, "excelbench", "tests", "fixtures", "mixed_small.xlsx")


class TestRunEngine(unittest.TestCase):
    def test_timeout_is_reported(self):
        # A command that outlives the timeout -> ENGINE_TIMEOUT, empty computed.
        outcome, computed, meta = run_engine(
            [sys.executable, "-c", "import time; time.sleep(30)"],
            MIXED, "/nonexistent.json", timeout_s=0.5,
        )
        self.assertEqual(outcome, ENGINE_TIMEOUT)
        self.assertEqual(computed, {})

    def test_nonzero_exit_is_crash(self):
        outcome, computed, meta = run_engine(
            [sys.executable, "-c", "import sys; sys.exit(3)"],
            MIXED, "/nonexistent.json", timeout_s=10,
        )
        self.assertEqual(outcome, ENGINE_CRASH)


class TestMeasureWorkbookDeclinesOnEngineFailure(unittest.TestCase):
    def test_crashed_engine_declines_every_oracle_cell(self):
        if not os.path.exists(MIXED):
            self.skipTest("mixed_small.xlsx fixture missing")
        f = Funnel(engine="recalc", scoring_mode="bit-exact")
        # Point the recalc adapter at a binary that always fails -> the runner
        # cannot produce values -> harness declines every oracle-bearing cell.
        measure_workbook("recalc", sys.executable, MIXED, 30.0, False,
                         ["--recalc-bin", "/bin/false"], f)
        self.assertEqual(f.engine_workbook_failures, 1)
        self.assertEqual(f.total_formula_cells, 10)
        # Every oracle-bearing formula cell is declined; the 1 no_oracle stays.
        self.assertEqual(f.no_oracle, 1)
        self.assertEqual(f.declined, 9)
        self.assertEqual(f.exact, 0)
        self.assertEqual(f.mismatch, 0)
        # Total is conserved: nothing silently dropped.
        self.assertEqual(f.exact + f.mismatch + f.declined + f.no_oracle + f.ulp_diff,
                         f.total_formula_cells)


if __name__ == "__main__":
    unittest.main()
