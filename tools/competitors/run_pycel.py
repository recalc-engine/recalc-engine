#!/usr/bin/env python3
"""pycel runner — measures the `pycel` pure-Python engine on one workbook.

Invoked by harness.py in an isolated subprocess:  python run_pycel.py <wb> <out>

It reports raw computed values only (no oracle, no classification). The set of
cells it evaluates is exactly this harness's formula-cell universe (every
`<f>`-bearing cell), taken from the stdlib oracle extractor, so recalc and pycel
are scored over the identical cell set.

Per-cell mapping to the runner value contract:
  * numeric result            -> number
  * Python bool               -> bool  (checked before int/float!)
  * a recognised Excel error  -> error (e.g. "#DIV/0!")
  * a non-Excel error marker  -> declined (pycel's #CIRCULAR!, unknowns)
  * None / empty              -> blank
  * anything array-shaped     -> the 1x1 anchor if trivially scalar, else declined
  * a per-cell exception      -> declined  (pycel's analog of #UNSUPPORTED!)

A failure to even construct the model (open/parse) -> load_failure: the harness
declines every oracle-bearing cell for the workbook (never a silent skip).

`pycel` is pinned in requirements.txt. Install into the VM venv only; the Cargo
zero-dependency rule is untouched (this is external VM tooling).
"""
from __future__ import annotations

import argparse
import sys
import time

from oracle import a1_ref, extract_oracle
from runner_common import CellSink

ENGINE = "pycel"

# Excel's built-in errors pycel may return as strings; keep in sync with
# oracle._KNOWN_ERRORS. Anything else that looks like an error is declined.
_EXCEL_ERRORS = {
    "#NULL!", "#DIV/0!", "#VALUE!", "#REF!", "#NAME?", "#NUM!", "#N/A",
    "#GETTING_DATA", "#SPILL!", "#CALC!",
}


def _scalarize(v):
    """Reduce array-ish pycel results to a scalar when trivially 1x1, else raise
    ValueError to signal 'not a scalar -> decline'."""
    # numpy scalars/0-d arrays expose .item()
    try:
        import numpy as np  # pycel pulls numpy in
        if isinstance(v, np.generic):
            return v.item()
        if isinstance(v, np.ndarray):
            flat = v.ravel()
            if flat.size == 1:
                return flat[0].item()
            raise ValueError("non-scalar array")
    except ImportError:
        pass
    if isinstance(v, (list, tuple)):
        # nested 1x1 -> unwrap; else decline
        cur = v
        while isinstance(cur, (list, tuple)):
            if len(cur) != 1:
                raise ValueError("non-scalar sequence")
            cur = cur[0]
        return cur
    return v


def emit_value(sink, sheet, row, col, v):
    try:
        v = _scalarize(v)
    except ValueError:
        sink.declined(sheet, row, col)
        return
    if v is None:
        sink.blank(sheet, row, col)
        return
    # bool BEFORE int/float (bool is a subclass of int).
    if isinstance(v, bool):
        sink.boolean(sheet, row, col, v)
        return
    if isinstance(v, (int, float)):
        import math
        if isinstance(v, float) and not math.isfinite(v):
            # non-finite is never a valid Excel cell value -> decline.
            sink.declined(sheet, row, col)
        else:
            sink.number(sheet, row, col, v)
        return
    if isinstance(v, str):
        if v in _EXCEL_ERRORS:
            sink.error(sheet, row, col, v)
        elif v.startswith("#") and v.endswith(("!", "?")):
            # pycel-internal marker (e.g. #CIRCULAR!) — not a scorable Excel
            # error; decline rather than fabricate a match/mismatch.
            sink.declined(sheet, row, col)
        else:
            sink.text(sheet, row, col, v)
        return
    # Unknown Python type (Datetime, Decimal, ...) -> decline conservatively.
    sink.declined(sheet, row, col)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("workbook")
    ap.add_argument("out")
    args = ap.parse_args()

    sink = CellSink(ENGINE, args.workbook)
    t0 = time.time()

    # Coordinates come from the harness's own oracle extractor (stdlib) so the
    # evaluated set matches exactly what will be scored.
    try:
        formula_cells = extract_oracle(args.workbook)
    except Exception as e:  # noqa: BLE001 - report as load failure, never crash the sweep
        sink.dump_load_failure(args.out, f"oracle coord extraction failed: {e}", time.time() - t0)
        return 0

    try:
        from pycel import ExcelCompiler
        excel = ExcelCompiler(filename=args.workbook)
    except Exception as e:  # noqa: BLE001
        sink.dump_load_failure(args.out, f"pycel could not open workbook: {e}", time.time() - t0)
        return 0

    for fc in formula_cells:
        addr = f"'{fc.sheet}'!{a1_ref(fc.row, fc.col)}"
        try:
            v = excel.evaluate(addr)
        except Exception:  # noqa: BLE001 - pycel raises on unimplemented fns
            sink.declined(fc.sheet, fc.row, fc.col)
            continue
        emit_value(sink, fc.sheet, fc.row, fc.col, v)

    sink.dump_ok(args.out, time.time() - t0)
    return 0


if __name__ == "__main__":
    sys.exit(main())
