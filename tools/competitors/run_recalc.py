#!/usr/bin/env python3
"""recalc adapter runner — the apples-to-apples VALIDATION engine.

Runs recalc's own `verify --json` on a workbook and reconstructs a per-cell
computed value from the resulting report, so recalc can be measured through the
*same* harness as every competitor. If this harness (its independent oracle
extraction + classify port) reproduces recalc's `verify-dir` funnel counts on a
sample, the harness is proven to score competitors on identical footing.

Reconstruction of recalc's computed value from a per-cell status:
  * exact              -> recalc computed == oracle; emit the oracle value
                          (from this harness's own extraction).
  * ulp_diff           -> (never occurs at ulp_tolerance 0) emit oracle value.
  * mismatch           -> emit the report's `actual` (recalc's computed value).
  * engine_unsupported -> declined (recalc's #UNSUPPORTED!/#BLOCKED!/#RESOURCE!).
  * no_oracle          -> declined placeholder (harness sees oracle None anyway).

recalc `verify` uses DiffConfig::default() == bit-exact, so validate this
adapter against the harness in bit-exact mode (the default).
"""
from __future__ import annotations

import argparse
import json
import subprocess
import tempfile
import time

from oracle import BLANK, BOOL, ERROR, NUMBER, TEXT, extract_oracle
from runner_common import CellSink

ENGINE = "recalc"


def oracle_map(workbook: str) -> dict:
    m = {}
    for fc in extract_oracle(workbook):
        if fc.oracle is not None:
            m[(fc.sheet, fc.row, fc.col)] = fc.oracle
    return m


def value_from_report(obj: dict):
    """recalc value_to_json -> (kind, payload) or None for unscorable."""
    t = obj.get("type")
    if t == "number":
        return (NUMBER, float(obj["value"]))
    if t == "text":
        return (TEXT, obj["value"])
    if t == "bool":
        return (BOOL, bool(obj["value"]))
    if t == "error":
        return (ERROR, obj["value"])
    if t == "blank":
        return (BLANK, None)
    return None  # array/ref/lambda -> not scorable here


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("workbook")
    ap.add_argument("out")
    ap.add_argument("--recalc-bin", default="recalc")
    args = ap.parse_args()

    sink = CellSink(ENGINE, args.workbook)
    t0 = time.time()

    with tempfile.NamedTemporaryFile(suffix=".json", delete=False) as tf:
        report_path = tf.name
    try:
        proc = subprocess.run(
            [args.recalc_bin, "verify", args.workbook, "--json", report_path, "--quiet"],
            capture_output=True,
            text=True,
        )
        # exit 0 = clean, 1 = has mismatch (NORMAL), 2 = load failure.
        if proc.returncode == 2:
            sink.dump_load_failure(args.out, f"recalc load failure: {proc.stderr.strip()[-300:]}",
                                   time.time() - t0)
            return 0
        with open(report_path) as f:
            report = json.load(f)
    finally:
        import os
        try:
            os.unlink(report_path)
        except OSError:
            pass

    omap = oracle_map(args.workbook)

    def emit(sheet, row, col, kv):
        """kv is (kind, payload) or None -> declined."""
        if kv is None:
            sink.declined(sheet, row, col)
            return
        k, v = kv
        if k == NUMBER:
            sink.number(sheet, row, col, v)
        elif k == TEXT:
            sink.text(sheet, row, col, v)
        elif k == BOOL:
            sink.boolean(sheet, row, col, v)
        elif k == ERROR:
            sink.error(sheet, row, col, v)
        else:
            sink.blank(sheet, row, col)

    for cell in report.get("cells", []):
        sheet = cell["sheet"]
        row, col = cell["row"], cell["col"]
        st = cell["status"]["kind"]
        if st in ("exact", "ulp_diff"):
            ov = omap.get((sheet, row, col))
            emit(sheet, row, col, None if ov is None else (ov.kind, ov.payload))
        elif st == "mismatch":
            emit(sheet, row, col, value_from_report(cell["status"]["actual"]))
        else:  # engine_unsupported or no_oracle
            sink.declined(sheet, row, col)

    sink.dump_ok(args.out, time.time() - t0)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
