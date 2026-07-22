#!/usr/bin/env python3
"""Competitor-measurement aggregator (INTERNAL).

Measures a third-party engine (or recalc itself, for validation) over a set of
workbooks using the SAME oracle and the SAME comparison semantics as
`xl-bench`'s `verify-dir`, and emits the same summary shape.

Pipeline per workbook (see the module docs of oracle.py / classify.py / funnel.py):
  1. Extract the oracle (Excel's cached <v> per formula cell). If the oracle
     cannot be parsed -> load failure, workbook contributes ZERO cells (exactly
     as verify-dir excludes an xl-io LoadFailure).
  2. Run the engine in an isolated subprocess with a per-workbook timeout.
     Crash / hang / nonzero exit / "load_failure" -> the ENGINE failed on a
     workbook the oracle loaded fine: every oracle-bearing formula cell is
     `declined`, every no-oracle cell is `no_oracle` (NoOracle wins first).
     Never a silent skip.
  3. Classify each formula cell through the single `classify()` port.
  4. Fold into the Funnel; emit verify-dir-shaped strict/lenient fidelity.

Engines: pycel | formulas | libreoffice | recalc. `recalc` runs recalc's own
`verify --json` and reconstructs per-cell values from it — used to VALIDATE that
this harness reproduces `verify-dir`'s funnel exactly (apples-to-apples proof).
"""
from __future__ import annotations

import argparse
import os
import sys
import tempfile

from classify import DECLINED_COMPUTED, classify
from funnel import Funnel
from oracle import OracleLoadError, extract_oracle
from runner_common import OK, run_engine
from sample import discover_workbooks, evenly_spaced_sample

HERE = os.path.dirname(os.path.abspath(__file__))

RUNNER_SCRIPTS = {
    "pycel": "run_pycel.py",
    "formulas": "run_formulas.py",
    "libreoffice": "run_libreoffice.py",
    "recalc": "run_recalc.py",
}


def build_cmd(engine: str, engine_python: str, workbook: str, out_path: str, extra: list) -> list:
    script = os.path.join(HERE, RUNNER_SCRIPTS[engine])
    return [engine_python, script, workbook, out_path, *extra]


def measure_workbook(engine, engine_python, workbook, timeout_s, fuzzy, extra, funnel):
    # 1. Oracle.
    try:
        formula_cells = extract_oracle(workbook)
    except OracleLoadError as e:
        funnel.load_failures += 1
        funnel.per_workbook.append(
            {"workbook": workbook, "status": "oracle_load_failure", "message": str(e)}
        )
        return

    funnel.workbooks_scored += 1

    # 2. Engine subprocess (isolated, timed).
    with tempfile.NamedTemporaryFile(suffix=".json", delete=False) as tf:
        out_path = tf.name
    try:
        cmd = build_cmd(engine, engine_python, workbook, out_path, extra)
        outcome, computed, meta = run_engine(cmd, workbook, out_path, timeout_s)
    finally:
        try:
            os.unlink(out_path)
        except OSError:
            pass

    engine_failed = outcome != OK
    if engine_failed:
        funnel.engine_workbook_failures += 1

    # 3+4. Classify every formula cell; fold into the funnel.
    wb_counts = {"exact": 0, "ulp_diff": 0, "mismatch": 0, "declined": 0, "no_oracle": 0}
    for fc in formula_cells:
        if engine_failed:
            comp = DECLINED_COMPUTED
        else:
            comp = computed.get((fc.sheet, fc.row, fc.col), DECLINED_COMPUTED)
        status = classify(comp, fc.oracle, fuzzy_15sig=fuzzy)
        funnel.add_status(status)
        wb_counts[status] += 1

    funnel.per_workbook.append(
        {
            "workbook": workbook,
            "status": "engine_failure" if engine_failed else "ok",
            "outcome": outcome,
            "message": meta.get("message"),
            "elapsed_s": round(meta.get("elapsed_s", 0.0), 3),
            "cells": len(formula_cells),
            **wb_counts,
        }
    )


def select_workbooks(args) -> list:
    if args.paths_file:
        with open(args.paths_file) as f:
            return [ln.strip() for ln in f if ln.strip()]
    if args.paths:
        return list(args.paths)
    if args.sample_dir:
        return evenly_spaced_sample(discover_workbooks(args.sample_dir), args.n)
    if args.dir:
        return discover_workbooks(args.dir)
    raise SystemExit("no workbooks selected: pass --sample-dir/--dir/--paths-file or positional paths")


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="Competitor measurement harness (INTERNAL).")
    ap.add_argument("--engine", required=True, choices=sorted(RUNNER_SCRIPTS))
    ap.add_argument("--engine-python", default=sys.executable,
                    help="python used to run the engine runner subprocess "
                         "(point at the venv python for pycel/formulas)")
    ap.add_argument("--sample-dir", help="corpus root; take the deterministic N-book sample")
    ap.add_argument("--n", type=int, default=50, help="sample size for --sample-dir")
    ap.add_argument("--dir", help="corpus root; measure EVERY workbook (the full sweep)")
    ap.add_argument("--paths-file", help="file of explicit workbook paths, one per line")
    ap.add_argument("paths", nargs="*", help="explicit workbook paths")
    ap.add_argument("--timeout", type=float, default=120.0, help="per-workbook seconds")
    ap.add_argument("--tol", dest="tol", default="bit-exact",
                    help="'15sig' for the 15-significant-figure float tolerance; "
                         "default is the bit-exact floor")
    ap.add_argument("--fuzzy", action="store_true", help="alias for --tol=15sig")
    ap.add_argument("--out", help="write the Funnel JSON here")
    ap.add_argument("--quiet", action="store_true")
    # Engine-specific pass-throughs.
    ap.add_argument("--soffice", help="soffice binary (libreoffice engine)")
    ap.add_argument("--recalc-bin", help="recalc binary (recalc engine)")
    args = ap.parse_args(argv)

    fuzzy = args.fuzzy or args.tol in ("15sig", "--tol=15sig", "15-sig-fig")
    scoring_mode = "15-sig-fig" if fuzzy else "bit-exact"

    extra = []
    if args.engine == "libreoffice" and args.soffice:
        extra += ["--soffice", args.soffice]
    if args.engine == "recalc" and args.recalc_bin:
        extra += ["--recalc-bin", args.recalc_bin]

    workbooks = select_workbooks(args)
    funnel = Funnel(engine=args.engine, scoring_mode=scoring_mode)

    total = len(workbooks)
    if not args.quiet:
        print(f"[SCORING MODE] {scoring_mode}  [ENGINE] {args.engine}  "
              f"[{total} workbook(s), timeout {args.timeout}s]", flush=True)
    for i, wb in enumerate(workbooks, 1):
        measure_workbook(args.engine, args.engine_python, wb, args.timeout, fuzzy, extra, funnel)
        if not args.quiet:
            last = funnel.per_workbook[-1]
            print(f"[{i}/{total}] {os.path.basename(wb)}: {last['status']}"
                  + (f" ({last.get('outcome')})" if last['status'] == 'engine_failure' else "")
                  + (f" — exact {last.get('exact')}, mismatch {last.get('mismatch')}, "
                     f"declined {last.get('declined')}, no_oracle {last.get('no_oracle')}"
                     if last['status'] not in ('oracle_load_failure',) else ""),
                  flush=True)

    print("\n" + funnel.summary_text(), flush=True)
    if args.out:
        with open(args.out, "w") as f:
            f.write(funnel.to_json())
        if not args.quiet:
            print(f"\nwrote {args.out}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
