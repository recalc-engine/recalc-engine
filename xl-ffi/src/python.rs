//! The `recalc` Python extension module (PyO3).
//!
//! Everything here is compiled only under `--features python`; the default
//! `cargo build --workspace` never touches pyo3 (approval condition 2). The
//! module wraps [`xl_engine::Engine`] behind a Python-friendly `Workbook`
//! class plus two loader functions, and marshals [`xl_value::Value`] into
//! native Python objects.
//!
//! # Value → Python mapping (approval condition: "never silently wrong")
//! | Recalc `Value` | Python object |
//! |---|---|
//! | `Number(f64)` | `float` |
//! | `Text(..)`    | `str` |
//! | `Bool(bool)`  | `bool` |
//! | `Blank`       | `None` |
//! | `Array(..)`   | `list[list[...]]`, row-major |
//! | `Error(kind)` | [`CellError`] with `.code == kind.as_str()` |
//! | `Ref(..)`     | [`CellError`] with `.code == "#UNSUPPORTED!"` |
//! | `Lambda(..)`  | [`CellError`] with `.code == "#UNSUPPORTED!"` (RFC-0013 §4 / BC-11) |
//!
//! An **error value is a distinct type**, not the bare `"#DIV/0!"` string, so
//! a cell whose literal *text* is `"#DIV/0!"` (a `str`) is never confused with
//! a cell that actually *errored* (a [`CellError`]). Callers branch with
//! `isinstance(v, recalc.CellError)`.
//!
//! # Provenance
//! Implemented for the M1 Python-binding task; pyo3 approved for `xl-ffi` only
//! (the dependency-approval policy, 2026-07-11). No hand-written
//! `unsafe` (see the crate-root note).

use pyo3::exceptions::{PyIOError, PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyFloat, PyList, PyString};

use xl_engine::{DiagnosticKind, Engine, Value};

use crate::a1::parse_a1;

/// A distinguishable spreadsheet **error value** surfaced to Python.
///
/// Returned in place of a `str` for any [`Value::Error`] (and for the
/// unsupported [`Value::Ref`] case) so error results are never mistaken for a
/// cell that literally *contains* the text `"#DIV/0!"`. Branch on it with
/// `isinstance(v, recalc.CellError)` and read [`CellError::code`] for the exact
/// Excel error string.
#[pyclass(frozen, module = "recalc", name = "CellError")]
struct CellError {
    /// The exact Excel error string, e.g. `"#DIV/0!"`, `"#N/A"`, or the
    /// Recalc-specific `"#UNSUPPORTED!"` / `"#BLOCKED!"` sentinels.
    #[pyo3(get)]
    code: String,
}

#[pymethods]
impl CellError {
    fn __repr__(&self) -> String {
        format!("CellError({:?})", self.code)
    }

    fn __str__(&self) -> String {
        self.code.clone()
    }
}

/// A single "the engine refused to compute this" record, surfaced to Python.
///
/// One entry per [`xl_engine::Diagnostic`], with the cell located by sheet
/// name and 0-based `(row, col)`, a stable machine-readable [`kind`](Self)
/// string, and the human-readable message.
#[pyclass(frozen, module = "recalc", name = "Diagnostic")]
struct Diagnostic {
    /// The display name of the sheet the diagnostic occurred on.
    #[pyo3(get)]
    sheet: String,
    /// 0-based row index of the offending cell.
    #[pyo3(get)]
    row: u32,
    /// 0-based column index of the offending cell.
    #[pyo3(get)]
    col: u32,
    /// Stable category string (see [`kind_str`]): one of `"ParseError"`,
    /// `"UnknownFunction"`, `"ArityError"`, `"UnsupportedConstruct"`,
    /// `"CircularReference"`.
    #[pyo3(get)]
    kind: String,
    /// Human-readable explanation.
    #[pyo3(get)]
    message: String,
}

#[pymethods]
impl Diagnostic {
    fn __repr__(&self) -> String {
        format!(
            "Diagnostic(sheet={:?}, row={}, col={}, kind={:?}, message={:?})",
            self.sheet, self.row, self.col, self.kind, self.message
        )
    }
}

/// A loaded workbook: parse → dependency graph → recalc, exposed to Python.
///
/// Obtain one with [`open`] (from a path) or [`open_bytes`] (from `bytes`).
/// Wraps an owned [`xl_engine::Engine`].
///
/// **Threading (v1 limitation):** the object is `unsendable` — it must be used
/// from the thread that created it; touching it from another Python thread
/// raises. Cross-thread use is out of scope for the v1 binding.
#[pyclass(unsendable, module = "recalc", name = "Workbook")]
struct Workbook {
    engine: Engine,
}

#[pymethods]
impl Workbook {
    /// Recalculate the whole workbook in dependency order (idempotent).
    ///
    /// Until this is called, [`value`](Self::value) returns the values the
    /// file last stored (Excel's cached values), not values Recalc computed.
    /// Call `recalc()` to compute fresh values. Refusals known at load time are
    /// already visible via [`diagnostics`](Self::diagnostics) before recalc.
    fn recalc(&mut self) {
        self.engine.recalc();
    }

    /// The workbook's sheet display names, in tab order.
    fn sheet_names(&self) -> Vec<String> {
        self.engine.sheet_names()
    }

    /// The value of a cell by sheet name and **0-based** `(row, col)`.
    ///
    /// Returns a native Python value per the module's mapping table. A cell
    /// with no stored value (a never-populated blank) returns `None`. Raises
    /// `KeyError` if `sheet` names no sheet in the workbook.
    ///
    /// **Before [`recalc`](Self::recalc)** this is the file's cached value
    /// (whatever Excel last stored), not a value Recalc computed; call
    /// `recalc()` first for fresh values. Either way, cells Recalc cannot
    /// faithfully compute surface as [`CellError`] and via
    /// [`diagnostics`](Self::diagnostics) — never as a silently-wrong number.
    fn value(&self, py: Python<'_>, sheet: &str, row: u32, col: u32) -> PyResult<Py<PyAny>> {
        let sid = self
            .engine
            .sheet_id(sheet)
            .ok_or_else(|| PyKeyError::new_err(format!("no sheet named {sheet:?}")))?;
        match self.engine.value(sid, row, col) {
            Some(v) => value_to_py(py, v),
            None => Ok(py.None()),
        }
    }

    /// Like [`value`](Self::value), but locating the cell with an A1 address
    /// (e.g. `"B2"`) parsed relative to `sheet`.
    ///
    /// Raises `ValueError` for a malformed A1 address and `KeyError` for an
    /// unknown sheet name.
    fn cell(&self, py: Python<'_>, sheet: &str, a1: &str) -> PyResult<Py<PyAny>> {
        let (row, col) = parse_a1(a1).map_err(|e| PyValueError::new_err(e.to_string()))?;
        self.value(py, sheet, row, col)
    }

    /// Every "refused to compute" record known so far, as [`Diagnostic`]
    /// objects (sheet, row, col, kind, message). Load-time refusals (parse
    /// errors, unsupported constructs) are present immediately after
    /// [`open`]/[`open_bytes`]; evaluation-time refusals (unknown functions,
    /// circular references) are added by [`recalc`](Self::recalc).
    fn diagnostics(&self) -> Vec<Diagnostic> {
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
    /// row-major `list[list[...]]` — or `None` if the addressed cell is not a
    /// dynamic-array spill anchor (BC-10). Routed through
    /// [`Engine::spill_region`](xl_engine::Engine::spill_region), the single
    /// read-only anchor→region query shared identically by all three bindings.
    ///
    /// Raises `KeyError` for an unknown sheet name and `ValueError` for a
    /// malformed A1 address. **Validation order is sheet-then-A1**, matching the
    /// Node/WASM bindings so the tri-binding behavior is identical when both
    /// arguments are bad (unified-surface invariant 10). The engine maintains a
    /// live spill-anchor registry (updated on every recalc), so a dynamic-array
    /// anchor returns its reconstructed region and a non-anchor returns `None`;
    /// spills are compute-only in v1 (the region is queryable here but not
    /// written back to the file).
    fn spill_region(&self, py: Python<'_>, sheet: &str, a1: &str) -> PyResult<Py<PyAny>> {
        let sid = self
            .engine
            .sheet_id(sheet)
            .ok_or_else(|| PyKeyError::new_err(format!("no sheet named {sheet:?}")))?;
        let (row, col) = parse_a1(a1).map_err(|e| PyValueError::new_err(e.to_string()))?;
        match self.engine.spill_region(sid, row, col) {
            Some(region) => value_to_py(py, &region),
            None => Ok(py.None()),
        }
    }
}

/// Open a workbook from a filesystem path. Raises `IOError` on any load/parse
/// failure (bad zip, malformed XML, hardening-cap violation, ...).
///
/// The returned [`Workbook`] holds the file's cached values; call
/// [`Workbook::recalc`] to compute fresh ones. Refusals detectable at load
/// (parse errors, unsupported constructs) are already reported by
/// [`Workbook::diagnostics`] before any recalc.
#[pyfunction]
fn open(path: &str) -> PyResult<Workbook> {
    let workbook = xl_io::open(path).map_err(io_err_to_py)?;
    Ok(Workbook {
        engine: Engine::load(workbook),
    })
}

/// Open a workbook from in-memory `bytes` (the raw `.xlsx`/`.xlsm` zip). Raises
/// `IOError` on any load/parse failure.
#[pyfunction]
fn open_bytes(data: &[u8]) -> PyResult<Workbook> {
    let workbook = xl_io::from_bytes(data).map_err(io_err_to_py)?;
    Ok(Workbook {
        engine: Engine::load(workbook),
    })
}

/// The `recalc` Python module: `open`, `open_bytes`, and the `Workbook`,
/// `CellError`, and `Diagnostic` classes.
#[pymodule]
fn recalc(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(open, m)?)?;
    m.add_function(wrap_pyfunction!(open_bytes, m)?)?;
    m.add_class::<Workbook>()?;
    m.add_class::<CellError>()?;
    m.add_class::<Diagnostic>()?;
    // The unified binding surface version (RFC-0013 §1.2c). The SAME integer
    // across Python/Node/WASM means the SAME operation set (invariant 10); it
    // moves only when the tri-binding surface changes in lockstep. Mirrors
    // Node's `SURFACE_VERSION` const and WASM's `surfaceVersion()`.
    m.add("SURFACE_VERSION", 1u32)?;
    Ok(())
}

// ----- internals ---------------------------------------------------------

/// Map an [`xl_io::IoError`] to a Python `IOError` carrying its message.
fn io_err_to_py(err: xl_io::IoError) -> PyErr {
    PyIOError::new_err(err.to_string())
}

/// The stable string form of a [`DiagnosticKind`] (mirrors the enum; kept as a
/// total match so a new variant is a compile error here, not a silent gap).
fn kind_str(kind: DiagnosticKind) -> &'static str {
    match kind {
        DiagnosticKind::ParseError => "ParseError",
        DiagnosticKind::UnknownFunction => "UnknownFunction",
        DiagnosticKind::ArityError => "ArityError",
        DiagnosticKind::UnsupportedConstruct => "UnsupportedConstruct",
        DiagnosticKind::CircularReference => "CircularReference",
    }
}

/// Convert one [`Value`] into the corresponding Python object per the module's
/// mapping table. Arrays recurse (row-major `list[list[...]]`).
fn value_to_py(py: Python<'_>, value: &Value) -> PyResult<Py<PyAny>> {
    Ok(match value {
        Value::Number(n) => PyFloat::new(py, *n).into_any().unbind(),
        Value::Text(t) => PyString::new(py, t.as_str()).into_any().unbind(),
        Value::Bool(b) => PyBool::new(py, *b).to_owned().into_any().unbind(),
        Value::Blank => py.None(),
        Value::Error(kind) => Py::new(
            py,
            CellError {
                code: kind.as_str().to_string(),
            },
        )?
        .into_any(),
        Value::Array(arr) => {
            let outer = PyList::empty(py);
            for r in 0..arr.rows() {
                let row_list = PyList::empty(py);
                for c in 0..arr.cols() {
                    // `get` is in-bounds for r < rows, c < cols by construction.
                    let elem = arr
                        .get(r, c)
                        .expect("array index within its own rows/cols is always present");
                    row_list.append(value_to_py(py, elem)?)?;
                }
                outer.append(row_list)?;
            }
            outer.into_any().unbind()
        }
        // A bare `Ref` reaching a cell value is not something v1 resolves; keep
        // it distinguishable and non-crashing rather than guessing.
        Value::Ref(_) => Py::new(
            py,
            CellError {
                code: "#UNSUPPORTED!".to_string(),
            },
        )?
        .into_any(),
        // BC-6 (RFC-0012) + RFC-0013 §4: a lambda is engine-internal and
        // NEVER crosses a language boundary. Marshal it as a distinguishable
        // `#UNSUPPORTED!` `CellError` (NOT a silent `None`, which would look
        // like a blank cell). NOTE: the Python API surface is a human
        // checkpoint before release (a Recalc design rule); this implements the refusal.
        Value::Lambda(_) => Py::new(
            py,
            CellError {
                code: "#UNSUPPORTED!".to_string(),
            },
        )?
        .into_any(),
    })
}
