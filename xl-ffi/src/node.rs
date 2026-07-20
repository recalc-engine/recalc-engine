//! The Recalc **Node** binding (napi-rs).
//!
//! Compiled only under `--features node`; the default `cargo build --workspace`
//! never touches napi (approval condition 3). It mirrors the Python binding
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
//! | `Array(..)`   | `Array<Array<...>>`, row-major (elements recurse) |
//! | `Error(kind)` | [`CellError`] with `.code == kind.as_str()` |
//! | `Ref(..)`     | [`CellError`] with `.code == "#UNSUPPORTED!"` (BC-9) |
//! | `Lambda(..)`  | [`CellError`] with `.code == "#UNSUPPORTED!"` (RFC-0013 §4 / BC-11) |
//!
//! An **error value is a distinct class** ([`CellError`]), never the bare
//! `"#DIV/0!"` string and never a thrown `Error`, so a cell whose literal
//! *text* is `"#DIV/0!"` is never confused with a cell that actually *errored*.
//! Clients branch with `v instanceof CellError`. In-cell computation failures
//! are **never** exceptions; only API misuse (bad bytes/path, unknown sheet,
//! malformed A1) throws a JS `Error` — condition-for-condition congruent with
//! the Python binding's `IOError`/`KeyError`/`ValueError` (the exception
//! *classes* cannot be, and are not, mirrored — §3.3 host-forced divergence).
//!
//! # Marshaling is exhaustive (RFC-0013 BC-11)
//! [`value_to_node`] matches `Value` with **no `_` arm**; a new `Value` variant
//! is a compile break here, forcing a conscious mapping decision. (The
//! `Lambda` variant of RFC-0012 BC-6 was mapped exactly this way at
//! integration — the exhaustive match broke, and the `Lambda → "#UNSUPPORTED!"`
//! row was added consciously, as the design intends.)
//!
//! # No hand-written `unsafe` (approval condition 8)
//! Every foreign value is built through **safe** napi APIs only —
//! [`Env::create_array`], [`Array::set`]/[`Array::get`],
//! `JavaScriptClassExt::into_instance`, `JsValue::into_unknown` — never the
//! `unsafe` `ToNapiValue::to_napi_value` extension point.
//!
//! # Provenance
//! Implemented for the M2 lane-8 Node-binding task; napi/napi-derive/napi-build
//! approved for `xl-ffi` only (the dependency-approval policy, 2026-07-14).
//! Mirrors `xl-ffi/src/python.rs` (the M1 precedent). The Node **API surface**
//! remains a human checkpoint before release (napi-rs condition 10).

use napi::bindgen_prelude::*;
use napi_derive::napi;

use xl_engine::{DiagnosticKind, Engine, Value};

use crate::a1::parse_a1;

/// The assertable surface-version constant (unified surface §1.2c). Integer `1`
/// denotes exactly the operation set of the unified binding API surface
/// (2026-07-15); bumped in lockstep across all bindings on any additive change.
#[napi]
pub const SURFACE_VERSION: u32 = 1;

/// A distinguishable spreadsheet **error value** surfaced to Node.
///
/// Returned in place of a `string` for any [`Value::Error`] (and the
/// unsupported [`Value::Ref`] case) so error results are never mistaken for a
/// cell that literally *contains* the text `"#DIV/0!"`. Branch on it with
/// `v instanceof CellError` and read [`code`](CellError::code) for the exact
/// Excel error string. Never thrown.
#[napi]
pub struct CellError {
    code: String,
}

#[napi]
impl CellError {
    /// The exact Excel error string, e.g. `"#DIV/0!"`, `"#N/A"`, or the
    /// Recalc-specific `"#UNSUPPORTED!"` / `"#BLOCKED!"` sentinels.
    #[napi(getter)]
    pub fn code(&self) -> String {
        self.code.clone()
    }
}

/// A single "the engine refused to compute this" record, surfaced to Node.
///
/// One entry per [`xl_engine::Diagnostic`], the cell located by sheet name and
/// 0-based `(row, col)`, a stable machine-readable [`kind`](Diagnostic::kind),
/// and the human-readable message. Field names are identical across all three
/// bindings.
#[napi]
pub struct Diagnostic {
    sheet: String,
    row: u32,
    col: u32,
    kind: String,
    message: String,
}

#[napi]
impl Diagnostic {
    /// The display name of the sheet the diagnostic occurred on.
    #[napi(getter)]
    pub fn sheet(&self) -> String {
        self.sheet.clone()
    }

    /// 0-based row index of the offending cell.
    #[napi(getter)]
    pub fn row(&self) -> u32 {
        self.row
    }

    /// 0-based column index of the offending cell.
    #[napi(getter)]
    pub fn col(&self) -> u32 {
        self.col
    }

    /// Stable category string: one of `"ParseError"`, `"UnknownFunction"`,
    /// `"ArityError"`, `"UnsupportedConstruct"`, `"CircularReference"`.
    #[napi(getter)]
    pub fn kind(&self) -> String {
        self.kind.clone()
    }

    /// Human-readable explanation.
    #[napi(getter)]
    pub fn message(&self) -> String {
        self.message.clone()
    }
}

/// A loaded workbook: parse → dependency graph → recalc, exposed to Node.
///
/// Obtain one with [`open`] (from a path) or [`open_bytes`] (from a
/// `Buffer`/`Uint8Array`). Wraps an owned [`xl_engine::Engine`].
///
/// **Threading (v1 limitation, unified surface §4):** a `Workbook` is bound to
/// the JS thread/env that created it and is not shared across Worker threads
/// (each Worker instantiates the addon independently). There is no async
/// surface in M2: [`recalc`](Workbook::recalc) is synchronous and blocks the
/// event loop for its duration.
#[napi]
pub struct Workbook {
    engine: Engine,
}

#[napi]
impl Workbook {
    /// Recalculate the whole workbook in dependency order (idempotent).
    ///
    /// Until this is called, [`value`](Workbook::value) returns the file's
    /// cached values (whatever Excel last stored), not values Recalc computed.
    /// Refusals known at load time are already visible via
    /// [`diagnostics`](Workbook::diagnostics) before recalc.
    #[napi]
    pub fn recalc(&mut self) {
        self.engine.recalc();
    }

    /// The workbook's sheet display names, in tab order.
    #[napi(js_name = "sheetNames")]
    pub fn sheet_names(&self) -> Vec<String> {
        self.engine.sheet_names()
    }

    /// The value of a cell by sheet name and **0-based** `(row, col)`.
    ///
    /// Returns a native JS value per the module's mapping table. A never-
    /// populated cell returns `null`. Throws a JS `Error` if `sheet` names no
    /// sheet in the workbook.
    ///
    /// **Before [`recalc`](Workbook::recalc)** this is the file's cached value.
    /// Cells Recalc cannot faithfully compute surface as [`CellError`] and via
    /// [`diagnostics`](Workbook::diagnostics) — never as a silently-wrong
    /// number.
    #[napi(ts_return_type = "number | string | boolean | null | CellError | any[]")]
    pub fn value(&self, env: Env, sheet: String, row: u32, col: u32) -> Result<Unknown<'static>> {
        let sid = self.engine.sheet_id(&sheet).ok_or_else(|| {
            Error::new(Status::GenericFailure, format!("no sheet named {sheet:?}"))
        })?;
        match self.engine.value(sid, row, col) {
            Some(v) => value_to_node(&env, v),
            None => to_unknown(&env, Null),
        }
    }

    /// Like [`value`](Workbook::value), but locating the cell with an A1 address
    /// (e.g. `"B2"`) parsed relative to `sheet`.
    ///
    /// Throws a JS `Error` for a malformed A1 address or an unknown sheet name.
    #[napi(ts_return_type = "number | string | boolean | null | CellError | any[]")]
    pub fn cell(&self, env: Env, sheet: String, a1: String) -> Result<Unknown<'static>> {
        let (row, col) =
            parse_a1(&a1).map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
        self.value(env, sheet, row, col)
    }

    /// Every "refused to compute" record known so far, as [`Diagnostic`]
    /// objects. Load-time refusals are present immediately after
    /// [`open`]/[`open_bytes`]; evaluation-time refusals are added by
    /// [`recalc`](Workbook::recalc).
    #[napi]
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
    /// anchor (BC-10: obstructed anchor, spilled-into, plain, or non-formula
    /// cell → `null`; a 1×1 dynamic-array anchor → its 1×1 array).
    ///
    /// Throws a JS `Error` for a malformed A1 address or an unknown sheet name
    /// — the same misuse errors as [`cell`](Workbook::cell).
    ///
    /// Routed through [`Engine::spill_region`](xl_engine::Engine::spill_region),
    /// the single read-only anchor→region query (RFC-0013 §3) shared by all
    /// three bindings. `Some(Value::Array)` marshals as a row-major 2-D JS array
    /// via the canonical [`value_to_node`]; `None` (non-anchor, BC-10) → JS
    /// `null`. The engine maintains a live spill-anchor registry (updated on
    /// every recalc), so a dynamic-array anchor returns its reconstructed region
    /// and a non-anchor returns `null`; spills are compute-only in v1 (queryable
    /// here but not written back to the file).
    #[napi(js_name = "spillRegion", ts_return_type = "any[] | null")]
    pub fn spill_region(&self, env: Env, sheet: String, a1: String) -> Result<Unknown<'static>> {
        let sid = self.engine.sheet_id(&sheet).ok_or_else(|| {
            Error::new(Status::GenericFailure, format!("no sheet named {sheet:?}"))
        })?;
        let (row, col) =
            parse_a1(&a1).map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
        match self.engine.spill_region(sid, row, col) {
            Some(region) => value_to_node(&env, &region),
            None => to_unknown(&env, Null),
        }
    }
}

/// Open a workbook from a filesystem path. Throws a JS `Error` on any
/// load/parse failure (bad zip, malformed XML, hardening-cap violation, …).
#[napi]
pub fn open(path: String) -> Result<Workbook> {
    let workbook =
        xl_io::open(&path).map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
    Ok(Workbook {
        engine: Engine::load(workbook),
    })
}

/// Open a workbook from in-memory bytes (the raw `.xlsx`/`.xlsm` zip).
///
/// Accepts a `Buffer` or a `Uint8Array` (a `Buffer` is a `Uint8Array` subclass
/// — idiom, not divergence, §3.3). Throws a JS `Error` on any load/parse
/// failure.
#[napi(js_name = "openBytes")]
pub fn open_bytes(data: Either<Buffer, Uint8Array>) -> Result<Workbook> {
    let bytes: &[u8] = match &data {
        Either::A(buffer) => buffer.as_ref(),
        Either::B(array) => array.as_ref(),
    };
    let workbook =
        xl_io::from_bytes(bytes).map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
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

/// Convert one [`Value`] into the corresponding JS value per RFC-0013 §2.
///
/// **Exhaustive, no `_` arm** (BC-11): a new `Value` variant is a compile break.
/// Arrays recurse (row-major `Array<Array<...>>`).
fn value_to_node(env: &Env, value: &Value) -> Result<Unknown<'static>> {
    match value {
        Value::Number(n) => to_unknown(env, *n),
        Value::Text(t) => to_unknown(env, t.as_str().to_string()),
        Value::Bool(b) => to_unknown(env, *b),
        Value::Blank => to_unknown(env, Null),
        Value::Error(kind) => to_unknown(env, cell_error(kind.as_str())),
        Value::Array(arr) => {
            // Row-major 2-D JS array, each element recursing (safe: `Array::set`
            // on a bindgen `Array`, then bridged to `Unknown`). Inlined —
            // rather than a typed helper — so the marshal need not name
            // `xl_value::Array` (xl-ffi does not depend on `xl-value`),
            // mirroring `python.rs`.
            let mut outer = env.create_array(arr.rows() as u32)?;
            for r in 0..arr.rows() {
                let mut row = env.create_array(arr.cols() as u32)?;
                for c in 0..arr.cols() {
                    // `get` is in-bounds for r < rows, c < cols by construction.
                    let elem = arr
                        .get(r, c)
                        .expect("array index within its own rows/cols is always present");
                    row.set(c as u32, value_to_node(env, elem)?)?;
                }
                outer.set(r as u32, row)?;
            }
            to_unknown(env, outer)
        }
        // A bare `Ref` reaching a cell value is not something M2 resolves; keep
        // it distinguishable and non-crashing rather than guessing (BC-9).
        Value::Ref(_) => to_unknown(env, cell_error("#UNSUPPORTED!")),
        // A lambda is engine-internal and never exposed to a host language
        // (RFC-0012 BC-6 "born refusing"; RFC-0013 §4 / BC-11): refuse with the
        // distinguishable `#UNSUPPORTED!` CellError (NOT a silent `null`, which
        // would look like a blank cell).
        Value::Lambda(_) => to_unknown(env, cell_error("#UNSUPPORTED!")),
    }
}

/// A [`CellError`] value carrying `code`. Passed to [`to_unknown`], where
/// napi-derive's generated `ToNapiValue` for the class materializes a JS
/// `CellError` **instance** (so `v instanceof CellError` holds).
fn cell_error(code: &str) -> CellError {
    CellError {
        code: code.to_string(),
    }
}

/// Bridge any `ToNapiValue` value — a scalar (number/string/boolean/null), a
/// [`CellError`], or a built `Array` — into an owned [`Unknown`], using only
/// **safe** APIs: stage it as the sole element of a fresh JS array, then read
/// it back. The returned `Unknown` does not borrow `env` (unlike
/// `JsValue::into_unknown`), so it can be returned from the `#[napi]` method.
///
/// (The napi 3.x minimal feature set — no `compat-mode` — offers no direct
/// value→`Unknown` constructor without the `unsafe`
/// `ToNapiValue::to_napi_value`, which the no-hand-written-`unsafe` rule
/// forbids. The one-element temporary array is the price of staying safe.)
fn to_unknown<T: ToNapiValue>(env: &Env, value: T) -> Result<Unknown<'static>> {
    let mut holder = env.create_array(0)?;
    holder.insert(value)?;
    Ok(holder
        .get::<Unknown<'static>>(0)?
        .expect("element 0 is present immediately after insert"))
}
