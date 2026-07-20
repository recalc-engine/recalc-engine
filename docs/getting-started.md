# Getting started with Recalc

## What Recalc is

Recalc is a headless, bug-for-bug Excel-compatible spreadsheet **calculation
engine** written in Rust. It opens an `.xlsx`/`.xlsm` file, rebuilds the formula
dependency graph, and recalculates exactly as Microsoft Excel would — same
values, same errors, same quirks — with **no Excel installation and no UI**. It
is an open-core *library* (Apache-2.0), shipped as bindings for Python, Node.js,
WASM, and Rust. (A separate proprietary batch server, `xl-server`, is *not*
covered here — this document is the free library only.)

The differentiator is a hard product rule: **never silently wrong.** Any
function or construct the engine cannot faithfully reproduce becomes a
*distinguishable* error value — `#UNSUPPORTED!` (or `#BLOCKED!` for sandbox-
blocked I/O like `WEBSERVICE`) — plus a workbook-level **diagnostic**, rather
than a guessed value that looks right but isn't. Every cell you read back is
either exactly what Excel computes, or it is loudly flagged.

---

## Install

> **State of the world (v0.1.0):** the crates are **not yet published** to
> crates.io, PyPI, or npm (`publish = false` in the workspace). Today you
> **build from source**. There is no `pip install recalc` / `npm install recalc`
> yet — treat any such command as aspirational until the first release.

All bindings live in the `xl-ffi` crate behind **off-by-default** Cargo
features (`python` / `node` / `wasm`); a plain `cargo build --workspace` links
*zero* binding dependencies. You turn on exactly one binding per build.

Prerequisites: a Rust toolchain (workspace `rust-version = 1.96`). In this repo
the Homebrew `rustup` is keg-only, so prefix cargo/rustc commands with:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
```

### Python (PyO3 → module `recalc`)

Built with **maturin** (external build tool, never a crate dependency) into a
single portable `abi3` wheel targeting CPython **3.10+**:

```sh
# dev install into the active virtualenv
cd xl-ffi
maturin develop --features python

# or build a redistributable wheel
maturin build --release --features python
```

The imported module is `recalc` (from `#[pymodule] fn recalc`). These commands
are documented in `xl-ffi/pyproject.toml`.

### Node.js (napi-rs → `.node` addon)

Enabled by the `node` feature; the native addon is produced with the
**`@napi-rs/cli`** tool (external tooling, never a crate dependency). N-API
stable-ABI level `napi4`, so one addon per platform loads on any Node ≥ 10.16.

```sh
# builds the .node addon with the `node` feature enabled
napi build --release --features node   # [verify: exact @napi-rs/cli flags — no package.json is checked in yet]
```

### WASM (wasm-bindgen)

Enabled by the `wasm` feature; the module is produced by **`wasm-bindgen-cli`**
(pinned to `0.2.126` — the external CLI version must equal the crate pin) or via
`wasm-pack`. Single-threaded by construction (no threads, no network).

```sh
cargo build -p xl-ffi --release --features wasm --target wasm32-unknown-unknown
wasm-bindgen --target web target/wasm32-unknown-unknown/release/xl_ffi.wasm --out-dir pkg
# [verify: exact wasm-bindgen/wasm-pack invocation — no packaging script is checked in yet]
```

### Rust (use the crates directly)

There is no separate "binding" for Rust — you depend on the engine crates. Since
nothing is published yet, use a path or git dependency:

```toml
[dependencies]
xl-io     = { path = "path/to/recalc/xl-io" }
xl-engine = { path = "path/to/recalc/xl-engine" }
```

---

## Quickstart

Each example does the same four things: **load → recalc → read a computed cell →
check diagnostics.** Cell coordinates in the `value(sheet, row, col)` form are
**0-based**; the `cell(sheet, a1)` form takes an Excel A1 address instead.

> Reading a cell *before* calling `recalc()` returns the file's **cached** value
> (whatever Excel last stored), not a value Recalc computed. Call `recalc()`
> first for fresh values.

### Python

```python
import recalc

wb = recalc.open("model.xlsx")          # or recalc.open_bytes(raw_bytes)
wb.recalc()                             # compute in dependency order

print(wb.sheet_names())                 # ['Sheet1', 'Summary', ...]

# By 0-based (row, col):
v = wb.value("Sheet1", 1, 1)            # B2
# ...or by A1 address:
v = wb.cell("Sheet1", "B2")

# "Never silently wrong": an error value is a distinct type, not a str.
if isinstance(v, recalc.CellError):
    print("flagged:", v.code)           # e.g. "#UNSUPPORTED!", "#DIV/0!"
else:
    print("value:", v)                  # float | str | bool | None | list[list]

# Enumerate everything the engine refused to compute:
for d in wb.diagnostics():
    print(d.sheet, d.row, d.col, d.kind, d.message)
```

Value mapping: `Number → float`, `Text → str`, `Bool → bool`, `Blank → None`,
`Array → list[list[...]]` (row-major), `Error → recalc.CellError`.

### Node.js

```js
const { open, openBytes, CellError } = require("./index.js"); // the built addon

const wb = open("model.xlsx");          // or openBytes(buffer)  (Buffer | Uint8Array)
wb.recalc();

console.log(wb.sheetNames());

const v = wb.cell("Sheet1", "B2");      // or wb.value("Sheet1", 1, 1)

if (v instanceof CellError) {
  console.log("flagged:", v.code);      // "#UNSUPPORTED!", "#DIV/0!", ...
} else {
  console.log("value:", v);             // number | string | boolean | null | any[][]
}

for (const d of wb.diagnostics()) {
  console.log(d.sheet, d.row, d.col, d.kind, d.message);
}
```

Value mapping: `Number → number`, `Text → string`, `Bool → boolean`,
`Blank → null`, `Array → Array<Array<...>>`, `Error → CellError`.

### WASM (browser / JS)

There is **no `open(path)`** in the browser sandbox — `openBytes` (a
`Uint8Array`) is the sole loader.

```js
import init, { openBytes, CellError, surfaceVersion } from "./pkg/xl_ffi.js";

await init();                           // instantiate the wasm module

const bytes = new Uint8Array(await file.arrayBuffer());
const wb = openBytes(bytes);
wb.recalc();

const v = wb.cell("Sheet1", "B2");
if (v instanceof CellError) {
  console.log("flagged:", v.code);
} else {
  console.log("value:", v);
}

for (const d of wb.diagnostics()) {
  console.log(d.sheet, d.row, d.col, d.kind, d.message);
}

wb.free();                              // wasm-bindgen destructor: release the instance
```

`surfaceVersion()` is a **function** here (wasm-bindgen exports functions, not
constants), unlike the Python/Node `SURFACE_VERSION` constant.

### Rust

```rust
use xl_engine::{Engine, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load the workbook (xl-io), then wrap it in the engine.
    let workbook = xl_io::open("model.xlsx")?;   // or xl_io::from_bytes(&bytes)?
    let mut engine = Engine::load(workbook);

    engine.recalc();

    let sid = engine.sheet_id("Sheet1").expect("sheet exists");
    match engine.value(sid, 1, 1) {              // 0-based (row, col) → B2
        Some(Value::Number(n)) => println!("value: {n}"),
        Some(Value::Error(kind)) => println!("flagged: {}", kind.as_str()),
        Some(other) => println!("value: {other:?}"),
        None => println!("blank / never populated"),
    }

    for d in engine.diagnostics() {
        println!("{:?} {} {}: {}", d.kind, d.cell.row, d.cell.col, d.message);
    }
    Ok(())
}
```

---

## Reading results & the fidelity report

Recalc never overwrites a doubtful cell with a plausible guess. There are two
places the "I couldn't do this faithfully" signal surfaces, and you should check
both:

1. **In the cell value.** A refused or errored cell reads back as a *distinct
   error type* — `CellError` in Python/Node/WASM (branch with
   `isinstance(v, recalc.CellError)` / `v instanceof CellError`), or
   `Value::Error(kind)` in Rust. Its `.code` (Rust: `kind.as_str()`) is the
   exact Excel error string. Because the error is a *type*, not a bare
   `"#DIV/0!"` string, a cell that literally *contains the text* `"#DIV/0!"` is
   never confused with a cell that actually *errored*. The Recalc-specific
   sentinels are `#UNSUPPORTED!` (engine can't reproduce it) and `#BLOCKED!`
   (sandbox-blocked I/O such as `WEBSERVICE`/`RTD`/`STOCKHISTORY`).

2. **In the workbook diagnostics — the fidelity report.** `diagnostics()`
   returns one record per refusal, located by `sheet`, 0-based `row`/`col`, a
   stable machine-readable `kind`, and a human-readable `message`. Branch on
   `kind`, not on the message text. The `kind` is one of:

   | `kind` | Meaning |
   |---|---|
   | `ParseError` | The formula text couldn't be parsed. |
   | `UnknownFunction` | A called function isn't in the registry. |
   | `ArityError` | Wrong number of arguments for the function. |
   | `UnsupportedConstruct` | A construct the engine refuses rather than guess (e.g. an unsupported reference shape, a shared/array follow-on cell). |
   | `CircularReference` | The cell is in a cycle (non-iterative, or pending iterative calc). |

   **Load-time** refusals (parse errors, unsupported constructs) are present
   immediately after `open`/`open_bytes`, *before* any recalc; **eval-time**
   refusals (unknown functions, circular references) are added by `recalc()`.
   A clean workbook returns an empty list.

---

## API overview

The three bindings expose the **same operation set** (a "surface version",
asserted equal across bindings). Only spelling and host idiom differ.

| Concept | Python (`recalc`) | Node (napi) | WASM (wasm-bindgen) |
|---|---|---|---|
| Load from path | `recalc.open(path)` | `open(path)` | — *(no fs in browser)* |
| Load from bytes | `recalc.open_bytes(data)` | `openBytes(data)` | `openBytes(data)` |
| Recalculate | `wb.recalc()` | `wb.recalc()` | `wb.recalc()` |
| Sheet names | `wb.sheet_names()` | `wb.sheetNames()` | `wb.sheetNames()` |
| Cell by (row,col) | `wb.value(sheet, row, col)` | `wb.value(sheet, row, col)` | `wb.value(sheet, row, col)` |
| Cell by A1 | `wb.cell(sheet, a1)` | `wb.cell(sheet, a1)` | `wb.cell(sheet, a1)` |
| Diagnostics | `wb.diagnostics()` | `wb.diagnostics()` | `wb.diagnostics()` |
| Spill region | `wb.spill_region(sheet, a1)` | `wb.spillRegion(sheet, a1)` | `wb.spillRegion(sheet, a1)` |
| Error value type | `recalc.CellError` (`.code`) | `CellError` (`.code`) | `CellError` (`.code`) |
| Diagnostic type | `recalc.Diagnostic` | `Diagnostic` | `Diagnostic` |
| Surface version | `recalc.SURFACE_VERSION` | `SURFACE_VERSION` | `surfaceVersion()` |
| Free instance | *(GC)* | *(GC)* | `wb.free()` |

`Diagnostic` fields are identical across all three: `sheet`, `row`, `col`,
`kind`, `message`.

**Rust core (`xl-engine`)** — the surface the bindings wrap:

| Method | Signature |
|---|---|
| `Engine::load` | `fn load(workbook: xl_io::Workbook) -> Engine` |
| `recalc` | `fn recalc(&mut self) -> RecalcResult` |
| `edit` | `fn edit(&mut self, cell: CellId, input: CellInput)` |
| `value` | `fn value(&self, sheet: SheetId, row: u32, col: u32) -> Option<&Value>` |
| `value_at` | `fn value_at(&self, cell: CellId) -> Option<&Value>` |
| `spill_region` | `fn spill_region(&self, sheet: SheetId, row: u32, col: u32) -> Option<Value>` |
| `diagnostics` | `fn diagnostics(&self) -> Vec<&Diagnostic>` |
| `diagnostics_for` | `fn diagnostics_for(&self, sheet: SheetId, row: u32, col: u32) -> &[Diagnostic]` |
| `sheet_id` | `fn sheet_id(&self, name: &str) -> Option<SheetId>` |
| `sheet_names` | `fn sheet_names(&self) -> Vec<String>` |
| `eval_count` | `fn eval_count(&self) -> u64` |
| `last_recalc_cells` | `fn last_recalc_cells(&self) -> &[CellId]` |

Loaders live in `xl-io`: `xl_io::open(path)` and `xl_io::from_bytes(&[u8])`,
each returning `Result<Workbook, xl_io::IoError>`. `xl-engine` re-exports
`CellId`, `SheetId`, and `Value`; `CellId::new(sheet, row, col)` and
`SheetId(u32)` construct identities for `value_at`/`edit`.

> **Note on `spill_region`/`spillRegion`:** for a dynamic-array **anchor** cell,
> this returns the spilled region as an array (`Value::Array` in Rust;
> `list[list]` / `Array<Array>` in the bindings), reconstructed on demand from the
> live spill registry the engine maintains on every recalc. A cell that is *not* a
> spill anchor returns `None`/`null` — a genuine "not an anchor" answer, never a
> guess. Note the v1 scope limit below: spills are **compute-only** — the region
> is available through this query but is not written back into the file.

---

## Scope — what it doesn't do (yet)

Candor about scope is a feature, not an apology. v1 is a **calculation** engine;
it deliberately does **not**:

- **Render anything** — no layout, charts, pivot-table UI, or drawing. It
  computes values; it does not draw sheets.
- **Execute VBA/macros.** VBA is parsed only far enough to extract formula
  dependencies; no macro *runs*.
- **Resolve external workbooks.** Formulas that link to *other* files return
  `#UNSUPPORTED!` — "no network, no filesystem from formulas" is a hard rule, so
  there is nothing to fetch the linked workbook with. (This, not engine error,
  dominates the "strict" miss count.)
- **Reach the network / disk from formulas.** `WEBSERVICE`, `RTD`,
  `STOCKHISTORY` return `#BLOCKED!`; `RAND*`/`NOW`/`TODAY` are seedable/injectable
  in the sandbox.
- **Support non-en-US locale semantics**, or a Google Sheets compatibility mode.
- **Write a full-fidelity file back.** v1 writes computed values into the
  original file's cached-value slots only; all other bytes are untouched. In
  particular, dynamic-array **spills are compute-only** and are not written back.

When a workbook uses one of these, you get a distinguishable error value plus a
diagnostic — never a silently wrong number.
