"""Funnel aggregation, emitting the SAME summary shape as `xl-bench`'s
`verify-dir` corpus total.

The corpus-level fidelity definitions are copied verbatim from
`bin/recalc.rs::cmd_verify_dir` and `report.rs::WorkbookSummary`, applied to
the folded corpus totals, with the competitor "declined" bucket standing in for
recalc's "engine_unsupported":

    strict  = (exact + ulp) / (total - no_oracle)
    lenient = (exact + ulp) / (total - declined - no_oracle)

`load_failures` are counted separately and NEVER enter the cell funnel — exactly
as `verify-dir` excludes an `xl-io` LoadFailure entry from every per-cell count
(the file is measured to have contributed nothing, not measured as wrong).
"""
from __future__ import annotations

import json
from dataclasses import dataclass, field

from classify import DECLINED, EXACT, MISMATCH, NO_ORACLE, ULP_DIFF


@dataclass
class Funnel:
    engine: str
    scoring_mode: str  # "bit-exact" or "15-sig-fig"
    total_formula_cells: int = 0
    exact: int = 0
    ulp_diff: int = 0
    mismatch: int = 0
    declined: int = 0  # competitor analog of engine_unsupported
    no_oracle: int = 0
    load_failures: int = 0  # workbooks the ORACLE could not parse (excluded)
    engine_workbook_failures: int = 0  # books that loaded but the engine failed
    workbooks_scored: int = 0
    per_workbook: list = field(default_factory=list)

    def add_status(self, status: str) -> None:
        self.total_formula_cells += 1
        if status == EXACT:
            self.exact += 1
        elif status == ULP_DIFF:
            self.ulp_diff += 1
        elif status == MISMATCH:
            self.mismatch += 1
        elif status == DECLINED:
            self.declined += 1
        elif status == NO_ORACLE:
            self.no_oracle += 1
        else:
            raise ValueError(f"unknown status {status!r}")

    # --- fidelity math (verbatim from verify-dir) ---------------------------

    @property
    def strict_denom(self) -> int:
        return max(self.total_formula_cells - self.no_oracle, 0)

    @property
    def lenient_denom(self) -> int:
        return max(self.strict_denom - self.declined, 0)

    @property
    def matched(self) -> int:
        return self.exact + self.ulp_diff

    def strict_pct(self):
        d = self.strict_denom
        return None if d == 0 else 100.0 * self.matched / d

    def lenient_pct(self):
        d = self.lenient_denom
        return None if d == 0 else 100.0 * self.matched / d

    # --- rendering ----------------------------------------------------------

    def summary_text(self) -> str:
        def pct(p):
            return "n/a" if p is None else f"{p:.3f}%"

        return (
            f"[ENGINE] {self.engine}   [SCORING MODE] {self.scoring_mode}\n"
            f"CORPUS TOTAL: {self.total_formula_cells} formula cells — "
            f"exact {self.exact}, ulp {self.ulp_diff}, mismatch {self.mismatch}, "
            f"declined {self.declined}, no_oracle {self.no_oracle}\n"
            f"CORPUS FIDELITY: strict {pct(self.strict_pct())} "
            f"(={self.matched}/{self.strict_denom}) / "
            f"lenient {pct(self.lenient_pct())} "
            f"(={self.matched}/{self.lenient_denom})\n"
            f"{self.workbooks_scored} workbook(s) scored, "
            f"{self.load_failures} oracle load failure(s), "
            f"{self.engine_workbook_failures} engine workbook failure(s)"
        )

    def to_dict(self) -> dict:
        return {
            "engine": self.engine,
            "scoring_mode": self.scoring_mode,
            "total_formula_cells": self.total_formula_cells,
            "exact": self.exact,
            "ulp_diff": self.ulp_diff,
            "mismatch": self.mismatch,
            "declined": self.declined,
            "no_oracle": self.no_oracle,
            "matched": self.matched,
            "strict_denom": self.strict_denom,
            "lenient_denom": self.lenient_denom,
            "strict_fidelity_pct": self.strict_pct(),
            "lenient_fidelity_pct": self.lenient_pct(),
            "load_failures": self.load_failures,
            "engine_workbook_failures": self.engine_workbook_failures,
            "workbooks_scored": self.workbooks_scored,
            "per_workbook": self.per_workbook,
        }

    def to_json(self) -> str:
        return json.dumps(self.to_dict(), indent=2)
