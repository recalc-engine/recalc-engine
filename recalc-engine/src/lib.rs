//! # Recalc — a headless, Excel-compatible recalculation engine
//!
//! `recalc-engine` is the public Rust entry point for Recalc: open an
//! `.xlsx`/`.xlsm` workbook, build its formula dependency graph, and
//! recalculate exactly as Microsoft Excel would — same values, same errors,
//! same quirks — with no UI and no Excel installation.
//!
//! This crate is a thin **facade**. The engine is split across several internal
//! crates (`xl-io` for the OOXML read path, `xl-engine` for orchestration, and
//! the `xl-value`/`xl-ast`/`xl-graph`/`xl-fn` crates beneath them). Those are
//! published only so this facade can depend on them; **depend on
//! `recalc-engine`, not on the `xl-*` crates directly** — their split and names
//! are an implementation detail that may change, whereas this facade is the
//! stable surface.
//!
//! ## Quickstart
//!
//! ```no_run
//! use recalc_engine::{Engine, Value};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Load the workbook, then wrap it in the engine.
//! let workbook = recalc_engine::open("model.xlsx")?; // or from_bytes(&bytes)?
//! let mut engine = Engine::load(workbook);
//!
//! // Recompute in dependency order (reading a cell before this returns the
//! // file's cached value, not a freshly computed one).
//! engine.recalc();
//!
//! let sid = engine.sheet_id("Sheet1").expect("sheet exists");
//! match engine.value(sid, 1, 1) {                 // 0-based (row, col) → B2
//!     Some(Value::Number(n)) => println!("value: {n}"),
//!     Some(Value::Error(kind)) => println!("flagged: {}", kind.as_str()),
//!     Some(other) => println!("value: {other:?}"),
//!     None => println!("blank / never populated"),
//! }
//!
//! // "Never silently wrong": everything the engine refused to compute.
//! for d in engine.diagnostics() {
//!     println!("{:?}: {}", d.kind, d.message);
//! }
//! # Ok(()) }
//! ```
//!
//! ## What you get
//!
//! * **Loading** — [`open`], [`open_with_caps`], [`from_bytes`], and
//!   [`from_bytes_with_caps`], returning a [`Workbook`]. The `*_with_caps`
//!   variants take a [`Caps`] hardening budget (zip-bomb / size / depth caps);
//!   the plain variants use [`Caps::default`].
//! * **The engine** — [`Engine`], driven by [`Engine::load`],
//!   [`Engine::recalc`], [`Engine::value`], [`Engine::sheet_id`],
//!   [`Engine::diagnostics`], and friends.
//! * **The value model** — [`Value`] (`Number`/`Text`/`Bool`/`Error`/`Blank`/
//!   `Array`/`Ref`) plus the cell/sheet identifiers [`CellId`] and [`SheetId`].
//! * **Diagnostics** — [`Diagnostic`] and its machine-readable
//!   [`DiagnosticKind`]: every "refused to compute" record from the last
//!   recalc, so an unsupported construct is *distinguishable*, never silently
//!   wrong.
//!
//! For the full loaded-workbook model (sheets, cells, defined names, calc
//! settings, number formats) reach through the re-exported [`io`] module; for
//! anything the facade does not surface directly, the underlying [`engine`]
//! crate is re-exported whole.

// ---------------------------------------------------------------------------
// Loading (the OOXML read path).
// ---------------------------------------------------------------------------

pub use xl_io::{from_bytes, from_bytes_with_caps, open, open_with_caps};

/// Loaded-workbook model and hardening caps (from the OOXML read path).
pub use xl_io::{
    CalcMode, CalcSettings, CapKind, Caps, Cell, DateSystem, DefinedName, FormulaKind, IoError,
    NumFmtId, RawFormula, Sheet, Workbook, WorkbookFlags,
};

// ---------------------------------------------------------------------------
// The engine, its value model, and diagnostics.
// ---------------------------------------------------------------------------

pub use xl_engine::{
    CellId, CellInput, Diagnostic, DiagnosticKind, Engine, RecalcResult, SheetId, Value,
};

// ---------------------------------------------------------------------------
// Escape hatches: the underlying crates in full, for advanced use the curated
// re-exports above do not cover. Prefer the items above; these are the door to
// everything else without adding the `xl-*` crates to your own manifest.
// ---------------------------------------------------------------------------

/// The orchestration crate (`xl-engine`) in full.
pub use xl_engine as engine;

/// The OOXML read-path crate (`xl-io`) in full.
pub use xl_io as io;
