"""Shared plumbing between the per-engine runners and the aggregator.

Design: each engine runner is a *standalone subprocess* invoked as

    python run_<engine>.py <workbook> <out.json>

It writes a JSON file describing the value it computed for each formula cell.
The runner NEVER classifies and NEVER looks at the oracle — it only reports raw
computed values. All scoring happens once, in `harness.py`, through the single
`classify()` port. This keeps every engine apples-to-apples and isolates each
workbook in its own process so a crashing/hanging engine takes down exactly one
workbook, never the sweep.

Per-cell value JSON shape:

    {
      "engine": "pycel",
      "workbook": "/abs/path.xlsx",
      "status": "ok" | "load_failure",
      "message": "<why, when load_failure>",
      "elapsed_s": 1.23,
      "cells": { "<sheet>": { "<row>,<col>": {"k": "<kind>", "v": <payload>} } }
    }

Kinds (`k`): number | text | bool | error | blank | declined.
  * error   -> `v` is a canonical Excel error string ("#DIV/0!"), used ONLY when
               the engine genuinely produced that Excel error.
  * declined-> the engine could not produce a comparable value (function not
               implemented, per-cell exception). `v` omitted. This is the
               competitor analog of recalc's `#UNSUPPORTED!` sentinel.
"""
from __future__ import annotations

import json
import subprocess
import time

from classify import DECLINED_COMPUTED
from oracle import BLANK, BOOL, ERROR, NUMBER, TEXT, OVal, canonical_error

# --- runner side ------------------------------------------------------------


class CellSink:
    """Accumulates per-cell computed values on the runner side, then dumps the
    JSON contract above."""

    def __init__(self, engine: str, workbook: str):
        self.engine = engine
        self.workbook = workbook
        self._cells: dict[str, dict[str, dict]] = {}

    def _put(self, sheet: str, row: int, col: int, obj: dict) -> None:
        self._cells.setdefault(sheet, {})[f"{row},{col}"] = obj

    def number(self, sheet, row, col, x):
        self._put(sheet, row, col, {"k": NUMBER, "v": float(x)})

    def text(self, sheet, row, col, s):
        self._put(sheet, row, col, {"k": TEXT, "v": str(s)})

    def boolean(self, sheet, row, col, b):
        self._put(sheet, row, col, {"k": BOOL, "v": bool(b)})

    def error(self, sheet, row, col, s):
        self._put(sheet, row, col, {"k": ERROR, "v": str(s)})

    def blank(self, sheet, row, col):
        self._put(sheet, row, col, {"k": BLANK})

    def declined(self, sheet, row, col):
        self._put(sheet, row, col, {"k": "declined"})

    def dump_ok(self, out_path: str, elapsed_s: float) -> None:
        self._dump(out_path, "ok", None, elapsed_s)

    def dump_load_failure(self, out_path: str, message: str, elapsed_s: float) -> None:
        self._dump(out_path, "load_failure", message, elapsed_s)

    def _dump(self, out_path, status, message, elapsed_s):
        doc = {
            "engine": self.engine,
            "workbook": self.workbook,
            "status": status,
            "message": message,
            "elapsed_s": elapsed_s,
            "cells": self._cells,
        }
        with open(out_path, "w") as f:
            json.dump(doc, f)


# --- aggregator side --------------------------------------------------------

# Outcome of invoking a runner subprocess for one workbook.
OK = "ok"
ENGINE_TIMEOUT = "engine_timeout"
ENGINE_CRASH = "engine_crash"
ENGINE_LOAD_FAILURE = "engine_load_failure"


def _obj_to_computed(obj: dict):
    """Turn one runner cell object into a computed value (OVal or DECLINED)."""
    k = obj.get("k")
    if k == NUMBER:
        return OVal.number(obj["v"])
    if k == TEXT:
        return OVal.text(obj["v"])
    if k == BOOL:
        return OVal.boolean(obj["v"])
    if k == ERROR:
        # Only a recognised Excel error is comparable; anything else is a
        # decline dressed as an error (the runner should already have filtered
        # these, but be defensive so an odd engine string can never be scored as
        # a spurious match/mismatch).
        s = canonical_error(obj["v"])
        return OVal.error(s)
    if k == BLANK:
        return OVal.blank()
    # "declined" or anything unknown -> declined.
    return DECLINED_COMPUTED


def run_engine(cmd: list, workbook: str, out_path: str, timeout_s: float):
    """Invoke a runner subprocess for one workbook with a hard timeout.

    Returns (outcome, computed_map, meta) where:
      * outcome is one of OK / ENGINE_TIMEOUT / ENGINE_CRASH / ENGINE_LOAD_FAILURE
      * computed_map is {(sheet,row,col): OVal|DECLINED} (empty unless OK)
      * meta is a dict with elapsed_s and any message.

    A timeout or crash means the ENGINE failed on a workbook the oracle loaded
    fine; the caller declines every oracle-bearing cell (no silent skip)."""
    t0 = time.time()
    try:
        proc = subprocess.run(
            cmd,
            timeout=timeout_s,
            capture_output=True,
            text=True,
        )
    except subprocess.TimeoutExpired:
        return ENGINE_TIMEOUT, {}, {"elapsed_s": time.time() - t0, "message": "timeout"}
    elapsed = time.time() - t0
    if proc.returncode != 0:
        msg = (proc.stderr or "").strip()[-500:]
        return ENGINE_CRASH, {}, {"elapsed_s": elapsed, "message": f"exit {proc.returncode}: {msg}"}

    try:
        with open(out_path) as f:
            doc = json.load(f)
    except (OSError, json.JSONDecodeError) as e:
        return ENGINE_CRASH, {}, {"elapsed_s": elapsed, "message": f"bad runner output: {e}"}

    if doc.get("status") != "ok":
        return ENGINE_LOAD_FAILURE, {}, {
            "elapsed_s": doc.get("elapsed_s", elapsed),
            "message": doc.get("message") or "engine load failure",
        }

    computed = {}
    for sheet, cells in (doc.get("cells") or {}).items():
        for rc, obj in cells.items():
            r_s, c_s = rc.split(",")
            computed[(sheet, int(r_s), int(c_s))] = _obj_to_computed(obj)
    return OK, computed, {"elapsed_s": doc.get("elapsed_s", elapsed), "message": None}
