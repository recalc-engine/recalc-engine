# `recalc verify` examples

Two small workbooks and one policy file for a first run of `recalc verify`.
The run takes under a minute.

| File | Purpose |
|------|---------|
| `demo.xlsx` | A one-sheet quarterly budget (`Budget`). Every formula cell carries a correct stored value. Verification passes. |
| `demo-stale.xlsx` | The same workbook after an edit to one input (`B9`, the annual budget, 75000 -> 70000) with no recalculation. Two dependent cells still carry the old stored values. Verification fails. |
| `recalc-policy.toml` | The default local policy: compare each formula cell with the value stored in the file; a formula error fails the run. Each key has a one-line comment. |
| `gen_demo.py` | The generator for both workbooks (Python 3 standard library only). Re-running it writes byte-identical files. |

## Run the passing workbook

```sh
recalc verify examples/demo.xlsx --policy examples/recalc-policy.toml --json report.json
```

Console output:

```
PASS  examples/demo.xlsx
  formula cells: 13; findings: 0 error, 0 warning
  report: report.json
```

Exit code `0`. `report.json` has `"decision": "pass"`, 13 formula cells, and
`"mismatches": 0`. Every cell carries the evidence label `matches_stored`.

## Run the stale workbook

```sh
recalc verify examples/demo-stale.xlsx --policy examples/recalc-policy.toml --json report-stale.json
```

Console output:

```
FAIL  examples/demo-stale.xlsx
  formula cells: 13; findings: 2 error, 0 warning
  error   Budget!B10  mismatch: computed value differs from stored cached value
  error   Budget!B11  mismatch: computed value differs from stored cached value
  report: report-stale.json
```

Exit code `1`. `report-stale.json` has `"decision": "fail"` and
`"mismatches": 2`. These two cells carry the evidence label `differs_stored`:

| Cell | Formula | Stored value | Recalculated value |
|------|---------|--------------|--------------------|
| `Budget!B10` | `=IF(F6>B9,"Over budget","Within budget")` | `Within budget` | `Over budget` |
| `Budget!B11` | `=ROUND(F6/B9*100,1)` | `96.8` | `103.7` |

The `issues` array of the report lists the same two cells with
`"code": "mismatch"`.

## Notes

- `--json` is optional. Without it the console summary is printed and no
  report file is written; scripts and agents should keep the JSON report.
- `--policy` is optional. Without it the defaults in `recalc-policy.toml`
  apply.
- Add `--quiet` to suppress the console summary; the exit code and the JSON
  report do not change.
- Exit codes: `0` PASS, `1` FAIL, `2` FALLBACK (no safe decision: an
  unsupported construct, missing evidence, or a file that does not load),
  `64` usage error.

## Workbook layout (`Budget` sheet)

| Cell | Content |
|------|---------|
| `A1:F1` | Header: `Line item`, `Q1`, `Q2`, `Q3`, `Q4`, `Total` |
| `A2:A5` | `Salaries`, `Rent`, `Marketing`, `Software` |
| `B2:E5` | Quarterly inputs |
| `F2:F5` | `=SUM(B2:E2)` .. `=SUM(B5:E5)` |
| `A6`, `B6:F6` | `Total`; `=SUM(B2:B5)` .. `=SUM(F2:F5)` |
| `A7`, `B7` | `Average per quarter`; `=AVERAGE(B6:E6)` |
| `A8`, `B8` | `Highest quarter`; `=MAX(B6:E6)` |
| `A9`, `B9` | `Annual budget`; `75000` (`70000` in `demo-stale.xlsx`) |
| `A10`, `B10` | `Status`; `=IF(F6>B9,"Over budget","Within budget")` |
| `A11`, `B11` | `Budget used (%)`; `=ROUND(F6/B9*100,1)` |

## Regenerate the workbooks

```sh
python3 examples/gen_demo.py
```
