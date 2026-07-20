//! The Recalc **WASM** binding (wasm-bindgen).
//!
//! Compiled only under `--features wasm`; the default `cargo build --workspace`
//! never touches wasm-bindgen. It mirrors the Python binding
//! ([`crate::python`]) using JS camelCase idiom, per the unified binding API
//! surface (2026-07-15): the same conceptual operations, the same one value
//! mapping (RFC-0013 §2), the same congruence rules.
//!
//! # Value → JS mapping (RFC-0013 §2 — the canonical table)
//! | Recalc `Value` | JS value |
//! |---|---|
//! | `Number(f64)` | `number` |
//! | `Text(..)`    | `string` |
//! | `Bool(bool)`  | `boolean` |
//! | `Blank`       | `null` (distinct from `Text("")` → `""`) |
//! | `Array(..)`   | nested `Array`, row-major (elements recurse) |
//! | `Error(kind)` | [`CellError`] with `.code == kind.as_str()` |
//! | `Ref(..)`     | [`CellError`] with `.code == "#UNSUPPORTED!"` (BC-9) |
//! | `Lambda(..)`  | [`CellError`] with `.code == "#UNSUPPORTED!"` (RFC-0013 §4 / BC-11) |
//!
//! An **error value is a distinct class** ([`CellError`]), never a bare string
//! and never a thrown `Error`; clients branch with `v instanceof CellError`.
//! In-cell computation failures are **never** exceptions; only API misuse
//! (bad bytes, unknown sheet, malformed A1) throws a JS `Error` — congruent
//! with the Python binding, whose fine-grained exception *classes* are a
//! host-forced divergence (§3.3), not mirrored.
//!
//! # Host-forced divergences (unified surface §3.3)
//! * **No `open(path)`** — the browser sandbox has no filesystem; [`open_bytes`]
//!   is the sole loader (also consistent with the engine's no-fs posture).
//! * **`surfaceVersion()` is a function**, not a module constant — wasm-bindgen
//!   exports functions, not constants.
//! * **`free()`** exists on every exported class (wasm-bindgen deterministic
//!   destructor); calling any method after `free()` throws — misuse, not a
//!   value.
//!
//! # Threading (unified surface §4)
//! Single-threaded by construction: no threads, no network, and the wasm build
//! **never** enables the parallel/rayon feature (wasm-bindgen dep condition 4).
//! [`recalc`](Workbook::recalc) blocks the JS thread; a caller wanting a
//! responsive tab runs the module in a Web Worker (a deployment pattern, not a
//! surface feature). No JS-callback or fetch surface is exposed into the engine.
//!
//! # Marshaling is exhaustive (RFC-0013 BC-11)
//! [`value_to_js`] matches `Value` with **no `_` arm**. (This worktree's
//! `Value` predates the `Lambda` variant of RFC-0012 BC-6; on a tree that has
//! it, the exhaustive match breaks and the integrator adds the
//! `Lambda → "#UNSUPPORTED!"` row — the intended design.) Nested arrays are
//! built from `Vec<JsValue>` via `JsValue::from`, using **no** `js-sys`
//! (unapproved): wasm-bindgen alone renders `Vec<JsValue>` as a JS `Array`.
//!
//! # Provenance
//! Implemented for the M2 lane-8 WASM-binding task; wasm-bindgen approved for
//! `xl-ffi` only (the dependency-approval policy, 2026-07-14). Mirrors
//! `xl-ffi/src/python.rs` (the M1 precedent). The WASM **API surface** remains
//! a human checkpoint before release.

use wasm_bindgen::prelude::*;

use xl_engine::{DiagnosticKind, Engine, Value};

use crate::a1::parse_a1;

/// The assertable surface-version integer (unified surface §1.2c). Returns `1`,
/// denoting exactly the operation set of the unified binding API surface
/// (2026-07-15). Exposed as a **function** (`surfaceVersion()`) because
/// wasm-bindgen exports functions, not module constants (§3.3).
#[wasm_bindgen(js_name = surfaceVersion)]
pub fn surface_version() -> u32 {
    1
}

/// A distinguishable spreadsheet **error value** surfaced to WASM.
///
/// Returned in place of a `string` for any [`Value::Error`] (and the
/// unsupported [`Value::Ref`] case) so error results are never mistaken for a
/// cell that literally *contains* the text `"#DIV/0!"`. Branch with
/// `v instanceof CellError` and read [`code`](CellError::code). Never thrown.
#[wasm_bindgen]
pub struct CellError {
    code: String,
}

#[wasm_bindgen]
impl CellError {
    /// The exact Excel error string, e.g. `"#DIV/0!"`, `"#N/A"`, or the
    /// Recalc-specific `"#UNSUPPORTED!"` / `"#BLOCKED!"` sentinels.
    #[wasm_bindgen(getter)]
    pub fn code(&self) -> String {
        self.code.clone()
    }
}

/// A single "the engine refused to compute this" record, surfaced to WASM.
///
/// One entry per [`xl_engine::Diagnostic`], the cell located by sheet name and
/// 0-based `(row, col)`, a stable machine-readable [`kind`](Diagnostic::kind),
/// and the human-readable message. Field names are identical across all three
/// bindings.
#[wasm_bindgen]
pub struct Diagnostic {
    sheet: String,
    row: u32,
    col: u32,
    kind: String,
    message: String,
}

#[wasm_bindgen]
impl Diagnostic {
    /// The display name of the sheet the diagnostic occurred on.
    #[wasm_bindgen(getter)]
    pub fn sheet(&self) -> String {
        self.sheet.clone()
    }

    /// 0-based row index of the offending cell.
    #[wasm_bindgen(getter)]
    pub fn row(&self) -> u32 {
        self.row
    }

    /// 0-based column index of the offending cell.
    #[wasm_bindgen(getter)]
    pub fn col(&self) -> u32 {
        self.col
    }

    /// Stable category string: one of `"ParseError"`, `"UnknownFunction"`,
    /// `"ArityError"`, `"UnsupportedConstruct"`, `"CircularReference"`.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        self.kind.clone()
    }

    /// Human-readable explanation.
    #[wasm_bindgen(getter)]
    pub fn message(&self) -> String {
        self.message.clone()
    }
}

/// A loaded workbook: parse → dependency graph → recalc, exposed to WASM.
///
/// Obtain one with [`open_bytes`] (there is no path loader in the browser
/// sandbox). Wraps an owned [`xl_engine::Engine`]. wasm-bindgen generates a
/// `free()` method; call it to release the module's linear-memory instance
/// where no `FinalizationRegistry` reclaims it.
#[wasm_bindgen]
pub struct Workbook {
    engine: Engine,
}

#[wasm_bindgen]
impl Workbook {
    /// Recalculate the whole workbook in dependency order (idempotent).
    ///
    /// Until this is called, [`value`](Workbook::value) returns the file's
    /// cached values, not values Recalc computed. Refusals known at load time
    /// are already visible via [`diagnostics`](Workbook::diagnostics).
    #[wasm_bindgen]
    pub fn recalc(&mut self) {
        self.engine.recalc();
    }

    /// The workbook's sheet display names, in tab order.
    #[wasm_bindgen(js_name = sheetNames)]
    pub fn sheet_names(&self) -> Vec<String> {
        self.engine.sheet_names()
    }

    /// The value of a cell by sheet name and **0-based** `(row, col)`.
    ///
    /// Returns a native JS value per the module's mapping table. A never-
    /// populated cell returns `null`. Throws a JS `Error` if `sheet` names no
    /// sheet. **Before [`recalc`](Workbook::recalc)** this is the file's cached
    /// value; cells Recalc cannot compute surface as [`CellError`].
    #[wasm_bindgen]
    pub fn value(&self, sheet: &str, row: u32, col: u32) -> Result<JsValue, JsError> {
        let sid = self
            .engine
            .sheet_id(sheet)
            .ok_or_else(|| JsError::new(&format!("no sheet named {sheet:?}")))?;
        Ok(match self.engine.value(sid, row, col) {
            Some(v) => value_to_js(v),
            None => JsValue::NULL,
        })
    }

    /// Like [`value`](Workbook::value), but locating the cell with an A1 address
    /// (e.g. `"B2"`). Throws a JS `Error` for a malformed A1 address or an
    /// unknown sheet name.
    #[wasm_bindgen]
    pub fn cell(&self, sheet: &str, a1: &str) -> Result<JsValue, JsError> {
        let (row, col) = parse_a1(a1).map_err(|e| JsError::new(&e.to_string()))?;
        self.value(sheet, row, col)
    }

    /// Every "refused to compute" record known so far, as [`Diagnostic`]
    /// objects. Load-time refusals are present immediately after
    /// [`open_bytes`]; evaluation-time refusals are added by
    /// [`recalc`](Workbook::recalc).
    #[wasm_bindgen]
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let names = self.engine.sheet_names();
        self.engine
            .diagnostics()
            .into_iter()
            .map(|d| {
                let idx = d.cell.sheet.0 as usize;
                let sheet = names.get(idx).cloned().unwrap_or_else(|| format!("#{idx}"));
                Diagnostic {
                    sheet,
                    row: d.cell.row,
                    col: d.cell.col,
                    kind: kind_str(d.kind).to_string(),
                    message: d.message.clone(),
                }
            })
            .collect()
    }

    /// The spill region anchored at `a1` (the `A1#` surface, RFC-0013 §3), as a
    /// row-major 2-D array — or `null` if the addressed cell is not a spill
    /// anchor (BC-10). Throws a JS `Error` for a malformed A1 address or an
    /// unknown sheet name — the same misuse errors as [`cell`](Workbook::cell).
    ///
    /// Routed through [`Engine::spill_region`](xl_engine::Engine::spill_region),
    /// the single read-only anchor→region query (RFC-0013 §3) shared by all
    /// three bindings. `Some(Value::Array)` marshals as a row-major 2-D JS array
    /// via the canonical [`value_to_js`]; `None` (non-anchor, BC-10) → JS
    /// `null`. The engine maintains a live spill-anchor registry (updated on
    /// every recalc), so a dynamic-array anchor returns its reconstructed region
    /// and a non-anchor returns `null`; spills are compute-only in v1 (queryable
    /// here but not written back to the file).
    #[wasm_bindgen(js_name = spillRegion)]
    pub fn spill_region(&self, sheet: &str, a1: &str) -> Result<JsValue, JsError> {
        let sid = self
            .engine
            .sheet_id(sheet)
            .ok_or_else(|| JsError::new(&format!("no sheet named {sheet:?}")))?;
        let (row, col) = parse_a1(a1).map_err(|e| JsError::new(&e.to_string()))?;
        Ok(match self.engine.spill_region(sid, row, col) {
            Some(region) => value_to_js(&region),
            None => JsValue::NULL,
        })
    }
}

/// Open a workbook from in-memory bytes (the raw `.xlsx`/`.xlsm` zip). Accepts a
/// `Uint8Array`. Throws a JS `Error` on any load/parse failure. This is the
/// sole loader — there is no `open(path)` in the browser sandbox (§3.3).
#[wasm_bindgen(js_name = openBytes)]
pub fn open_bytes(data: &[u8]) -> Result<Workbook, JsError> {
    let workbook = xl_io::from_bytes(data).map_err(|e| JsError::new(&e.to_string()))?;
    Ok(Workbook {
        engine: Engine::load(workbook),
    })
}

// ----- internals ---------------------------------------------------------

/// The stable string form of a [`DiagnosticKind`] (mirrors the enum; a total
/// match so a new variant is a compile error here, not a silent gap).
fn kind_str(kind: DiagnosticKind) -> &'static str {
    match kind {
        DiagnosticKind::ParseError => "ParseError",
        DiagnosticKind::UnknownFunction => "UnknownFunction",
        DiagnosticKind::ArityError => "ArityError",
        DiagnosticKind::UnsupportedConstruct => "UnsupportedConstruct",
        DiagnosticKind::CircularReference => "CircularReference",
    }
}

/// Convert one [`Value`] into the corresponding [`JsValue`] per RFC-0013 §2.
///
/// **Exhaustive, no `_` arm** (BC-11): a new `Value` variant is a compile break.
/// Arrays recurse into a row-major nested `Array`, built entirely with
/// wasm-bindgen (no `js-sys`): a `Vec<JsValue>` renders as a JS `Array`.
fn value_to_js(value: &Value) -> JsValue {
    match value {
        Value::Number(n) => JsValue::from_f64(*n),
        Value::Text(t) => JsValue::from_str(t.as_str()),
        Value::Bool(b) => JsValue::from_bool(*b),
        Value::Blank => JsValue::NULL,
        Value::Error(kind) => JsValue::from(CellError {
            code: kind.as_str().to_string(),
        }),
        Value::Array(arr) => {
            let mut rows: Vec<JsValue> = Vec::with_capacity(arr.rows());
            for r in 0..arr.rows() {
                let mut row: Vec<JsValue> = Vec::with_capacity(arr.cols());
                for c in 0..arr.cols() {
                    // `get` is in-bounds for r < rows, c < cols by construction.
                    let elem = arr
                        .get(r, c)
                        .expect("array index within its own rows/cols is always present");
                    row.push(value_to_js(elem));
                }
                // `Vec<JsValue>` → a JS `Array` (nested), no js-sys needed.
                rows.push(JsValue::from(row));
            }
            JsValue::from(rows)
        }
        // A bare `Ref` reaching a cell value is not something M2 resolves; keep
        // it distinguishable and non-crashing rather than guessing (BC-9).
        Value::Ref(_) => JsValue::from(CellError {
            code: "#UNSUPPORTED!".to_string(),
        }),
        // A lambda is engine-internal and never exposed to a host language
        // (RFC-0012 BC-6 "born refusing"; RFC-0013 §4 / BC-11): refuse with the
        // distinguishable `#UNSUPPORTED!` CellError (NOT a silent `null`, which
        // would look like a blank cell).
        Value::Lambda(_) => JsValue::from(CellError {
            code: "#UNSUPPORTED!".to_string(),
        }),
    }
}
