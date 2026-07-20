# recalc-engine (Python / Node bindings)

Language bindings for [Recalc](https://github.com/recalc-engine/recalc-engine) —
a headless, bug-for-bug Excel-compatible spreadsheet recalculation engine. Open
an `.xlsx`/`.xlsm` workbook, build its formula dependency graph, and recalculate
exactly as Microsoft Excel would — same values, same errors, same quirks — with
no UI and no Excel installation.

Two principles run through the engine:

- **Fidelity is a measured number.** Agreement with a pinned Excel build is
  measured on a corpus of real workbooks, not asserted.
- **Never silently wrong.** Anything the engine cannot compute returns a
  distinguishable error value and a diagnostic, never a guess.

## Python

```
pip install recalc-engine
```

The imported module is `recalc`:

```python
import recalc

wb = recalc.open("model.xlsx")          # or recalc.open_bytes(raw_bytes)
wb.recalc()                             # compute in dependency order

v = wb.cell("Sheet1", "B2")             # or wb.value("Sheet1", 1, 1)  (0-based)

# An error value is a distinct type, never a bare string.
if isinstance(v, recalc.CellError):
    print("flagged:", v.code)           # e.g. "#DIV/0!", "#UNSUPPORTED!"
else:
    print("value:", v)                  # float | str | bool | None | list[list]

for d in wb.diagnostics():
    print(d.sheet, d.row, d.col, d.kind, d.message)
```

Value mapping: `Number → float`, `Text → str`, `Bool → bool`, `Blank → None`,
`Array → list[list[...]]` (row-major), `Error → recalc.CellError`.

## Node.js

```
npm install recalc-engine
```

```js
const { open, openBytes, CellError } = require("recalc-engine");

const wb = open("model.xlsx");          // or openBytes(buffer)
wb.recalc();

const v = wb.cell("Sheet1", "B2");      // or wb.value("Sheet1", 1, 1)
if (v instanceof CellError) {
  console.log("flagged:", v.code);
} else {
  console.log("value:", v);             // number | string | boolean | null | any[][]
}
```

Value mapping: `Number → number`, `Text → string`, `Bool → boolean`,
`Blank → null`, `Array → Array<Array<...>>`, `Error → CellError`.

Reading a cell *before* `recalc()` returns the file's cached value (whatever
Excel last stored), not a value this engine computed. Call `recalc()` first.

## License

Apache-2.0.
