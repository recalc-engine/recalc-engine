//! The argument-access interface ([`CallArgs`]) a function uses to pull its
//! operands, plus the syntactic [`ArgShape`] classification.
//!
//! # Why a trait, not a `&[Value]`
//! Two Excel facts make eager `&[Value]` arguments wrong:
//!
//! 1. **Laziness.** `IF` must evaluate only the *selected* branch — the other
//!    branch may divide by zero, reference a `#UNSUPPORTED!` construct, or read
//!    a volatile, and none of that may happen (`docs/specs/IF.md` §2). So the
//!    function must control *when* each argument is evaluated; it cannot be
//!    handed pre-evaluated values.
//! 2. **The scalar/aggregate coercion asymmetry.** `SUM(TRUE,"2")` = 3 (direct
//!    scalar args coerce) but `SUM(A1:A3)` ignores text/logicals in the range
//!    (`docs/specs/SUM.md`). The function needs to know each argument's
//!    *syntactic shape* (a lone scalar vs a multi-cell range/array) to pick the
//!    coercion mode, and to stream a range's cells without materializing them.
//!
//! `xl-engine` implements this trait over the parsed argument expressions and
//! the current value store; `xl-fn` stays free of the AST-walking / cell-store
//! machinery. The methods take `&mut self` so an implementation may cache or
//! advance internal state as arguments are forced.
//!
//! # What a Tier-0 implementer needs to know
//! Pull operands only through this trait — never assume a `&[Value]`. Force a
//! scalar with [`eval_scalar`](CallArgs::eval_scalar); an omitted/out-of-range
//! position reads as [`Value::Blank`], and a lazily-selected branch must be
//! forced only on the path you take (see `func_if`). For range/array operands
//! pick the shape-aware access that matches your semantics: [`for_each_cell`](CallArgs::for_each_cell)
//! streams only *materialized* cells (blanks elided, sparse whole-column-safe)
//! for order-independent aggregates like `SUM`; [`dims`](CallArgs::dims) plus
//! [`for_each_row`](CallArgs::for_each_row) walk the dense rectangle *with blanks
//! surfaced positionally* for lockstep/positional functions like `SUMIF` and
//! `VLOOKUP` — but that dense walk refuses an unbounded whole-column/row range
//! (returns `None`/`Err`; see [`for_each_row`](CallArgs::for_each_row)), so
//! guard for it. Every cell-streaming visitor returns [`ControlFlow`] so an
//! aggregate can stop at the first error instead of scanning the whole range.

use std::ops::ControlFlow;

use xl_value::{ErrorKind, SheetId, Value};

/// The syntactic shape of one call argument, as the engine classifies it.
///
/// This drives coercion-mode selection: [`ArgShape::Scalar`] arguments coerce
/// per [`xl_value::CoercionMode::Scalar`]; [`ArgShape::Range`] /
/// [`ArgShape::Array`] arguments are streamed cell-by-cell under
/// [`xl_value::CoercionMode::RangeAggregate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgShape {
    /// A single scalar operand: a literal, a single-cell reference, a
    /// computed sub-expression, or a 1×1 range/array (lifted to its element).
    Scalar,
    /// A multi-cell reference range (`A1:A3`, `A:A`).
    Range,
    /// A multi-element array constant (`{1,2,3}`).
    Array,
    /// An omitted argument (an elided position such as the empty slot in
    /// `IF(A1,,2)` or a trailing `SUM(1,)`).
    Omitted,
}

/// Per-cell provenance surfaced alongside a streamed cell value by
/// [`CallArgs::for_each_cell_tagged`].
///
/// This is the payload of the RFC 0002 cell-provenance channel: it lets an
/// aggregate see *how a referenced cell was produced*, not just its value, so
/// `SUBTOTAL` can skip cells that are themselves `SUBTOTAL` results (Excel's
/// nested-subtotal exclusion — a grand-total over sub-totals must not
/// double-count) *and* skip cells sitting in a hidden worksheet row (Excel's
/// `101`–`111` forms exclude those). The struct is intentionally a small `Copy`
/// bag of bits with room to grow; both consumers ride the very same channel.
///
/// [`Default`] is "no provenance" (every flag `false`), the correct reading for
/// a cell with no formula, a literal, or a computed sub-expression.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellFlags {
    /// The source cell's own formula head is a `SUBTOTAL(...)` call, so a
    /// `SUBTOTAL` aggregating over it must ignore it (RFC 0002).
    pub is_subtotal: bool,
    /// The source cell lies in a **manually-hidden worksheet row** (OOXML
    /// `<row hidden="1">`, plumbed from `xl-io`'s row properties through the
    /// engine — OXP-121). `SUBTOTAL`'s `101`–`111` `function_num` forms drop
    /// these cells before aggregating; the `1`–`11` forms keep them. RFC 0002
    /// reserved this second provenance bit for exactly this consumer.
    ///
    /// Caveat: OOXML `hidden="1"` conflates *manually*-hidden rows and rows
    /// hidden by an active AutoFilter — Excel excludes both from `101`–`111`, so
    /// the conflation is correct there. The `1`–`11` forms (which never act on
    /// this flag) therefore stay unaware of *filter*-hidden rows — a documented
    /// latent divergence, see `docs/specs/SUBTOTAL.md`.
    pub is_hidden_row: bool,
}

/// The position **and** extent of a *reference* argument — the RFC 0005
/// reference-position channel surfaced by [`CallArgs::arg_ref_extent`].
///
/// Where [`CallArgs::dims`] reports only a rectangle's *size* (`(rows, cols)`),
/// this also carries its **position** (the 0-based row/column of the reference's
/// top-left cell), which `ROW`/`COLUMN` need — a reference's address, not its
/// contents. All four fields are geometry, never values.
///
/// # Coordinate + extent contract
/// - `row` / `col` are **0-based** (matching `xl-graph`'s `CellId` /
///   `xl-value`'s `RectRange`); a consumer that wants Excel's 1-based row/column
///   *number* adds 1 (`ROW(A1)` = `row + 1` = `1`).
/// - `height` / `width` are inclusive cell **counts** (always `>= 1`): a
///   single cell is `1 × 1`, a whole column `A:A` is `1_048_576 × 1`, a whole
///   row `1:1` is `1 × 16_384`.
///
/// Unlike `dims`, `arg_ref_extent` does **not** refuse an unbounded
/// whole-column/row range: `height`/`width` carry the full sheet-axis extent so
/// `ROWS(A:A)` / `COLUMNS(1:1)` can read it (OXP-116). This type lives in
/// `xl-fn::args`, **not** `xl-value` — it is a function/engine seam detail, not
/// part of the frozen coercion contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefRect {
    /// 0-based row of the reference's top-left cell.
    pub row: u32,
    /// 0-based column of the reference's top-left cell.
    pub col: u32,
    /// Number of rows the reference spans (inclusive count, `>= 1`).
    pub height: u32,
    /// Number of columns the reference spans (inclusive count, `>= 1`).
    pub width: u32,
}

/// The interface a function uses to inspect and evaluate its arguments.
///
/// Indices are 0-based over the call's argument list. A function should only
/// call [`eval_scalar`](CallArgs::eval_scalar) / [`for_each_cell`](CallArgs::for_each_cell)
/// for the arguments it actually needs — forcing an argument is what triggers
/// its (possibly side-effecting, possibly erroring) evaluation.
pub trait CallArgs {
    /// The number of arguments supplied at the call site (including omitted
    /// positions, which still count — `IF(A1,,2)` has three).
    fn count(&self) -> usize;

    /// Classify argument `index` without evaluating it.
    ///
    /// Out-of-range indices classify as [`ArgShape::Omitted`].
    fn shape(&mut self, index: usize) -> ArgShape;

    /// Evaluate argument `index` in **scalar context** and return its value.
    ///
    /// A multi-cell range/array in scalar context follows `xl-value`'s rule
    /// (1×1 lifts to its element; larger → `#UNSUPPORTED!`). An out-of-range or
    /// omitted argument evaluates to [`Value::Blank`].
    fn eval_scalar(&mut self, index: usize) -> Value;

    /// Evaluate argument `index` as an **array-context aggregator argument**
    /// (RFC-0011 + the M2 lane-6 amendment). Semantically like
    /// [`CallArgs::eval_scalar`], except a **multi-cell** range/computed
    /// reference reaching scalar context anywhere in the argument's evaluation —
    /// directly, or through nested `IF`/`ISBLANK`/`NOT`/operators — is handled as
    /// a genuine array rather than silently implicit-intersecting a single cell.
    /// A **bounded** such range **materializes** into a `Value::Array` so the
    /// consumed-array evaluator can fold it (the OXP-201-pinned subset —
    /// `SUM(IF(range,…))` → the array-sum, a number); an **unbounded** or
    /// over-cap range, and any shape NOT on that pinned list, still returns loud
    /// `#UNSUPPORTED!` (Principle 2 — never silently wrong). Aggregators call
    /// this for their **value** arguments (not their leading scalar arg — e.g.
    /// `SUBTOTAL`'s `function_num`, `NPV`'s `rate` — which keep `eval_scalar`'s
    /// pinned implicit intersection).
    ///
    /// Defaulted to [`CallArgs::eval_scalar`]: only the engine's array-aware
    /// implementation refuses; test mocks and any non-engine caller are
    /// unaffected.
    fn eval_scalar_array_arg(&mut self, index: usize) -> Value {
        self.eval_scalar(index)
    }

    /// Whether this call is being evaluated inside an **array-context aggregator
    /// argument** (RFC-0011 / M2 lane 6): `true` when a multi-cell range in
    /// scalar position is materialized into a `Value::Array` rather than
    /// implicit-intersected. Consulted by `IF`/`ISBLANK`/`NOT` (and the engine's
    /// operator arms) to decide whether to map their scalar rule **element-wise**
    /// over a genuine consumed array. It surfaces the already-ratified RFC-0011
    /// flag read-only; it adds no new state.
    ///
    /// Gating the element-wise paths on this — rather than merely on "the operand
    /// is an array" — keeps top-level array-returning expressions
    /// (`=INDEX(A:A,0,1)+1`, `=ISBLANK(INDEX(A:A,0,1))`) byte-identical: outside
    /// array context they still refuse loudly (Principle 2), because a bare
    /// `Value::Array` there must not silently spill.
    ///
    /// Defaulted to `false`: only the engine's array-aware implementation (and
    /// the `SUBTOTAL` delegate shims that forward it) report `true`; test mocks
    /// and non-engine callers are unaffected.
    fn array_arg_ctx(&self) -> bool {
        false
    }

    /// Stream every materialized cell/element of a range/array argument, in
    /// row-major order, into `visit`.
    ///
    /// Only *materialized* cells are visited: an absent cell in a referenced
    /// range is a blank that aggregation skips, so it is elided here rather
    /// than surfaced as [`Value::Blank`] (documented so future non-`SUM`
    /// aggregates that care about blanks can use [`for_each_row`](CallArgs::for_each_row)
    /// instead). If the argument cannot be resolved to a range (a bad sheet, a
    /// 3-D span, an unresolved name), `visit` is called once with a
    /// [`Value::Error`] carrying [`ErrorKind::Unsupported`] so the aggregate
    /// propagates it.
    ///
    /// `visit` returns [`ControlFlow`]: yielding [`ControlFlow::Break`] stops
    /// the stream immediately (so an aggregate can short-circuit at the first
    /// error instead of visiting-and-ignoring the rest — `SUM` relies on this).
    /// Because it is blank-eliding and sparse, this method is safe on an
    /// unbounded whole-column/row range (it costs `O(populated cells)`); the
    /// dense [`for_each_row`](CallArgs::for_each_row) walk is not.
    fn for_each_cell(&mut self, index: usize, visit: &mut dyn FnMut(&Value) -> ControlFlow<()>);

    /// Report the rectangular dimensions `(rows, cols)` of a range/array
    /// argument, without materializing it.
    ///
    /// Returns `None` for anything that is not a bounded, densely-walkable
    /// rectangle: a scalar/lazy/omitted argument (it has no rectangle), **or**
    /// an unbounded whole-column/row range whose rectangle spans the full sheet
    /// (`A:A` is 1,048,576 rows). The unbounded case is a deliberate engineering
    /// guardrail, not an Excel semantic — see [`for_each_row`](CallArgs::for_each_row)
    /// for the rationale and policy. A caller that expected a rectangle and gets
    /// `None` should treat the argument as non-materializable (typically its own
    /// `#UNSUPPORTED!`/`#VALUE!` path).
    ///
    /// Precision note: only true scalar *expressions* (plus lazy/omitted and
    /// unbounded args) return `None` — a 1×1 **range or array literal** still
    /// reports `Some((1, 1))` even though [`shape`](CallArgs::shape) classifies
    /// it `Scalar`. Both branches remain safe either way; just don't assume
    /// `shape() == Scalar` implies `dims() == None`.
    fn dims(&mut self, index: usize) -> Option<(u32, u32)>;

    /// Visit the argument's rectangle one **row** at a time, in row-major order,
    /// with blanks surfaced positionally.
    ///
    /// Each call to `visit` receives one full-width row slice of the rectangle;
    /// an absent cell inside the rectangle is passed as [`Value::Blank`] at its
    /// column position (unlike [`for_each_cell`](CallArgs::for_each_cell), which
    /// elides blanks). This is the access mode positional/lockstep functions
    /// need — `SUMIF`'s range/`sum_range` walk (with the top-left-anchor resize
    /// rule) and `VLOOKUP`/`INDEX`/`MATCH`'s column extraction and first-column
    /// search — because those must know *where* each cell sits, blanks included.
    /// Rows are materialized one at a time, so a lockstep consumer never holds
    /// the whole range at once. That guarantee is **per argument**: this method
    /// walks one argument per call, so a two-argument lockstep walk (SUMIF's
    /// `range` + `sum_range`) must buffer one argument's rows while walking the
    /// other — there is no combined zip walk.
    ///
    /// Cost note: the unbounded-range refusal below only fires when an axis
    /// spans the *entire* sheet. A huge-but-bounded rectangle (say
    /// `A1:XFC1048574`) is walked faithfully, which can mean billions of cells —
    /// the cost is proportional to the rectangle you ask for, bounded or not.
    ///
    /// `visit` returns [`ControlFlow`]: [`ControlFlow::Break`] stops the walk
    /// after the current row (a first-match `VLOOKUP` scan stops as soon as it
    /// hits its row).
    ///
    /// # Errors
    /// Returns `Err` if the argument cannot be resolved to a materializable
    /// rectangle:
    /// - `Err(ErrorKind::Unsupported)` for an **unbounded** whole-column/row
    ///   range. Iterating the full rectangle of `A:A` is 1,048,576 rows, and the
    ///   engine cannot cheaply know a sheet's used-range extent (xl-io does not
    ///   surface `<dimension>` to the evaluator, and the value store holds only
    ///   populated cells), so rather than *guess* an extent or spend 1M
    ///   iterations the dense walk is **refused**. This is an engineering policy,
    ///   documented as such — not a claim about Excel. (A future used-range
    ///   extent from xl-io could turn this into a clamp instead of a refusal.)
    ///   The blank-eliding [`for_each_cell`](CallArgs::for_each_cell) remains the
    ///   whole-column-safe path for order-independent aggregates.
    /// - `Err(ErrorKind::Unsupported)` for an argument that resolves to no range
    ///   at all (bad sheet, 3-D span, unresolved name).
    ///
    /// A scalar argument is treated as a 1×1 rectangle (one row of one value),
    /// so a positional function can accept a lone scalar uniformly.
    fn for_each_row(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(&[Value]) -> ControlFlow<()>,
    ) -> Result<(), ErrorKind>;

    /// Visit an argument's **populated** rows within its column span, one row at
    /// a time, in **ascending row order**, tolerating an unbounded whole-column
    /// range in `O(populated cells)`.
    ///
    /// This is the sparse, *used-extent* counterpart to the dense
    /// [`for_each_row`](CallArgs::for_each_row). Where `for_each_row` walks
    /// *every* row of the rectangle (blanks surfaced) and therefore **refuses**
    /// an unbounded whole-column range (`A:D` = 1,048,576 rows), this method
    /// walks only the rows that actually contain a cell in the range's column
    /// span — which the engine's sparse cell store knows exactly — so a
    /// whole-column argument is served without a 1M-row scan.
    ///
    /// Each populated row is passed to `visit` as a full-column-span slice
    /// (`col_end - col_start + 1` wide), with absent cells inside that span
    /// surfaced as [`Value::Blank`] at their column position (like
    /// `for_each_row`, unlike the blank-eliding [`for_each_cell`](CallArgs::for_each_cell)).
    /// The first `visit` argument is the row's 0-based index **relative to the
    /// range's top row** (0 = the range's first row); for a whole-column range
    /// the top is row 0, so it doubles as the absolute row. Relative indexing is
    /// what a lookup *position* (`MATCH`) or a top-left-anchored sum-range
    /// (`SUMIF`) needs, and keeps `xl-fn` free of sheet geometry.
    ///
    /// # When to use which
    /// - **Dense positional** (`for_each_row`): a bounded range whose blank rows
    ///   are semantically significant and must be surfaced at their position.
    /// - **Used-extent** (this method): the whole-column case, where the only
    ///   sane iteration is over populated rows. A consumer that works for both
    ///   bounded and unbounded arguments should route through the crate-internal
    ///   `for_each_row_or_used` helper, which prefers the dense walk and falls
    ///   back to this one only when the dense walk refuses.
    ///
    /// # Ordering
    /// Rows are yielded in strictly ascending row order (the store is
    /// `(sheet,row,col)`-ordered), which the `VLOOKUP`/`MATCH` linear and binary
    /// searches rely on.
    ///
    /// # Errors
    /// - `Err(ErrorKind::Unsupported)` for a **whole-row** range (unbounded
    ///   *columns*): the column span would be the whole sheet width, so a
    ///   per-row slice is 16,384 wide — this row-oriented iterator does not serve
    ///   that shape (a future used-extent *column* iterator would).
    /// - `Err(ErrorKind::Unsupported)` for an argument that resolves to no range
    ///   (bad sheet, 3-D span, unresolved name).
    ///
    /// # Default
    /// The default implementation refuses everything
    /// (`Err(ErrorKind::Unsupported)`), preserving the pre-RFC-0001 whole-column
    /// behavior for any `CallArgs` implementor that does not opt in. Only
    /// [`EngineArgs`](../../xl_engine/index.html) (over the real cell store) and
    /// the test mocks override it. See `rfcs/0001-used-extent-range-iteration.md`.
    fn for_each_used_row(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
    ) -> Result<(), ErrorKind> {
        let _ = (index, visit);
        Err(ErrorKind::Unsupported)
    }

    /// Visit an argument's **populated** columns within its row span, one column
    /// at a time, in **ascending column order**, tolerating an unbounded
    /// whole-**row** range in `O(populated)` — the transpose of
    /// [`for_each_used_row`](CallArgs::for_each_used_row) (RFC 0008).
    ///
    /// Where [`for_each_used_row`](CallArgs::for_each_used_row) serves the
    /// whole-**column** form (`A:A`) by walking populated *rows*, this serves the
    /// whole-**row** form (`1:1`, `1:5`) — a horizontal criteria band or lookup
    /// vector — by walking only the *columns* that actually contain a cell in the
    /// range's row span, which the engine's sparse cell store knows exactly, so a
    /// whole-row argument is served without a 16,384-column scan.
    ///
    /// Each populated column is passed to `visit` as a full-row-span slice
    /// (`row_end - row_start + 1` tall), with absent cells inside that span
    /// surfaced as [`Value::Blank`] at their row position (like
    /// [`for_each_used_row`](CallArgs::for_each_used_row), unlike the
    /// blank-eliding [`for_each_cell`](CallArgs::for_each_cell)). The first
    /// `visit` argument is the column's 0-based index **relative to the range's
    /// left column** (0 = the range's first column); for a whole-row range the
    /// left is column 0, so it doubles as the absolute column. Relative indexing
    /// is what a lookup *position* (`MATCH`, returning `relative col + 1`) or a
    /// top-left-anchored `sum_range` (`SUMIF`) needs, and keeps `xl-fn` free of
    /// sheet geometry.
    ///
    /// # Ordering (load-bearing, and *not* free — see RFC 0008 §2)
    /// Columns are yielded in strictly ascending column order. Unlike
    /// [`for_each_used_row`](CallArgs::for_each_used_row), which gets ascending
    /// order for free from the `(sheet, row, col)`-ordered store (a row band scan
    /// is intrinsically row-ascending), a row-major scan encounters *columns* out
    /// of order, so the engine implementation **buffers** the row band's
    /// populated cells grouped by column and emits them sorted. Callers such as
    /// `MATCH`'s linear/binary search rely on this ascending guarantee.
    ///
    /// # Errors
    /// - `Err(ErrorKind::Unsupported)` for a whole-**column** range (unbounded
    ///   *rows*): the row span would be the whole sheet height, so a per-column
    ///   slice is 1,048,576 tall — this column-oriented iterator does not serve
    ///   that shape (that is [`for_each_used_row`](CallArgs::for_each_used_row)'s
    ///   job).
    /// - `Err(ErrorKind::Unsupported)` for an argument that resolves to no range
    ///   (bad sheet, 3-D span, unresolved name).
    ///
    /// # Default
    /// The default implementation refuses everything
    /// (`Err(ErrorKind::Unsupported)`), so any `CallArgs` implementor that does
    /// not opt in keeps the pre-RFC-0008 whole-row behavior (`#UNSUPPORTED!`).
    /// Only [`EngineArgs`](../../xl_engine/index.html) (over the real cell store)
    /// and the test mocks override it. See
    /// `rfcs/0008-used-extent-column-iteration.md`.
    fn for_each_used_col(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
    ) -> Result<(), ErrorKind> {
        let _ = (index, visit);
        Err(ErrorKind::Unsupported)
    }

    /// Stream every materialized cell of a range/array argument like
    /// [`for_each_cell`](CallArgs::for_each_cell), but additionally hand each
    /// cell's provenance [`CellFlags`] to `visit` — the RFC 0002 cell-provenance
    /// channel.
    ///
    /// The cell *values* are exactly those [`for_each_cell`](CallArgs::for_each_cell)
    /// yields (materialized cells only, blanks elided, whole-column-safe sparse
    /// scan); the added [`CellFlags`] tell the aggregate how each cell was
    /// produced. `SUBTOTAL` uses this to skip cells whose own formula is a
    /// `SUBTOTAL` call ([`CellFlags::is_subtotal`]) so a grand-total does not
    /// double-count its sub-totals. `visit` returns [`ControlFlow`] with the same
    /// short-circuit contract as `for_each_cell`.
    ///
    /// # Errors / refusal
    /// Returns `Err(())` when the argument shape carries **no per-cell
    /// provenance** the implementation can serve — e.g. an array constant
    /// (`{1,2,3}` has cells but no originating formula) — so a caller can fall
    /// back to the plain, un-tagged [`for_each_cell`](CallArgs::for_each_cell)
    /// walk for that argument. The refusal is *per argument*: a call may serve
    /// one argument tagged and refuse another.
    ///
    /// **Atomic-refusal invariant (load-bearing):** an implementation MUST NOT
    /// invoke `visit` before returning `Err(())`. Callers are entitled to
    /// re-walk the same argument with [`for_each_cell`](CallArgs::for_each_cell)
    /// on refusal (as `SUBTOTAL`'s nested-subtotal filter does); if an
    /// implementation visited some cells and *then* refused, the caller's
    /// fallback re-walk would double-count every already-visited cell. Refuse
    /// up front, before the first `visit`.
    ///
    /// # Default
    /// The default implementation refuses everything (`Err(())`), preserving
    /// behavior for every `CallArgs` implementor and every function that does
    /// not consume provenance. Only [`EngineArgs`](../../xl_engine/index.html)
    /// (which owns the tagged cell store) overrides it. Mirrors how
    /// [`for_each_used_row`](CallArgs::for_each_used_row) rolled out under RFC
    /// 0001. See `rfcs/0002-subtotal-nested-exclusion.md`.
    //
    // The `Err(())` carries no payload by design: it is a *binary* refuse signal
    // (this shape has no per-cell provenance to serve), and the caller only ever
    // branches served-vs-refused — unlike `for_each_used_row`, whose `ErrorKind`
    // is surfaced as a cell value. A unit error keeps the "there is nothing to
    // report" intent explicit, so the `result_unit_err` lint is allowed here.
    #[allow(clippy::result_unit_err)]
    fn for_each_cell_tagged(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(&Value, CellFlags) -> ControlFlow<()>,
    ) -> Result<(), ()> {
        let _ = (index, visit);
        Err(())
    }

    /// The **calling cell**'s own position — the sheet and 0-based `(row, col)`
    /// of the cell whose formula is being evaluated — the RFC 0005
    /// reference-position channel for the no-argument `ROW()` / `COLUMN()` forms.
    ///
    /// `ROW()` (no argument) is the 1-based row of the cell that *contains the
    /// formula*, so the aggregate needs its own anchor, which no other
    /// `CallArgs` method surfaces ([`dims`](CallArgs::dims) et al. describe an
    /// *argument*, not the caller). The engine already threads this anchor
    /// through evaluation; this exposes it.
    ///
    /// # Default
    /// The default returns `None` — an implementor with no per-cell anchor (an
    /// array-formula probe, a bare test mock) does not lie about a position, so
    /// `ROW()`/`COLUMN()` return `#UNSUPPORTED!` rather than a guessed origin.
    /// Only [`EngineArgs`](../../xl_engine/index.html), which owns the anchor,
    /// overrides it. See `rfcs/0005-callargs-reference-position.md`.
    fn anchor(&self) -> Option<(SheetId, u32, u32)> {
        None
    }

    /// The position **and** extent of argument `index` **when it is a
    /// reference** — the RFC 0005 channel for `ROW(reference)` /
    /// `COLUMN(reference)` (which read the reference's top-left address) and for
    /// `ROWS`/`COLUMNS` over a **whole-column/row** range (which read its full
    /// sheet-axis size — OXP-116).
    ///
    /// Returns `Some(RefRect)` only for an argument that resolves to a single
    /// rectangular **reference**: a bare cell (`A1`), a `ref:ref` range
    /// (`A1:C4`, `A:A`, `1:1`), a simple defined name, or a
    /// reference-returning call (`OFFSET`/`INDIRECT`; RFC 0003). A literal, an
    /// array constant, a computed scalar, a reference union, or a
    /// computed reference that *errors* has no ref extent → `None`.
    ///
    /// Contrast [`dims`](CallArgs::dims): `dims` reports a bounded rectangle's
    /// *size* and deliberately **refuses** (`None`) an unbounded whole-column/row
    /// range; `arg_ref_extent` reports *position + full extent* and does **not**
    /// refuse the unbounded case — its `height`/`width` carry the whole-axis
    /// count (`A:A` → `height == 1_048_576`), which is exactly what
    /// `ROWS`/`COLUMNS` need to answer `ROWS(A:A) = 1_048_576` (OXP-116).
    ///
    /// # Default
    /// The default returns `None` — non-breaking for every existing implementor
    /// and mock (mirrors how [`for_each_used_row`](CallArgs::for_each_used_row)
    /// and [`for_each_cell_tagged`](CallArgs::for_each_cell_tagged) rolled out).
    /// Only [`EngineArgs`](../../xl_engine/index.html), which can map an argument
    /// to its AST reference and resolve its geometry, overrides it. See
    /// `rfcs/0005-callargs-reference-position.md`.
    fn arg_ref_extent(&mut self, index: usize) -> Option<RefRect> {
        let _ = index;
        None
    }

    /// Read the single cell at an **absolute** 0-based `(row, col)` that lies
    /// **inside** argument `index`'s declared reference rectangle — the RFC-0006
    /// in-rectangle absolute read.
    ///
    /// This is the O(1) companion to [`arg_ref_extent`](CallArgs::arg_ref_extent):
    /// a function first reads an argument's geometry (position + full extent)
    /// with `arg_ref_extent`, then reads individual cells at absolute sheet
    /// coordinates with `cell_at`, **without materialising the rectangle**. It is
    /// what lets `INDEX(A:A, n)` and a whole-column `HLOOKUP` work over an
    /// unbounded whole-column/row range: `INDEX(A:A, 3)` is one read at row 2
    /// (0-based) of column A, and a whole-column `HLOOKUP` scans only the search
    /// row plus one result cell — never the 1,048,576-row rectangle.
    ///
    /// # Coordinate contract
    /// `row` / `col` are **absolute** 0-based sheet coordinates (matching
    /// [`RefRect`]'s `row`/`col` and `xl-graph`'s `CellId`), *not* offsets within
    /// the rectangle. A caller derives them from the extent:
    /// `abs_row = rect.row + (row_num - 1)`, `abs_col = rect.col + (col_num - 1)`.
    ///
    /// # Bounds + return
    /// The read is **confined to the argument's declared rectangle**: a position
    /// outside it returns `None`. A position **inside** the rectangle that has no
    /// stored cell returns `Some(Value::Blank)` — an absent cell is a blank, not a
    /// failure. `None` therefore means "the infrastructure cannot serve this
    /// position": the argument is not a resolvable single-area reference, or the
    /// position is out of its rectangle. The **caller** decides what that maps to
    /// (`INDEX` turns its own out-of-bounds index into `#REF!` *before* calling,
    /// so it never asks `cell_at` for a cell it has not already bounds-checked).
    /// Confining every read to the AST-declared precedent rectangle is what keeps
    /// it sound: each cell is already a static dependency of the formula, so there
    /// is no dependency-graph hazard and no whole-sheet scan.
    ///
    /// # Default
    /// The default returns `None` — non-breaking for every existing implementor
    /// and mock (mirrors [`arg_ref_extent`](CallArgs::arg_ref_extent) and the
    /// earlier RFC 0001/0002 roll-outs). Only [`EngineArgs`](../../xl_engine/index.html),
    /// which can map an argument to its reference and read the cell store, and the
    /// test mocks override it. See `rfcs/0006-in-rectangle-absolute-read.md`.
    fn cell_at(&mut self, index: usize, row: u32, col: u32) -> Option<Value> {
        let _ = (index, row, col);
        None
    }
}

/// Walk an argument's rows for a positional function, transparently spanning the
/// bounded and unbounded-whole-column cases.
///
/// Runs the dense [`CallArgs::for_each_row`] walk first — so a bounded range,
/// array constant, or scalar behaves **exactly** as before, blanks surfaced at
/// their positions. If that walk *refuses* (an unbounded whole-column range, per
/// its documented guardrail), this falls back to the sparse
/// [`CallArgs::for_each_used_row`] walk over the populated rows. `visit` receives
/// each row's 0-based index relative to the range top plus the row slice; the
/// dense path numbers rows `0, 1, 2, …` (its natural order), matching the
/// used-extent path's relative indexing.
///
/// Returns `Ok(true)` if the **used-extent** (sparse) path was taken, `Ok(false)`
/// if the dense path was, or `Err` if neither could materialise the argument (a
/// whole-row range, an unresolvable range). The `bool` lets a consumer apply the
/// used-extent-only guards (a `Blank` lookup key, a blank-matching criterion —
/// see `rfcs/0001`) precisely on the path where they matter.
///
/// The dense walk never visits a row before returning `Err` (it checks
/// boundedness up front), so on the fallback there is no double-visit and any
/// accumulator the caller mutates in `visit` is still clean.
pub(crate) fn for_each_row_or_used(
    args: &mut dyn CallArgs,
    index: usize,
    visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
) -> Result<bool, ErrorKind> {
    let mut rel: u32 = 0;
    let dense = args.for_each_row(index, &mut |row| {
        let flow = visit(rel, row);
        rel = rel.saturating_add(1);
        flow
    });
    match dense {
        Ok(()) => Ok(false),
        Err(_) => args.for_each_used_row(index, visit).map(|()| true),
    }
}

/// Effective argument shape for the aggregate coercion decision (RFC 0010).
///
/// A single-cell **reference** is a reference, not a scalar literal: Excel's math
/// aggregates IGNORE text/logicals in a reference (single- *or* multi-cell),
/// while a scalar **literal** coerces (`SUM("5")` = 5, `SUM("abc")` = `#VALUE!`,
/// `SUM(TRUE)` = 1). [`CallArgs::shape`] alone cannot tell them apart — both a
/// 1×1 reference and a literal report [`ArgShape::Scalar`] — so this collapses
/// `shape` + [`CallArgs::arg_ref_extent`] into one decision: a `Scalar`-shaped
/// argument that resolves to a reference (`arg_ref_extent` is `Some`) aggregates
/// like a range; otherwise it is a literal / computed scalar.
///
/// A consumer routes [`EffShape::Aggregate`] through its own **range arm**
/// (`for_each_cell` under [`xl_value::CoercionMode::RangeAggregate`]) and
/// [`EffShape::ScalarLiteral`] through its **scalar arm** (`eval_scalar` under
/// [`xl_value::CoercionMode::Scalar`]) — the reroute reuses each function's
/// already-pinned reference semantics, so it adds no new behavior of its own.
///
/// Residual (unchanged, loud): `@A1` implicit-intersection and `A1#` spill
/// operands are not detected as references here (`arg_ref_extent` = `None`) and
/// keep today's scalar behavior; unions / 3-D / multi-area likewise fall to
/// `ScalarLiteral`'s existing path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EffShape {
    Omitted,
    ScalarLiteral,
    Aggregate,
}

/// Classify argument `index` for RFC-0010 aggregate coercion. See [`EffShape`].
pub(crate) fn eff_shape(args: &mut dyn CallArgs, index: usize) -> EffShape {
    match args.shape(index) {
        ArgShape::Omitted => EffShape::Omitted,
        ArgShape::Range | ArgShape::Array => EffShape::Aggregate,
        // A 1×1 reference is a reference (aggregate); a literal / computed scalar
        // is not.
        ArgShape::Scalar => {
            if args.arg_ref_extent(index).is_some() {
                EffShape::Aggregate
            } else {
                EffShape::ScalarLiteral
            }
        }
    }
}

/// If argument `index` is a **scalar-literal error** — a directly-written error
/// value such as the `#REF!` in `SUMIF(#REF!, ">0")` (an `EffShape::ScalarLiteral`
/// that evaluates to `Value::Error`) — return its [`ErrorKind`]. This is the
/// general error-propagation contract (a function argument that *is* an error
/// propagates — SUM.md / OXP-082), applied to the range-shaped arguments of the
/// conditional-aggregate family (`SUMIF`/`SUMIFS`/…), whose walkers would
/// otherwise treat the lone error as a non-matching cell and silently return `0`
/// (a *Never-silently-wrong* violation — the range argument of a formula whose
/// source reference was deleted).
///
/// Returns `None` for every non-literal shape — a **single-cell reference** whose
/// cell holds an error (`EffShape::Aggregate` via a `1×1` ref extent) and a
/// **multi-cell range** containing an error cell (`ArgShape::Range`) are
/// deliberately *not* caught here: whether an error *cell* inside a criteria
/// range propagates or is excluded is a separate, independently-pinned question
/// (see `func_sumif`'s `genuine_error_in_tested_cell_still_excluded_unchanged`
/// and the queued OXP), which this helper leaves untouched.
pub(crate) fn scalar_literal_error(args: &mut dyn CallArgs, index: usize) -> Option<ErrorKind> {
    // Evaluate under the array-context gate: the argument sits in a
    // range/array position, so an operator expression over a range inside it
    // (`SUMIF(B1:B7*1,">3")`) must materialize (and then be refused by the
    // walk, loudly) rather than implicit-intersect to the host-row-dependent
    // `#VALUE!`/value that scalar context produced.
    if eff_shape(args, index) == EffShape::ScalarLiteral {
        match args.eval_scalar_array_arg(index) {
            Value::Error(k) => return Some(k),
            // The gate materialized a multi-cell array (an operator, `IF`, or
            // `CHOOSE` over ranges in a criteria/sum position). No walk consumes
            // such an array for this function, so it must surface as a loud
            // `#UNSUPPORTED!` here — never fall through to a scalar-context
            // re-evaluation that would intersect it into a silent number.
            Value::Array(a) if a.as_scalar().is_none() => return Some(ErrorKind::Unsupported),
            _ => {}
        }
    }
    None
}

/// Walk an argument's cells for an **order-independent aggregate**, transparently
/// spanning the bounded, whole-**column**, *and* whole-**row** cases (RFC 0008).
///
/// Extends [`for_each_row_or_used`] with the column used-extent fallback: it runs
/// the dense walk (bounded / array / scalar), then the used-extent **row** walk
/// (whole-column, `A:A`), then the used-extent **column** walk (whole-row,
/// `1:1`). The dense walk runs **once** — the whole-row fallback is reached only
/// after `for_each_row_or_used` has *already* refused (dense and used-row both
/// refuse a whole-row range up front, visiting nothing), so there is no
/// double-visit and no double dense walk.
///
/// Returns `Ok(true)` if **either** used-extent path (row or column) was taken,
/// `Ok(false)` for the dense path, or `Err` if none could materialise the
/// argument (an unresolvable range). The `bool` lets a consumer apply the
/// used-extent-only guards (a blank-matching criterion — see `rfcs/0001` §3 and
/// `rfcs/0008` §3) precisely on the path where they matter.
///
/// **Orientation contract (RFC 0008 §2).** The dense / used-row branches yield
/// **row** slices; the used-col branch yields **column** slices. This helper is
/// therefore for **orientation-insensitive** consumers only — an order-
/// independent aggregate (`SUMIF`/`COUNTIF`/`AVERAGEIF` summing every cell of the
/// slice regardless of position) is unaffected by the difference. An
/// orientation-*sensitive* consumer (a positional lockstep, or `MATCH`'s
/// axis-dependent blank rule) must **not** use this helper; it must branch on the
/// axis explicitly (see `func_sumif`'s `indexed_cols` and `func_match`'s
/// column-path fallback).
pub(crate) fn for_each_row_or_used_any_axis(
    args: &mut dyn CallArgs,
    index: usize,
    visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
) -> Result<bool, ErrorKind> {
    match for_each_row_or_used(args, index, visit) {
        Ok(used) => Ok(used),
        // The row path refused (dense + used-row): a whole-row range (unbounded
        // columns) or an unresolvable range. Try the column used-extent walk; it
        // serves the whole-row shape and refuses the rest (propagated as Err).
        Err(_) => args.for_each_used_col(index, visit).map(|()| true),
    }
}
