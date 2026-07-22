# tools/competitors — competitor measurement harness

Measures third-party spreadsheet engines (LibreOffice, pycel, formulas) on the
FUSE corpus through the **same oracle and same comparison semantics** as
`xl-bench verify-dir`, so recalc can be placed next to competitors
apples-to-apples on the ExcelBench / Sheetmark leaderboard.

**Full design, caveats, sample rule, and how-to-run: `docs/competitor-harness.md`.**

Quick start (see the doc for the real thing):

```
# tests (stdlib only; recalc-reproduction tests skip if recalc isn't built)
python3 -m unittest discover -s tools/competitors/tests

# on the measurement host: install engines
bash tools/competitors/setup_farm.sh

# deterministic 50-book smoke, one engine
python3 tools/competitors/harness.py --engine pycel \
    --engine-python "$HOME/.venvs/recalc-competitors/bin/python" \
    --sample-dir "$CORPUS_DIR" --n 50 --out /tmp/pycel-smoke.json
```

- The Cargo workspace **zero-dependency rule is untouched** — these are external
  tools (stdlib Python + two pinned pip engines), no crate deps added.
- Clean-room: LibreOffice is driven only via its public UNO API; no GPL source
  is read.
- The corpus (`$CORPUS_DIR`) is READ-ONLY and never modified.
