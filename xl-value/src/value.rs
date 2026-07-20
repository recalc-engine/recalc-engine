//! The [`Value`] enum and its structural payloads ([`Array`], [`Ref`],
//! [`RectRange`], [`SheetId`]).
//!
//! ## Provenance
//! The variant set (`Number`, `Text`, `Bool`, `Error`, `Blank`, `Array`,
//! `Ref`) is fixed by `implementation-plan.md` §2. The **NaN/Inf invariant**
//! (below) follows from Excel having no NaN/Inf *cell* values: operations
//! that would produce them yield `#NUM!` (or `#DIV/0!` for division by zero).
//! See Microsoft Learn "Overflow" / `#NUM!` guidance behind
//! <https://support.microsoft.com/en-us/office/how-to-correct-a-num-error>.

use crate::error::ErrorKind;
use crate::text::Text;
use std::sync::Arc;

/// A calculated spreadsheet value — the unit of data flowing between every
/// Recalc crate. **This type is the frozen cross-crate contract**
/// (`implementation-plan.md` §2); changing its shape requires an RFC.
///
/// ## NaN / Infinity invariant (read this)
/// A [`Value::Number`] **must never hold a NaN or infinite `f64`**. Excel has
/// no NaN/Inf cell value; any operation that would produce one must instead
/// yield an error ([`ErrorKind::Num`] in general, [`ErrorKind::Div0`] for
/// division by zero, which the *caller* detects before dividing).
///
/// Construct numbers through [`Value::number`], which normalizes non-finite
/// inputs to [`ErrorKind::Num`]. The `Number(f64)` variant is left public
/// (per the plan's literal shape and for ergonomic pattern matching), so the
/// invariant is upheld *by construction discipline*: **do not** write
/// `Value::Number(x / y)` directly — route through [`Value::number`], or
/// check the divisor and emit [`ErrorKind::Div0`] yourself.
///
/// (An alternative that enforces the invariant in the type system — wrapping
/// the payload in a private `Finite(f64)` newtype — was considered and
/// rejected for v0 because it would make every `match` on `Value::Number`
/// clumsier for the many downstream call sites; revisit via RFC if discipline
/// proves insufficient.)
///
/// `Value` is intentionally **not** `Eq`/`Hash` (it carries `f64`). Use
/// [`crate::compare`] for Excel comparison semantics and
/// [`crate::total_order`] for a deterministic total order.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A finite IEEE-754 double. See the NaN/Inf invariant on [`Value`].
    Number(f64),
    /// An interned string (may be empty; `""` is **distinct** from
    /// [`Value::Blank`] — see the coercion module).
    Text(Text),
    /// A logical value.
    Bool(bool),
    /// An error value; propagates through operations.
    Error(ErrorKind),
    /// An empty cell. Distinct from `Text("")`: coerces to `0` in numeric
    /// context and `""` in text context, whereas `Text("")` does not coerce
    /// to a number at all.
    Blank,
    /// A rectangular 2-D block of values (an array constant or a spilled
    /// dynamic-array result). Always at least 1×1 and rectangular.
    Array(Array),
    /// A reference to a rectangular range on a sheet. **Placeholder** until
    /// `xl-ast`/`xl-graph` exist; extended by RFC.
    Ref(Ref),
    /// An engine-internal **lambda value** — the opaque payload a LET/LAMBDA
    /// closure evaluates to (RFC-0012 §2, BC-3). The concrete closure type
    /// lives in the interpreter crate *above* `xl-ast` (P2 lane 7); this crate
    /// sees only the opaque [`Lambda`] handle and never inspects it.
    ///
    /// **Born refusing (BC-6):** nothing in production constructs this variant
    /// yet — every coercion/ordering/marshalling site refuses lambdas with a
    /// distinguishable error until the interpreter (and the relevant oracle
    /// experiments) land. See [`Lambda`].
    Lambda(Lambda),
}

impl Value {
    /// Constructs a numeric value, upholding the NaN/Inf invariant: a
    /// non-finite `x` becomes `Value::Error(ErrorKind::Num)`.
    ///
    /// This is the **only** sanctioned way to turn a computed `f64` into a
    /// `Value`. Division-by-zero is *not* handled here (a caller dividing by
    /// zero must emit [`ErrorKind::Div0`] before the result becomes `Inf`);
    /// this maps every non-finite result — including one that slipped through
    /// — to `#NUM!` as a backstop.
    #[must_use]
    pub fn number(x: f64) -> Value {
        if x.is_finite() {
            Value::Number(x)
        } else {
            Value::Error(ErrorKind::Num)
        }
    }

    /// Constructs an interned text value.
    #[must_use]
    pub fn text(s: &str) -> Value {
        Value::Text(Text::new(s))
    }

    /// Constructs a boolean value.
    #[must_use]
    pub fn bool(b: bool) -> Value {
        Value::Bool(b)
    }

    /// Constructs an error value.
    #[must_use]
    pub fn error(kind: ErrorKind) -> Value {
        Value::Error(kind)
    }

    /// `true` if this is `Error(_)`.
    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self, Value::Error(_))
    }

    /// `true` if this is `Blank`.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        matches!(self, Value::Blank)
    }

    /// Debug/test helper: checks the NaN/Inf invariant holds for this value
    /// (recursively for arrays). Library code should never be able to make
    /// this return `false`; property tests assert it.
    #[must_use]
    pub fn upholds_number_invariant(&self) -> bool {
        match self {
            Value::Number(n) => n.is_finite(),
            Value::Array(a) => a.iter().all(Value::upholds_number_invariant),
            // A lambda holds no `f64`, so the invariant is vacuously upheld.
            // Enumerated (no bare `_`) so the next `Value` variant breaks the
            // build here too — BC-6 (RFC-0012 §4).
            Value::Text(_)
            | Value::Bool(_)
            | Value::Error(_)
            | Value::Blank
            | Value::Ref(_)
            | Value::Lambda(_) => true,
        }
    }
}

/// The opaque payload of [`Value::Lambda`] — an **engine-internal** lambda
/// object (a LET/LAMBDA closure: its parameters, body handle, and captured
/// environment).
///
/// ## Why a trait object (RFC-0012 §2, BC-3)
/// The **concrete** closure type lives in the interpreter crate *above*
/// `xl-ast` (P2 lane 7). Because `xl-ast` depends on `xl-value`, a concrete
/// `body: Expr` field inside `xl-value` would be a dependency cycle and would
/// not compile (BC-3, a defect in the original RFC draft). `xl-value`
/// therefore sees only this opaque trait object and never inspects a lambda's
/// internals.
///
/// ## Thread-safety (BC-2)
/// `Send + Sync` is **mandatory**: [`Value`] must stay `Send` for
/// same-milestone parallel recalc (P2 lane 11), and it already pays the atomic
/// cost ([`Text`](crate::Text) is `Arc<str>`). The whole payload — params,
/// body handle, captured env — is thread-safe. `Arc` (never `Rc`) is what
/// keeps `Value: Send`.
pub trait LambdaObj: core::fmt::Debug + Send + Sync + core::any::Any {
    /// Upcast to [`Any`](core::any::Any) so the **engine's interpreter** can
    /// recover the concrete closure type (`downcast_ref`) when it needs to
    /// *apply* a lambda (RFC-0012 §10 amendment, M2 lane 2). `xl-value` still
    /// never inspects the closure — it only forwards this reference; the
    /// closure's parameters, body, and captured environment, and the entire
    /// application semantics, live in `xl-engine` (BC-3 layering, unchanged).
    fn as_any(&self) -> &dyn core::any::Any;
}

/// A first-class **lambda value** — a newtype over `Arc<dyn LambdaObj>`
/// (RFC-0012 §2, BC-2/BC-3).
///
/// - **`Clone`** is `Arc::clone` (a cheap refcount bump), so `Value::clone`
///   stays cheap.
/// - **`PartialEq`** is `Arc::ptr_eq` **pointer identity** — two distinct
///   lambdas are unequal. This exists **only** to keep `Value: PartialEq`
///   total; it is **NOT** the `=`/`<>` operator semantics. The comparison
///   operators route through [`compare`](crate::compare) / `compare_fuzzy`,
///   which **refuse** lambdas (`#UNSUPPORTED!`) until an oracle experiment pins
///   Excel's actual lambda-comparison behavior (BC-6 rider ii). Do **not**
///   define lambda equality from memory.
///
/// **Born refusing (BC-6):** no production code constructs a `Lambda`; only
/// tests do (via the `test-util` feature). The real constructor is the lane-7
/// LET/LAMBDA interpreter, not yet built.
#[derive(Clone, Debug)]
pub struct Lambda(Arc<dyn LambdaObj>);

impl PartialEq for Lambda {
    /// Pointer identity only (`Arc::ptr_eq`); see the type docs. A totality
    /// crutch for `Value: PartialEq`, **not** the `=` operator (which refuses
    /// lambdas — BC-6 rider ii).
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Lambda {
    /// **Production** constructor wrapping the engine interpreter's opaque
    /// closure object (RFC-0012 §2/§8 — "the real constructor is the
    /// LET/LAMBDA interpreter"; the §10 amendment ratifies this ungated
    /// constructor for M2 lane 2). The born-refusing guarantee (BC-6) is
    /// unchanged: every coercion / ordering / comparison arm still refuses
    /// `Value::Lambda`; only the engine's interpreter constructs one (here)
    /// and applies it (via [`Lambda::obj`] → [`LambdaObj::as_any`]).
    #[must_use]
    pub fn new(obj: Arc<dyn LambdaObj>) -> Lambda {
        Lambda(obj)
    }

    /// Borrow the opaque payload as a [`LambdaObj`] so the engine can downcast
    /// it (`self.obj().as_any().downcast_ref::<Closure>()`) back to its concrete
    /// closure to apply it. `xl-value` exposes only this opaque handle — it
    /// never depends on the concrete closure type (BC-3).
    #[must_use]
    pub fn obj(&self) -> &dyn LambdaObj {
        &*self.0
    }

    /// **Test-only** constructor wrapping an arbitrary [`LambdaObj`].
    ///
    /// Gated behind `test` / the `test-util` feature so a default
    /// `cargo build` compiles **no** `Value::Lambda` constructor at all —
    /// "born refusing" (RFC-0012 BC-6): only this crate's tests, or a
    /// downstream crate that opts into `test-util` in its `[dev-dependencies]`,
    /// can materialize a lambda value to exercise the refusing arms.
    #[cfg(any(test, feature = "test-util"))]
    #[must_use]
    pub fn new_for_test(obj: Arc<dyn LambdaObj>) -> Lambda {
        Lambda(obj)
    }
}

/// A trivial [`LambdaObj`] for tests — carries no closure. Exists only so
/// tests can materialize a [`Value::Lambda`] and assert the born-refusing
/// arms. Gated identically to [`Lambda::new_for_test`].
#[cfg(any(test, feature = "test-util"))]
#[derive(Debug)]
pub struct TestLambda;

#[cfg(any(test, feature = "test-util"))]
impl LambdaObj for TestLambda {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

#[cfg(any(test, feature = "test-util"))]
impl Value {
    /// **Test-only** helper: a fresh [`Value::Lambda`] wrapping a
    /// [`TestLambda`]. Each call allocates a distinct `Arc`, so two calls are
    /// pointer-unequal — useful for asserting distinct-lambda ordering/dedup.
    /// See [`Lambda::new_for_test`]. NOT a production constructor.
    #[must_use]
    pub fn test_lambda() -> Value {
        Value::Lambda(Lambda::new_for_test(Arc::new(TestLambda)))
    }
}

/// Error returned by [`Array::new`] when the supplied dimensions and data do
/// not form a valid rectangle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShapeError {
    /// `rows * cols != data.len()`.
    LengthMismatch {
        /// Declared row count.
        rows: usize,
        /// Declared column count.
        cols: usize,
        /// Actual length of the supplied data.
        len: usize,
    },
    /// A dimension was zero. Excel arrays are always at least 1×1.
    Empty,
}

impl core::fmt::Display for ShapeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ShapeError::LengthMismatch { rows, cols, len } => write!(
                f,
                "array data length {len} does not match {rows}x{cols} = {}",
                rows * cols
            ),
            ShapeError::Empty => f.write_str("array dimensions must be at least 1x1"),
        }
    }
}

impl std::error::Error for ShapeError {}

/// A rectangular 2-D array of [`Value`]s, stored **row-major**.
///
/// The rectangular invariant (`data.len() == rows * cols`, both ≥ 1) is
/// enforced by [`Array::new`] and preserved by all methods. Out-of-bounds
/// element access returns `None` here; the Excel-specific "reading past a
/// spilled array yields `#N/A`" behavior is layered on later in `xl-fn`
/// (kept out of this crate deliberately).
///
/// Elements are *not* prevented from themselves being `Array`/`Ref`, but
/// Excel arrays do not nest; producers should keep elements scalar.
#[derive(Clone, Debug, PartialEq)]
pub struct Array {
    rows: usize,
    cols: usize,
    data: Vec<Value>,
}

impl Array {
    /// Builds an array from row-major `data`. Errors unless
    /// `data.len() == rows * cols` and both dimensions are ≥ 1.
    pub fn new(rows: usize, cols: usize, data: Vec<Value>) -> Result<Array, ShapeError> {
        if rows == 0 || cols == 0 {
            return Err(ShapeError::Empty);
        }
        if rows.checked_mul(cols) != Some(data.len()) {
            return Err(ShapeError::LengthMismatch {
                rows,
                cols,
                len: data.len(),
            });
        }
        Ok(Array { rows, cols, data })
    }

    /// Lifts a single scalar into a 1×1 array (the inverse of
    /// [`Array::as_scalar`]).
    #[must_use]
    pub fn from_scalar(value: Value) -> Array {
        Array {
            rows: 1,
            cols: 1,
            data: vec![value],
        }
    }

    /// Number of rows (≥ 1).
    #[must_use]
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Number of columns (≥ 1).
    #[must_use]
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Borrows the element at `(row, col)` (0-based), or `None` if out of
    /// bounds.
    #[must_use]
    pub fn get(&self, row: usize, col: usize) -> Option<&Value> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        self.data.get(row * self.cols + col)
    }

    /// If this array is exactly 1×1, borrows its single element (scalar
    /// lift). Used by coercion to treat a 1×1 array as the scalar it holds.
    #[must_use]
    pub fn as_scalar(&self) -> Option<&Value> {
        if self.rows == 1 && self.cols == 1 {
            self.data.first()
        } else {
            None
        }
    }

    /// Iterates over elements in row-major order.
    pub fn iter(&self) -> core::slice::Iter<'_, Value> {
        self.data.iter()
    }
}

impl<'a> IntoIterator for &'a Array {
    type Item = &'a Value;
    type IntoIter = core::slice::Iter<'a, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.data.iter()
    }
}

/// A sheet identifier. Opaque index assigned by `xl-io` when a workbook is
/// opened; the numbering scheme is that crate's concern.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SheetId(pub u32);

/// A rectangular range of cells in **0-based, inclusive** `(row, col)`
/// coordinates. `start <= end` on both axes is expected but not enforced here
/// (a normalized/validated range type is deferred to `xl-ast`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RectRange {
    /// First row (0-based, inclusive).
    pub row_start: u32,
    /// Last row (0-based, inclusive).
    pub row_end: u32,
    /// First column (0-based, inclusive).
    pub col_start: u32,
    /// Last column (0-based, inclusive).
    pub col_end: u32,
}

impl RectRange {
    /// Convenience constructor.
    #[must_use]
    pub fn new(row_start: u32, row_end: u32, col_start: u32, col_end: u32) -> RectRange {
        RectRange {
            row_start,
            row_end,
            col_start,
            col_end,
        }
    }
}

/// A reference to a rectangular range on one sheet.
///
/// **Placeholder.** Until `xl-ast`/`xl-graph` are built there is no formula
/// AST or graph to resolve against, so `Ref` carries only a sheet id and a
/// rectangular range. It will be extended (3-D refs, absolute/relative flags,
/// R1C1, whole-row/column, cross-workbook) by RFC. Coercion of a `Ref`
/// requires resolving it against sheet data, which this crate cannot do — the
/// `to_*` functions therefore return `#UNSUPPORTED!` for `Ref` inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ref {
    /// The sheet the range lives on.
    pub sheet: SheetId,
    /// The rectangular range (0-based, inclusive).
    pub range: RectRange,
}

impl Ref {
    /// Convenience constructor.
    #[must_use]
    pub fn new(sheet: SheetId, range: RectRange) -> Ref {
        Ref { sheet, range }
    }
}
