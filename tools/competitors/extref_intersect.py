#!/usr/bin/env python3
"""External-workbook-ref decomposition of an engine's fidelity (INTERNAL).

Leaderboard publish **condition 1** (steering memo 2026-07-20 §3.1): measure how
much of an engine's strict fidelity rests on cached external-link values it could
not have computed. LibreOffice's `calculateAll` cannot recompute a reference into
a workbook that is not on disk (the FUSE corpus ships each book standalone, its
linked externals absent), so on those cells LO can only echo Excel's last-saved
cached value — which IS the oracle — and scores `exact` by echo, not by
computation. recalc, by contrast, *declines* every such cell on principle
(no network / no fs). This script quantifies that split.

The **external-ref cell set** is recalc's own decline-attribution output: every
cell classified `external_direct` or `external_cascade` by
`recalc decline-attribution --dump-cells` (986,069 cells @ v4/`bbb6984`). This
script intersects that set, cell-for-cell, with an engine's per-cell results
scored through the SAME oracle + classifier as `harness.py` (so the numbers are
apples-to-apples with the §6b funnels), and reports, split by membership:

    external-ref cells  : exact / ulp / mismatch / declined / no_oracle
    the rest (in-subset): exact / ulp / mismatch / declined / no_oracle

Both scoring tolerances (bit-exact floor + 15-sig headline) are scored from a
SINGLE engine execution per workbook — the two tolerances differ only in the
number comparison inside `classify()`, so one run of the engine feeds both
tallies (strictly cleaner than the §6b sweep, whose two tolerances were
independent executions with their own timeout variance).

Only the workbooks that BEAR at least one external-ref cell are run (the rest of
the corpus contributes zero external cells by construction). "The rest, whole
corpus" is then `whole-corpus funnel − external tally` (the whole-corpus funnel
is the §6b sweep number, supplied for the subtraction / cross-checked per
workbook against the retained shard `per_workbook` records).

INTERNAL / UNPUBLISHED. Reuses the committed, md5-pinned harness modules
(oracle.py / classify.py / runner_common.py) and the committed LO runner
(run_libreoffice.py) unchanged — same code path as harness.py.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
import time
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from classify import DECLINED_COMPUTED, classify  # noqa: E402
from oracle import OracleLoadError, extract_oracle, parse_a1  # noqa: E402
from runner_common import OK, run_engine  # noqa: E402

STATUSES = ("exact", "ulp_diff", "mismatch", "declined", "no_oracle")
EXTERNAL_CLASSES = ("external_direct", "external_cascade")


def _zero():
    return {s: 0 for s in STATUSES}


def load_extref(path):
    """decl-cells.tsv -> (ext_by_wb, n_direct, n_cascade).

    ext_by_wb[workbook] = set of (sheet, row0, col0) for external_* declines.
    Keys match the harness's per-cell keys exactly: sheet DISPLAY name +
    0-based (row, col) via the same `parse_a1` the oracle uses on `<c r=...>`.
    """
    ext = defaultdict(set)
    n_direct = n_cascade = 0
    with open(path) as f:
        for ln in f:
            parts = ln.rstrip("\n").split("\t")
            if len(parts) != 4:
                continue
            wb, sheet, a1, cls = parts
            if cls not in EXTERNAL_CLASSES:
                continue
            row0, col0 = parse_a1(a1)
            ext[wb].add((sheet, row0, col0))
            if cls == "external_direct":
                n_direct += 1
            else:
                n_cascade += 1
    return ext, n_direct, n_cascade


def measure(wb, ext_keys, engine_python, runner, extra, timeout_s):
    """Run the engine once on `wb`; return dual-tolerance tallies.

    Returns (ok, ext_tally, rest_tally, seen_ext, whole_funnel_15sig, outcome)
    where the tallies are {tol: {status: count}} for tol in (bitexact, 15sig).
    """
    try:
        formula_cells = extract_oracle(wb)
    except OracleLoadError as e:
        return None, None, None, 0, None, f"oracle_load_failure: {e}"

    with tempfile.NamedTemporaryFile(suffix=".json", delete=False) as tf:
        out_path = tf.name
    try:
        cmd = [engine_python, os.path.join(HERE, runner), wb, out_path, *extra]
        outcome, computed, meta = run_engine(cmd, wb, out_path, timeout_s)
    finally:
        try:
            os.unlink(out_path)
        except OSError:
            pass

    failed = outcome != OK
    ext_tally = {"bitexact": _zero(), "15sig": _zero()}
    rest_tally = {"bitexact": _zero(), "15sig": _zero()}
    whole_15sig = _zero()  # for the shard cross-check (whole-workbook funnel)
    seen_ext = 0
    for fc in formula_cells:
        comp = DECLINED_COMPUTED if failed else computed.get(
            (fc.sheet, fc.row, fc.col), DECLINED_COMPUTED
        )
        st_bit = classify(comp, fc.oracle, fuzzy_15sig=False)
        st_15 = classify(comp, fc.oracle, fuzzy_15sig=True)
        whole_15sig[st_15] += 1
        if (fc.sheet, fc.row, fc.col) in ext_keys:
            ext_tally["bitexact"][st_bit] += 1
            ext_tally["15sig"][st_15] += 1
            seen_ext += 1
        else:
            rest_tally["bitexact"][st_bit] += 1
            rest_tally["15sig"][st_15] += 1
    return (not failed), ext_tally, rest_tally, seen_ext, whole_15sig, outcome


def add_into(dst, src):
    for tol in dst:
        for s in STATUSES:
            dst[tol][s] += src[tol][s]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--extref", required=True, help="decl-cells.tsv from decline-attribution --dump-cells")
    ap.add_argument("--engine", default="libreoffice")
    ap.add_argument("--runner", default="run_libreoffice.py")
    ap.add_argument("--engine-python", default=sys.executable)
    ap.add_argument("--soffice")
    ap.add_argument("--timeout", type=float, default=180.0)
    ap.add_argument("--out", help="write the result JSON here")
    ap.add_argument("--percell-out", help="write per-workbook whole-funnel (15sig) TSV for the shard cross-check")
    ap.add_argument("--limit", type=int, default=0, help="cap workbooks (debug)")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args(argv)

    extra = []
    if args.soffice:
        extra += ["--soffice", args.soffice]

    ext, n_direct, n_cascade = load_extref(args.extref)
    wbs = sorted(ext.keys())
    if args.limit:
        wbs = wbs[:args.limit]
    total = len(wbs)
    print(f"[extref] external cells: direct {n_direct} + cascade {n_cascade} = "
          f"{n_direct + n_cascade} across {total} workbook(s); engine={args.engine} "
          f"timeout={args.timeout}s", flush=True)

    ext_tot = {"bitexact": _zero(), "15sig": _zero()}
    rest_tot = {"bitexact": _zero(), "15sig": _zero()}
    engine_fail = 0
    oracle_fail = 0
    missing_ext = 0
    percell = []
    t0 = time.time()
    for i, wb in enumerate(wbs, 1):
        ok, et, rt, seen, whole15, outcome = measure(
            wb, ext[wb], args.engine_python, args.runner, extra, args.timeout
        )
        if et is None:
            oracle_fail += 1
            if not args.quiet:
                print(f"[{i}/{total}] {os.path.basename(wb)}: {outcome}", flush=True)
            continue
        if not ok:
            engine_fail += 1
        add_into(ext_tot, et)
        add_into(rest_tot, rt)
        missing_ext += (len(ext[wb]) - seen)
        percell.append((wb, whole15, len(ext[wb])))
        if not args.quiet:
            print(f"[{i}/{total}] {os.path.basename(wb)}: {outcome} — "
                  f"ext15 exact {et['15sig']['exact']} mm {et['15sig']['mismatch']} "
                  f"decl {et['15sig']['declined']}", flush=True)

    elapsed = time.time() - t0
    ext_measured = sum(ext_tot["15sig"].values())
    result = {
        "engine": args.engine,
        "extref_file": os.path.abspath(args.extref),
        "external_cells_expected": {"direct": n_direct, "cascade": n_cascade,
                                    "total": n_direct + n_cascade},
        "external_cells_measured": ext_measured,
        "missing_ext_keys": missing_ext,  # ext keys NOT found among formula cells (want 0)
        "workbooks": total,
        "engine_workbook_failures": engine_fail,
        "oracle_load_failures": oracle_fail,
        "elapsed_s": round(elapsed, 1),
        "external_tally": ext_tot,        # {tol: {status: n}}  ON the external-ref cells
        "rest_subset_tally": rest_tot,    # {tol: {status: n}}  non-external cells IN the run subset
    }
    print("\n=== EXTERNAL-REF DECOMPOSITION (INTERNAL) ===", flush=True)
    print(json.dumps(result, indent=2), flush=True)

    if args.out:
        with open(args.out, "w") as f:
            json.dump(result, f, indent=2)
        print(f"\nwrote {args.out}", flush=True)
    if args.percell_out:
        with open(args.percell_out, "w") as f:
            for wb, w15, n_ext in percell:
                f.write(f"{wb}\t{n_ext}\t" + "\t".join(str(w15[s]) for s in STATUSES) + "\n")
        print(f"wrote {args.percell_out}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
