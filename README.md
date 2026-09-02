# Recalc

**Verify spreadsheet recalculations locally — without uploading workbooks.**

Recalc is a headless spreadsheet calculation engine and local verification
toolkit in Rust. It opens an `.xlsx`/`.xlsm` file, rebuilds the formula
dependency graph, and reports computed values together with explicit evidence
labels. It does not require Excel or upload workbook contents.

The one property that matters: **never silently wrong.** When Recalc cannot
safely compute a construct, it returns a distinguishable sentinel such as
`#UNSUPPORTED!` or `#BLOCKED!` and records a diagnostic. A cached-value match is
labelled as cached evidence; it is not presented as an independent Excel-oracle
result.

Apache-2.0. Pure Rust core, with thin bindings for Python, Node.js, and WASM.

## Why

If you need Excel's answer on a server, at volume, you have had two bad options:
run a farm of licensed Excel instances on Windows (slow, fragile, expensive to
babysit), or use an open-source calc library that reimplements a slice of Excel
and then quietly diverges in the corners — a date one day off, an error type
swapped, a lookup returning `0` where Excel returns `#N/A` — without telling you.

Recalc is the third option: it computes like Excel *and* is explicit about the
moments it can't. A wrong number you can see is a footnote; a wrong number you
can't see is a liability. Recalc is built so you never ship the second kind.

## Install

The public Rust entry point is the `recalc-engine` crate — a thin facade over
the internal `xl-*` crates. Depend on it, not on the `xl-*` crates directly.

```toml
# Cargo.toml
[dependencies]
recalc-engine = "0.1"
```

Language bindings are built from this source tree (availability is platform and
release specific; check the tagged artifact before adding a dependency):

```sh
pip install recalc-engine      # Python (import name: recalc)
npm install recalc-engine      # Node.js native addon
npm install recalc-engine-wasm # WASM (browser / edge)
```

To build from source instead, clone this repository and `cargo build
--workspace` (a plain build links zero binding dependencies). Full build,
binding, and packaging notes are in [`docs/getting-started.md`](docs/getting-started.md).

## Quickstart

### Rust

```rust
use recalc_engine::{Engine, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workbook = recalc_engine::open("model.xlsx")?; // or from_bytes(&bytes)?
    let mut engine = Engine::load(workbook);

    engine.recalc(); // recompute in dependency order

    let sid = engine.sheet_id("Sheet1").expect("sheet exists");
    match engine.value(sid, 1, 1) {                 // 0-based (row, col) → B2
        Some(Value::Number(n)) => println!("value: {n}"),
        Some(Value::Error(kind)) => println!("flagged: {}", kind.as_str()),
        Some(other) => println!("value: {other:?}"),
        None => println!("blank / never populated"),
    }

    // The fidelity report: one record per cell the engine refused to compute.
    for d in engine.diagnostics() {
        println!("{:?}: {}", d.kind, d.message);
    }
    Ok(())
}
```

### Python

```python
import recalc

wb = recalc.open("model.xlsx")     # or recalc.open_bytes(raw_bytes)
wb.recalc()

v = wb.cell("Sheet1", "B2")        # or wb.value("Sheet1", 1, 1) — 0-based
if isinstance(v, recalc.CellError):
    print("flagged:", v.code)      # "#UNSUPPORTED!", "#DIV/0!", ...
else:
    print("value:", v)

for d in wb.diagnostics():         # everything it refused to compute
    print(d.sheet, d.row, d.col, d.kind, d.message)
```

Reading a cell *before* `recalc()` returns the file's cached value (whatever
Excel last stored), not a value Recalc computed. Call `recalc()` first.

See [`docs/getting-started.md`](docs/getting-started.md) for the Node and WASM
quickstarts and the full API surface (identical operation set across all
bindings).

## Verify a workbook locally

The Verify v1 command ships as the `recalc` binary of the `xl-bench` crate in
this tree. Build and run it from a clone:

```sh
cargo run --release -p xl-bench --bin recalc -- verify OUTPUT.xlsx \
  --policy recalc-policy.toml --json report.json
```

The contract is specified in
[`docs/specs/recalc-verify-v1.md`](docs/specs/recalc-verify-v1.md); the report
and receipt schemas sit beside it. `xl-bench/tests/fixtures/verify-policy.toml`
is a working policy file to start from.

Exit `0` means PASS, `1` means a measured FAIL, and `2` means FALLBACK because
the requested evidence or a safe calculation was unavailable. Reports follow
the versioned `recalc.verify.report/v1` schema. Independent pinned-Excel
evidence and hosted batch service availability are separate, explicitly
labelled workstreams.

## Fidelity, measured

Compatibility claims are evidence-scoped. Recalc reports whether a value was
computed, matched a stored value, matched a supplied baseline, or was refused.
The current CLI supports stored cached values, a local `--baseline`, and a
supplied result with an explicit `--excel-build` label. The label records caller
provenance; it is not inferred by Recalc. Do not infer pinned-Excel agreement
from a local run.

## Scope — what it is, and isn't

Recalc is a **calculation** engine, on purpose. v1 does not:

- render, lay out, or draw charts (it computes values, it does not draw sheets);
- *execute* VBA/macros (they are parsed only far enough to trace dependencies);
- resolve external workbooks, or reach the network/disk from a formula
  (`WEBSERVICE`/`RTD`/`STOCKHISTORY` return `#BLOCKED!`);
- support non-en-US locale semantics, or a Google Sheets compatibility mode;
- write a full-fidelity file back (v1 writes computed values into the original
  file's cached-value slots only).

When a workbook uses one of these, you get a distinguishable error value plus a
diagnostic — never a silently wrong number.

## Repository layout

| Crate | Role |
|---|---|
| `recalc-engine` | Public Rust facade — the crate you depend on. |
| `xl-io` | OOXML (`.xlsx`/`.xlsm`) reader, hardened against malformed input. |
| `xl-ast` | Formula lexer + parser (A1/R1C1, 3-D refs, structured refs, `LET`/`LAMBDA`). |
| `xl-value` | Value model and coercion tables — the shared contract. |
| `xl-graph` | Dependency graph: dirty-marking, topological recalc, cycle detection. |
| `xl-fn` | Function library, one module per function. |
| `xl-engine` | Orchestration: load → graph → recalc → report. |
| `xl-ffi` | Thin Python / Node / WASM bindings (off-by-default features). |
| `xl-bench` | Conformance harness (corpus and oracle supplied by the user). |

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). The short version: never guess Excel
semantics, never be silently wrong, clean-room only, and keep the dependency
set minimal.

## License

Apache License, Version 2.0 — see [`LICENSE`](LICENSE).

---

Microsoft and Excel are trademarks of the Microsoft group of companies. Recalc
is independent and not affiliated with, sponsored by, or endorsed by Microsoft.
References to Excel are nominative.
