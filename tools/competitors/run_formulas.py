#!/usr/bin/env python3
"""formulas runner — measures the `formulas` pure-Python engine on one workbook.

Invoked by harness.py in an isolated subprocess:  python run_formulas.py <wb> <out>

`formulas` compiles the WHOLE workbook into a dependency model and solves it in
one shot (`ExcelModel().loads(path).finish().calculate()`), returning a dict
keyed by fully-qualified cell references like `"'[BOOK.XLSX]SHEET1'!A1"`. We map
those results back onto this harness's formula-cell universe (every `<f>` cell,
from the stdlib oracle extractor) so recalc and `formulas` are scored over the
identical cell set.

Value mapping mirrors run_pycel.py; `formulas` wraps values in numpy arrays and
its own `XlError` tokens, which we unwrap defensively. Anything we cannot
confidently convert to a scalar Excel value is `declined` rather than guessed.

CAVEATS (documented in docs/competitor-harness.md):
  * Reference-key matching is by (UPPER sheet name, A1). A workbook with two
    sheets differing only by case would collide — vanishingly rare, and such a
    cell is declined, never mis-scored.
  * A compile/solve exception (unsupported construct) fails the whole workbook
    -> load_failure -> the harness declines every oracle-bearing cell.

`formulas` is pinned in requirements.txt; VM venv only.
"""
from __future__ import annotations

import argparse
import math
import os
import re
import sys
import time

from oracle import a1_ref, extract_oracle
from runner_common import CellSink

ENGINE = "formulas"

_EXCEL_ERRORS = {
    "#NULL!", "#DIV/0!", "#VALUE!", "#REF!", "#NAME?", "#NUM!", "#N/A",
    "#GETTING_DATA", "#SPILL!", "#CALC!",
}

# "'[book.xlsx]Sheet1'!A1"  ->  (sheet, a1)
_KEY_RE = re.compile(r"^'?\[[^\]]*\](?P<sheet>[^']+?)'?!(?P<a1>\$?[A-Za-z]+\$?[0-9]+)$")


def _unwrap(v):
    """Reduce a formulas result to a Python scalar, or raise ValueError.

    `formulas` wraps every cell result in a `Ranges` object whose `.value` is a
    numpy array; unwrap that first, then peel the (typically 1x1) array to a
    scalar element (which may itself be a numpy scalar or an `XlError` token)."""
    # formulas.ranges.Ranges -> underlying numpy array
    if type(v).__name__ == "Ranges" and hasattr(v, "value"):
        v = v.value
    try:
        import numpy as np
        if isinstance(v, np.generic):
            return v.item()
        if isinstance(v, np.ndarray):
            flat = v.ravel()
            if flat.size == 1:
                el = flat[0]
                return el.item() if hasattr(el, "item") else el
            raise ValueError("non-scalar array")
    except ImportError:
        pass
    if isinstance(v, (list, tuple)):
        cur = v
        while isinstance(cur, (list, tuple)):
            if len(cur) != 1:
                raise ValueError("non-scalar sequence")
            cur = cur[0]
        return cur
    return v


def emit_value(sink, sheet, row, col, v):
    try:
        v = _unwrap(v)
    except ValueError:
        sink.declined(sheet, row, col)
        return
    if v is None:
        sink.blank(sheet, row, col)
        return
    if isinstance(v, bool):
        sink.boolean(sheet, row, col, v)
        return
    if isinstance(v, (int, float)):
        if isinstance(v, float) and not math.isfinite(v):
            sink.declined(sheet, row, col)
        else:
            sink.number(sheet, row, col, v)
        return
    if isinstance(v, str):
        # Coerce to a PLAIN str: formulas' XlError subclasses str but overrides
        # __eq__/__hash__, so `xlerror in _EXCEL_ERRORS` (set membership) is
        # False even when its characters are "#DIV/0!". str() gives a plain str.
        s = str(v)
        if s in _EXCEL_ERRORS:
            sink.error(sheet, row, col, s)
        elif s.startswith("#") and (s.endswith("!") or s.endswith("?")):
            sink.declined(sheet, row, col)  # formulas-internal marker
        else:
            sink.text(sheet, row, col, s)
        return
    # An XlError token or other non-primitive: use its string form if it is a
    # recognised Excel error, else decline (never fabricate a scorable value).
    try:
        sval = str(v)
    except Exception:  # noqa: BLE001
        sval = ""
    if sval in _EXCEL_ERRORS:
        sink.error(sheet, row, col, sval)
    else:
        sink.declined(sheet, row, col)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("workbook")
    ap.add_argument("out")
    args = ap.parse_args()

    sink = CellSink(ENGINE, args.workbook)
    t0 = time.time()

    try:
        formula_cells = extract_oracle(args.workbook)
    except Exception as e:  # noqa: BLE001
        sink.dump_load_failure(args.out, f"oracle coord extraction failed: {e}", time.time() - t0)
        return 0

    try:
        import formulas
        xl_model = formulas.ExcelModel().loads(args.workbook).finish()
        sol = xl_model.calculate()
    except Exception as e:  # noqa: BLE001 - unsupported construct fails the book
        sink.dump_load_failure(args.out, f"formulas could not solve workbook: {e}", time.time() - t0)
        return 0

    # Index the solution by (UPPER sheet, UPPER a1).
    by_cell = {}
    for key, val in dict(sol).items():
        m = _KEY_RE.match(str(key))
        if not m:
            continue
        sheet = m.group("sheet").upper()
        a1 = m.group("a1").replace("$", "").upper()
        by_cell[(sheet, a1)] = val

    for fc in formula_cells:
        a1 = a1_ref(fc.row, fc.col)
        key = (fc.sheet.upper(), a1)
        if key not in by_cell:
            # formulas produced no result for this formula cell -> declined
            # (declared gap; never silently dropped).
            sink.declined(fc.sheet, fc.row, fc.col)
            continue
        emit_value(sink, fc.sheet, fc.row, fc.col, by_cell[key])

    sink.dump_ok(args.out, time.time() - t0)
    return 0


if __name__ == "__main__":
    sys.exit(main())
