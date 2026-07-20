# recalc-engine

A headless, bug-for-bug Excel-compatible spreadsheet recalculation engine, in
Rust. Open an `.xlsx`/`.xlsm` workbook, build its formula dependency graph, and
recalculate exactly as Microsoft Excel would — same values, same errors, same
quirks — with no UI and no Excel installation.

Two principles run through the whole engine:

- **Fidelity is a measured number.** Agreement with a pinned Excel build is
  measured on a corpus of real workbooks, not asserted.
- **Never silently wrong.** Anything the engine cannot compute returns a
  distinguishable error value and a diagnostic, never a guess.

## Install

```toml
[dependencies]
recalc-engine = "0.1"
```

Depend on `recalc-engine` — not on the internal `xl-*` crates it re-exports.
Those are published only so this facade can depend on them; their names and
split are an implementation detail.

## Usage

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

    for d in engine.diagnostics() {
        println!("{:?}: {}", d.kind, d.message);
    }
    Ok(())
}
```

Reading a cell *before* `recalc()` returns the file's cached value (whatever
Excel last stored), not a value this engine computed. Call `recalc()` first.

## Other languages

The same engine ships as a Python package (`pip install recalc-engine`, import
`recalc`) and a Node addon (`npm install recalc-engine`).

## License

Apache-2.0.
