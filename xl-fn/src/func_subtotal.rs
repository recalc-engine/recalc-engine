//! `SUBTOTAL` — a dispatcher aggregate: `SUBTOTAL(function_num, ref1, [ref2], …)`
//! selects one of the classic aggregates by number and applies it to the refs.
//!
//! # Provenance
//! Behavior contract: `docs/specs/SUBTOTAL.md`, which cites the Microsoft Learn
//! SUBTOTAL function page (fetched/verified 2026-07-08). This module implements
//! only the faithful subset that page pins; everything else returns
//! `#UNSUPPORTED!` per the "never silently wrong" rule.
//!
//! # What is implemented (`function_num` ∈ {1..11, 101..111})
//! Selectors `1,2,3,4,5,6,9` dispatch to the *existing* xl-fn aggregate over
//! `ref1..` (arguments 1..N), reusing its exact coercion/error semantics.
//! Selectors `7,8,10,11` (STDEV/STDEVP/VAR/VARP) — resolved by oracle run
//! `RUN-2026-07-11-oracle01` (OXP-120) — are computed in-module by
//! [`stat_reduce`], which collects numbers through the *same* frozen
//! coercion contract the delegates use:
//!
//! | function_num | aggregate | implementation |
//! |---|---|---|
//! | 1  | AVERAGE  | [`crate::func_average`] |
//! | 2  | COUNT    | [`crate::func_count`]   |
//! | 3  | COUNTA   | [`crate::func_counta`]  |
//! | 4  | MAX      | [`crate::func_max`]     |
//! | 5  | MIN      | [`crate::func_min`]     |
//! | 6  | PRODUCT  | [`crate::func_product`] |
//! | 7  | STDEV    | [`stat_reduce`] (sample stdev)      |
//! | 8  | STDEVP   | [`stat_reduce`] (population stdev)  |
//! | 9  | SUM      | [`crate::func_sum`]     |
//! | 10 | VAR      | [`stat_reduce`] (sample variance)      |
//! | 11 | VARP     | [`stat_reduce`] (population variance)   |
//!
//! The `101`–`111` forms select the **same** aggregate as their `1`–`11` twin
//! (`101→AVERAGE … 109→SUM … 111→VARP`, i.e. `code − 100`) and additionally
//! **exclude manually-hidden rows** (`RUN-2026-07-11-oracle01`, OXP-121). Over
//! `A1:A5 = 10,20,30,40,50` with row 3 hidden, `=SUBTOTAL(109,A1:A5) = 120`
//! (drops the hidden `30`) while the `1`–`11` twin `=SUBTOTAL(9,A1:A5) = 150`
//! keeps it. This rides the same RFC 0002 provenance channel as the
//! nested-SUBTOTAL exclusion — see "Hidden-row exclusion" below.
//!
//! Dispatch is done by wrapping the incoming [`CallArgs`] in [`OffsetArgs`],
//! which re-indexes so that argument 1 (`ref1`) appears as argument 0 to the
//! delegate — the delegate then sees exactly the ref list and none of the
//! `function_num` selector. This means SUBTOTAL inherits, for free, each
//! aggregate's whole-column/array/error/empty behavior and any oracle-deferred
//! (`OXP-…`) cases those modules already document.
//!
//! # Selector validation (`RUN-2026-07-11-oracle01`, OXP-122)
//! - A **non-integer** `function_num` is **truncated toward zero** before
//!   validation: `=SUBTOTAL(9.5, …)` sums (`9.5 → 9`). [OXP-122.]
//! - An **out-of-range** integer selector (`0`, `12`–`100`, `112+`, negatives)
//!   returns **`#VALUE!`**: `=SUBTOTAL(0,…)` and `=SUBTOTAL(12,…)` both observed
//!   `#VALUE!`. [OXP-122.] (An **error** selector still propagates its own kind,
//!   and a non-numeric text selector is `#VALUE!` via the coercion contract.)
//!
//! # Deferred to `#UNSUPPORTED!` (loud, never silently wrong)
//! - **The too-few-numbers degenerate case of `7/8/10/11` (and `107/108/110/111`).**
//!   [`stat_reduce`] needs ≥2 numbers for the sample forms (divisor `n-1`) and
//!   ≥1 for the population forms; fewer was not probed by OXP-120 (Excel raises
//!   `#DIV/0!`, which is not oracle-pinned here), so it stays `#UNSUPPORTED!`.
//!
//! # Hidden-row exclusion for `101`–`111` (`OXP-121`, via RFC 0002)
//! `xl-io` parses each worksheet's `<row hidden="1">` rows; `xl-engine` flattens
//! them into a `(sheet, row)` set and, on the RFC 0002 provenance-tagged cell
//! walk ([`CallArgs::for_each_cell_tagged`]), flags every streamed cell that sits
//! in a hidden row with [`CellFlags::is_hidden_row`]. For a `101`–`111` selector
//! this module builds its [`NestedSubtotalFilter`] with `exclude_hidden_rows =
//! true`, so the filter drops those flagged cells (alongside the always-dropped
//! nested-`SUBTOTAL` cells) before the delegate aggregate sees them. The `1`–`11`
//! forms build the filter with `exclude_hidden_rows = false`, so they keep hidden
//! rows — matching Excel. This is the exact channel RFC 0002 reserved
//! `is_hidden_row` for.
//!
//! ## ⚠ Latent `1`–`11` divergence under an active AutoFilter (documented, not fixed)
//! OOXML's single `<row hidden="1">` bit conflates *manually*-hidden rows and
//! rows hidden by an **active AutoFilter** — the file does not distinguish them.
//! For `101`–`111` this is correct: Excel excludes *both* causes, so dropping
//! every `is_hidden_row` cell matches. But Excel *also* excludes **filter**-hidden
//! rows from the `1`–`11` forms (only *manually*-hidden rows are kept there), and
//! this module — lacking any manual-vs-filter distinction in the data — keeps
//! **all** hidden rows for `1`–`11`. So on a workbook with an active AutoFilter,
//! a `1`–`11` `SUBTOTAL` over a filtered range can *over-count* relative to Excel
//! (it includes filter-hidden rows Excel would drop). This is a **known, bounded
//! divergence**, not a silent-correctness claim: no `1`–`11` form here is
//! filter-aware, and `docs/specs/SUBTOTAL.md` records it. Distinguishing the two
//! hidden-row causes needs AutoFilter state `xl-io` does not yet surface.
//!
//! `// OXP (unassigned)` — probe `SUBTOTAL(9, range)` vs `SUBTOTAL(109, range)`
//! over a range with an **active AutoFilter** hiding some rows, to pin whether
//! (and by how much) the `1`–`11` forms must also exclude filter-hidden rows and
//! to obtain the manual-vs-filter signal Recalc would need to close this gap.
//!
//! # Nested-SUBTOTAL exclusion is modeled (`OXP-123`, via RFC 0002)
//! The page states nested SUBTOTALs inside `ref1, ref2,…` are ignored to avoid
//! double-counting, so a grand-total `SUBTOTAL(9, …)` over a column that already
//! contains sub-`SUBTOTAL`s does not double-count. This is now implemented via
//! the RFC 0002 cell-provenance channel: `xl-engine` tags every cell whose own
//! formula head is a `SUBTOTAL` call ([`CellFlags::is_subtotal`]), and this
//! module wraps its refs in [`NestedSubtotalFilter`], which streams them through
//! [`CallArgs::for_each_cell_tagged`] and drops the flagged cells before the
//! delegate aggregate (`SUM`/`AVERAGE`/… via [`CallArgs::for_each_cell`]) ever
//! sees them. A referenced cell is excluded whether it is a whole range member
//! or a single-cell ref.
//!
//! The **lone-scalar** nested case — `SUBTOTAL(9, A3)` where `A3` is *itself* a
//! SUBTOTAL passed as a bare single-cell argument — is now also excluded
//! (`RUN-2026-07-11-oracle01`, OXP-123: Excel returns `0`, the cell dropped).
//! A single-cell reference is *scalar*-shaped, so the delegate would pull it via
//! [`CallArgs::eval_scalar`] (no provenance) and double-count it. To close that,
//! [`NestedSubtotalFilter::shape`] peeks a scalar argument's single
//! provenance-tagged cell and, if it is a SUBTOTAL, reclassifies the argument as
//! [`ArgShape::Omitted`] so the delegate drops it entirely — matching the
//! observed `0`.
//!
//! ## Residual cases (documented, bounded)
//! 1. **No-provenance shapes.** [`CallArgs::for_each_cell_tagged`] refuses
//!    argument shapes that carry no per-cell provenance — an **array constant**
//!    (`{1,2,3}` has cells but no originating formula), or any [`CallArgs`]
//!    implementation that does not opt into the channel (e.g. the unit-test
//!    mock). For those, [`NestedSubtotalFilter`] falls back to the plain
//!    [`CallArgs::for_each_cell`] aggregate (and leaves scalar classification
//!    unchanged). That is correct, not a silent gap: a shape with no originating
//!    formulas cannot contain a nested SUBTOTAL, so there is nothing to exclude.
//!
//! The exclusion therefore covers both the range/defined-name Data ▸ Subtotal
//! layout the RFC targets *and* the lone-scalar single-cell reference (OXP-123).

use std::ops::ControlFlow;

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg, to_number};

use crate::args::{ArgShape, CallArgs, CellFlags, EffShape, RefRect, eff_shape};
use crate::context::EvalContext;
use crate::registry::EvalFn;

/// Collect every number the refs contribute, reusing the frozen `xl-value`
/// coercion contract (the same one `SUM`/`AVERAGE` use), then reduce it to a
/// sample/population variance or standard deviation.
///
/// `sample` selects the divisor (`n-1` for the sample forms `STDEV`/`VAR`, `n`
/// for the population forms `STDEVP`/`VARP`); `do_sqrt` selects standard
/// deviation (`true`) vs variance (`false`). Number collection mirrors
/// [`crate::func_sum`] exactly — direct scalars coerce under
/// [`CoercionMode::Scalar`], range/array cells under
/// [`CoercionMode::RangeAggregate`] (only real numbers count) — so these forms
/// inherit the identical error short-circuit and text/blank/logical skipping,
/// rather than a hand-invented coercion. Because collection routes through
/// [`CallArgs::for_each_cell`], the interposed [`NestedSubtotalFilter`] still
/// drops nested-`SUBTOTAL` cells here too.
///
/// # Provenance
/// `function_num` ∈ {7,8,10,11} resolved to STDEV/STDEVP/VAR/VARP by oracle run
/// `RUN-2026-07-11-oracle01` (OXP-120): over `A1:A5 = 10,20,30,40,50`,
/// `SUBTOTAL(7,…)=15.811388300841896`, `SUBTOTAL(8,…)=14.142135623730951`,
/// `SUBTOTAL(10,…)=250`, `SUBTOTAL(11,…)=200` — the textbook sample/population
/// definitions. Only that pure-numeric shape is oracle-pinned; the degenerate
/// too-few-numbers case is *not* probed, so it stays loud (`#UNSUPPORTED!`)
/// rather than guessing Excel's `#DIV/0!`.
fn stat_reduce(args: &mut dyn CallArgs, sample: bool, do_sqrt: bool) -> Value {
    let mut xs: Vec<f64> = Vec::new();

    for i in 0..args.count() {
        // RFC 0010: a single-cell REFERENCE aggregates like a range (text/logical
        // ignored), matching `func_sum`; only a scalar LITERAL coerces. The
        // ref-position channel `eff_shape` reads is forwarded through
        // `OffsetArgs`/`NestedSubtotalFilter`, so the reroute reaches this
        // in-module delegate too.
        match eff_shape(args, i) {
            EffShape::Omitted => {}
            EffShape::ScalarLiteral => {
                match coerce_number_arg(&args.eval_scalar_array_arg(i), CoercionMode::Scalar) {
                    NumericArg::Number(n) => xs.push(n),
                    NumericArg::Skip => {}
                    NumericArg::Error(k) => return Value::Error(k),
                }
            }
            EffShape::Aggregate => {
                let mut err: Option<ErrorKind> = None;
                let xs_ref = &mut xs;
                args.for_each_cell(i, &mut |v| match coerce_number_arg(
                    v,
                    CoercionMode::RangeAggregate,
                ) {
                    NumericArg::Number(n) => {
                        xs_ref.push(n);
                        ControlFlow::Continue(())
                    }
                    NumericArg::Skip => ControlFlow::Continue(()),
                    NumericArg::Error(k) => {
                        err = Some(k);
                        ControlFlow::Break(())
                    }
                });
                if let Some(k) = err {
                    return Value::Error(k);
                }
            }
        }
    }

    // Sample forms need ≥2 numbers (divisor `n-1`), population forms ≥1. Fewer
    // is a degenerate case OXP-120 did not probe (Excel raises `#DIV/0!`, but
    // that is not oracle-pinned here) — stay loud rather than guess.
    let min = if sample { 2 } else { 1 };
    if xs.len() < min {
        return Value::Error(ErrorKind::Unsupported);
    }

    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    let ss: f64 = xs.iter().map(|x| (x - mean) * (x - mean)).sum();
    let variance = ss / if sample { n - 1.0 } else { n };
    Value::number(if do_sqrt { variance.sqrt() } else { variance })
}

/// `SUBTOTAL(7, …)` → `STDEV` (sample standard deviation). OXP-120.
fn eval_stdev(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    stat_reduce(args, true, true)
}
/// `SUBTOTAL(8, …)` → `STDEVP` (population standard deviation). OXP-120.
fn eval_stdevp(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    stat_reduce(args, false, true)
}
/// `SUBTOTAL(10, …)` → `VAR` (sample variance). OXP-120.
fn eval_var(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    stat_reduce(args, true, false)
}
/// `SUBTOTAL(11, …)` → `VARP` (population variance). OXP-120.
fn eval_varp(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    stat_reduce(args, false, false)
}

/// A [`CallArgs`] view that hides the first `offset` arguments of an inner
/// arg list, re-indexing the remainder from 0.
///
/// SUBTOTAL uses `offset = 1` so a delegate aggregate sees `ref1, ref2, …`
/// (the SUBTOTAL arguments at indices 1..N) as *its* arguments 0..N-1, with the
/// `function_num` selector at index 0 hidden. Every method simply forwards to
/// the inner args with the index shifted, so the delegate's streaming,
/// dimensioning, and whole-column handling are preserved unchanged.
struct OffsetArgs<'a> {
    inner: &'a mut dyn CallArgs,
    offset: usize,
}

/// SUBTOTAL's SUM-only born-refusing guard (M2 lane 6): a value argument that
/// evaluates to a **multi-cell** consumed array becomes `#UNSUPPORTED!`, so
/// SUBTOTAL (whose `9`/`109` delegate is `func_sum::eval`, which would otherwise
/// fold it) stays loud — SUBTOTAL is not an OXP-201-pinned aggregator. A scalar,
/// a 1×1 array, or an error passes through unchanged.
fn refuse_consumed_array(v: Value) -> Value {
    match &v {
        Value::Array(a) if a.as_scalar().is_none() => Value::Error(ErrorKind::Unsupported),
        _ => v,
    }
}

impl CallArgs for OffsetArgs<'_> {
    fn count(&self) -> usize {
        self.inner.count().saturating_sub(self.offset)
    }
    fn shape(&mut self, index: usize) -> ArgShape {
        self.inner.shape(index + self.offset)
    }
    fn eval_scalar(&mut self, index: usize) -> Value {
        self.inner.eval_scalar(index + self.offset)
    }
    // RFC-0011: forward the array-context evaluation (index-shifted) explicitly —
    // the default would fall back to plain `eval_scalar` and silently drop the
    // array context for SUBTOTAL's value args.
    //
    // M2 lane 6 born-refusing boundary: SUBTOTAL is **not** an OXP-201-pinned
    // aggregator, but `function_num` 9/109 delegates to `func_sum::eval`, which
    // *does* fold a consumed array. Evaluating in array context (above) keeps a
    // range-producing value arg LOUD rather than silently implicit-intersecting;
    // this guard then converts the materialized multi-cell array to
    // `#UNSUPPORTED!` before the SUM delegate can fold it — so SUBTOTAL stays
    // refused exactly as under RFC-0011 (SUM-only fold; the spec's §2d "stays
    // loud for free via coerce_number_arg(Scalar)" assumption does not hold for
    // the SUM delegate). `NestedSubtotalFilter` forwards through this method, so
    // one guard point covers every SUBTOTAL delegate.
    fn eval_scalar_array_arg(&mut self, index: usize) -> Value {
        refuse_consumed_array(self.inner.eval_scalar_array_arg(index + self.offset))
    }
    // M2 lane 6: forward the array-context signal so a delegate's `IF`/`ISBLANK`/
    // `NOT`/operator element-wise paths see it (the default `false` would drop it).
    fn array_arg_ctx(&self) -> bool {
        self.inner.array_arg_ctx()
    }
    fn for_each_cell(&mut self, index: usize, visit: &mut dyn FnMut(&Value) -> ControlFlow<()>) {
        self.inner.for_each_cell(index + self.offset, visit);
    }
    fn dims(&mut self, index: usize) -> Option<(u32, u32)> {
        self.inner.dims(index + self.offset)
    }
    fn for_each_row(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(&[Value]) -> ControlFlow<()>,
    ) -> Result<(), ErrorKind> {
        self.inner.for_each_row(index + self.offset, visit)
    }
    fn for_each_used_row(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
    ) -> Result<(), ErrorKind> {
        self.inner.for_each_used_row(index + self.offset, visit)
    }
    fn for_each_cell_tagged(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(&Value, CellFlags) -> ControlFlow<()>,
    ) -> Result<(), ()> {
        self.inner.for_each_cell_tagged(index + self.offset, visit)
    }
    // Forward the RFC-0005 reference-position channel (index-shifted) so the
    // delegate's `eff_shape` can see that a `ref1` slot is a single-cell
    // reference and route it through its range arm (RFC 0010). Without this, the
    // wrapper would hide the ref extent (default `None`) and a bare `SUBTOTAL(9,
    // A1)` would coerce `A1` as a scalar literal instead of aggregating it.
    fn arg_ref_extent(&mut self, index: usize) -> Option<RefRect> {
        self.inner.arg_ref_extent(index + self.offset)
    }
}

/// A [`CallArgs`] view that excludes nested-`SUBTOTAL` cells — and, for the
/// `101`–`111` forms, hidden-row cells — from range/array aggregation (RFC 0002
/// / OXP-121), interposed between [`OffsetArgs`] and the delegate aggregate so
/// the delegate's coercion/error semantics are otherwise untouched.
///
/// The delegates (`SUM`/`AVERAGE`/`COUNT`/`COUNTA`/`MAX`/`MIN`/`PRODUCT` and the
/// in-module `stat_reduce` for STDEV/…) pull every ref through
/// [`CallArgs::for_each_cell`]. This wrapper overrides *only* that method: it
/// routes through the provenance-carrying [`CallArgs::for_each_cell_tagged`] and
/// drops any cell flagged [`CellFlags::is_subtotal`] before the delegate ever
/// sees it, so a grand-total `SUBTOTAL` does not double-count the sub-`SUBTOTAL`s
/// inside its refs (Excel's nested-subtotal exclusion). When
/// `exclude_hidden_rows` is set (the `101`–`111` `function_num` forms) it *also*
/// drops any cell flagged [`CellFlags::is_hidden_row`], so those forms exclude
/// manually-hidden rows (OXP-121). Every other method forwards verbatim, so
/// shape classification, scalar coercion, and the `function_num` arm are all
/// inherited unchanged.
struct NestedSubtotalFilter<'a> {
    inner: &'a mut dyn CallArgs,
    /// `true` only for the `101`–`111` `function_num` forms: additionally drop
    /// cells flagged [`CellFlags::is_hidden_row`] (manually-hidden rows —
    /// OXP-121). `false` for `1`–`11`, which include hidden rows.
    exclude_hidden_rows: bool,
}

impl CallArgs for NestedSubtotalFilter<'_> {
    fn count(&self) -> usize {
        self.inner.count()
    }
    fn shape(&mut self, index: usize) -> ArgShape {
        let shape = self.inner.shape(index);
        // OXP-123 (RUN-2026-07-11-oracle01): a nested SUBTOTAL referenced as a
        // lone *scalar* single-cell argument must also be excluded. Excel's
        // `=SUBTOTAL(9, A3)` where `A3` is itself a SUBTOTAL returns 0 (the cell
        // is dropped), not the double-counted value. Scalar-shaped arguments
        // bypass `for_each_cell` — the delegate pulls them via `eval_scalar`,
        // which the range-oriented exclusion never sees — so detect a scalar
        // whose single provenance-tagged cell is a SUBTOTAL and report it
        // `Omitted`, dropping it exactly as the range walk drops an in-range
        // nested SUBTOTAL. Only `Scalar` args are peeked, and the tagged walk of
        // a scalar streams at most one cell, so ranges are untouched and the peek
        // is O(1). (A non-SUBTOTAL single-cell ref re-reads the value store once,
        // which is idempotent; a computed scalar sub-expression is evaluated once
        // here and again by the delegate — harmless, and such cells never carry
        // SUBTOTAL provenance so they are never reclassified.)
        if shape == ArgShape::Scalar {
            let mut is_nested = false;
            let tagged = self.inner.for_each_cell_tagged(index, &mut |_v, flags| {
                if flags.is_subtotal {
                    is_nested = true;
                }
                ControlFlow::Continue(())
            });
            if tagged.is_ok() && is_nested {
                return ArgShape::Omitted;
            }
            // Note: this scalar peek reclassifies only a *nested SUBTOTAL* scalar
            // (oracle-pinned by OXP-123). Hidden-row exclusion for the 101–111
            // forms is NOT decided here. Post-RFC-0010, a bare single-cell
            // *reference* argument is routed to the aggregate path by `eff_shape`
            // (`arg_ref_extent` = Some), so it streams through `for_each_cell`
            // below and a hidden-row cell IS dropped — the reference≡1×1-range
            // reading of OXP-121's range rule. Only a *computed* scalar (`A1+0`,
            // a function result — `arg_ref_extent` = None) stays a `ScalarLiteral`
            // and is left included (matching the 1–11 shape); its hidden-row
            // status is genuinely unprobed for that residual, and not guessed.
        }
        shape
    }
    fn eval_scalar(&mut self, index: usize) -> Value {
        self.inner.eval_scalar(index)
    }
    // RFC-0011: forward array-context evaluation explicitly (see OffsetArgs).
    fn eval_scalar_array_arg(&mut self, index: usize) -> Value {
        self.inner.eval_scalar_array_arg(index)
    }
    // M2 lane 6: forward the array-context signal (see OffsetArgs).
    fn array_arg_ctx(&self) -> bool {
        self.inner.array_arg_ctx()
    }
    fn for_each_cell(&mut self, index: usize, visit: &mut dyn FnMut(&Value) -> ControlFlow<()>) {
        // Preferred path: stream with provenance and skip nested SUBTOTAL cells
        // (always) plus manually-hidden-row cells (the 101–111 forms only).
        let exclude_hidden = self.exclude_hidden_rows;
        let tagged = self.inner.for_each_cell_tagged(index, &mut |v, flags| {
            if flags.is_subtotal || (exclude_hidden && flags.is_hidden_row) {
                // Excel ignores a referenced cell whose own formula is a
                // SUBTOTAL (nested-exclusion), and — for function_num 101–111 —
                // a cell sitting in a manually-hidden row (OXP-121). Skip it
                // (value *and* any error it holds).
                ControlFlow::Continue(())
            } else {
                visit(v)
            }
        });
        // Fallback (documented): the argument carries no per-cell provenance the
        // engine can tag — an array constant `{…}`, or a mock/`CallArgs` impl
        // that does not implement the RFC 0002 channel. Such shapes cannot
        // contain a nested-SUBTOTAL cell (they have no originating formulas), so
        // the plain aggregate over them is already correct.
        if tagged.is_err() {
            self.inner.for_each_cell(index, visit);
        }
    }
    fn dims(&mut self, index: usize) -> Option<(u32, u32)> {
        self.inner.dims(index)
    }
    fn for_each_row(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(&[Value]) -> ControlFlow<()>,
    ) -> Result<(), ErrorKind> {
        self.inner.for_each_row(index, visit)
    }
    fn for_each_used_row(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
    ) -> Result<(), ErrorKind> {
        self.inner.for_each_used_row(index, visit)
    }
    fn for_each_cell_tagged(
        &mut self,
        index: usize,
        visit: &mut dyn FnMut(&Value, CellFlags) -> ControlFlow<()>,
    ) -> Result<(), ()> {
        self.inner.for_each_cell_tagged(index, visit)
    }
    // Forward the RFC-0005 reference-position channel verbatim so a `ref` slot's
    // single-cell-reference-ness reaches the delegate's `eff_shape` (RFC 0010).
    // A nested-SUBTOTAL scalar is still dropped by `shape` reclassifying it to
    // `Omitted` (which `eff_shape` reads *before* the ref extent), so this
    // forward does not defeat the OXP-123 exclusion.
    fn arg_ref_extent(&mut self, index: usize) -> Option<RefRect> {
        self.inner.arg_ref_extent(index)
    }
}

/// Evaluate a `SUBTOTAL(...)` call. See the module docs for the supported
/// subset, the deferrals, and their spec provenance.
pub(crate) fn eval(ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // `function_num` is a direct scalar. `to_number` applies the frozen
    // xl-value coercion (Bool→1/0, Blank→0, numeric text→its number) and
    // *propagates* an error selector (e.g. `SUBTOTAL(#REF!, …)` → #REF!).
    let selector = args.eval_scalar(0);
    let n = match to_number(&selector) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };

    // Non-finite selector: unreachable via the normal value model (a numeric
    // cell can never hold ±∞/NaN — `Value::number` maps those to `#NUM!`), and
    // unprobed, so stay loud rather than invent a result.
    if !n.is_finite() {
        return Value::Error(ErrorKind::Unsupported);
    }

    // OXP-122 (RUN-2026-07-11-oracle01): a non-integer `function_num` is
    // truncated toward zero before validation — `=SUBTOTAL(9.5, A1:A3)` sums
    // (i.e. `9.5 → 9`). Truncate, then a saturating cast keeps huge magnitudes
    // out of the recognized set (they fall through to the invalid arm).
    let code = n.trunc() as i64;

    // OXP-121 (RUN-2026-07-11-oracle01): the `101`–`111` forms are the `1`–`11`
    // forms that *additionally* exclude manually-hidden rows — e.g. over
    // `A1:A5 = 10,20,30,40,50` with row 3 hidden, `SUBTOTAL(109,…) = 120`
    // (excludes the hidden 30) while `SUBTOTAL(9,…) = 150` (includes it).
    // Normalize a `101`–`111` selector to its `1`–`11` twin (`code - 100`) and
    // remember to drop hidden-row cells; the delegate table below is then shared
    // by both ranges.
    let (selector, exclude_hidden_rows) = match code {
        1..=11 => (code, false),
        101..=111 => (code - 100, true),

        // Any other (truncated) selector — 0, 12–100, 112+, negatives: OXP-122
        // pins `#VALUE!` for an invalid `function_num` (`=SUBTOTAL(0,…)` and
        // `=SUBTOTAL(12,…)` both observed `#VALUE!`).
        _ => return Value::Error(ErrorKind::Value),
    };

    let delegate: EvalFn = match selector {
        1 => crate::func_average::eval,
        2 => crate::func_count::eval,
        3 => crate::func_counta::eval,
        4 => crate::func_max::eval,
        5 => crate::func_min::eval,
        6 => crate::func_product::eval,
        9 => crate::func_sum::eval,

        // STDEV/STDEVP/VAR/VARP — resolved by OXP-120 to the textbook
        // sample/population aggregates; implemented in-module via `stat_reduce`,
        // reusing the frozen number-coercion contract (see its docs).
        7 => eval_stdev,
        8 => eval_stdevp,
        10 => eval_var,
        11 => eval_varp,

        // `selector` is guaranteed to be in `1..=11` by the normalization above.
        _ => unreachable!("selector normalized into 1..=11"),
    };

    // Apply the chosen aggregate to ref1.. (indices 1..N), with the selector at
    // index 0 hidden. `NestedSubtotalFilter` sits between the offset view and the
    // delegate so nested-SUBTOTAL cells are excluded (RFC 0002 / OXP-123) and —
    // for `101`–`111` (`exclude_hidden_rows`) — hidden-row cells too (OXP-121):
    // the delegate streams refs through `for_each_cell`, which the filter serves
    // from the provenance-tagged walk, dropping `is_subtotal` (and, when asked,
    // `is_hidden_row`) cells. Where the tagged walk refuses (array constants, or
    // a `CallArgs` without the channel) the filter falls back to the plain
    // aggregate for that argument — correct, since such shapes have no
    // originating rows to be hidden and cannot hold a nested SUBTOTAL.
    let mut refs = OffsetArgs {
        inner: args,
        offset: 1,
    };
    let mut filtered = NestedSubtotalFilter {
        inner: &mut refs,
        exclude_hidden_rows,
    };
    delegate(ctx, &mut filtered)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{TestArg, TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    // ---- Live function_nums (1-6, 9) dispatch to the right aggregate --------

    #[test]
    fn subtotal_9_is_sum() {
        // SUBTOTAL(9, {1,2,3}) = SUM = 6.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(9.0)), Range(vec![num(1.0), num(2.0), num(3.0)])]
            ),
            num(6.0)
        );
    }

    #[test]
    fn subtotal_1_is_average() {
        // SUBTOTAL(1, {2,4,6}) = AVERAGE = 4.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(1.0)), Range(vec![num(2.0), num(4.0), num(6.0)])]
            ),
            num(4.0)
        );
    }

    #[test]
    fn subtotal_2_is_count() {
        // COUNT counts only numbers: text and blank are skipped → 2.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(2.0)),
                    Range(vec![num(10.0), txt("x"), Value::Blank, num(20.0)])
                ]
            ),
            num(2.0)
        );
    }

    #[test]
    fn subtotal_3_is_counta() {
        // COUNTA counts every non-blank: number + text + number = 3.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(3.0)),
                    Range(vec![num(10.0), txt("x"), Value::Blank, num(20.0)])
                ]
            ),
            num(3.0)
        );
    }

    #[test]
    fn subtotal_4_is_max() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(4.0)), Range(vec![num(3.0), num(9.0), num(5.0)])]
            ),
            num(9.0)
        );
    }

    #[test]
    fn subtotal_5_is_min() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(5.0)), Range(vec![num(3.0), num(9.0), num(5.0)])]
            ),
            num(3.0)
        );
    }

    #[test]
    fn subtotal_6_is_product() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(num(6.0)), Range(vec![num(2.0), num(3.0), num(4.0)])]
            ),
            num(24.0)
        );
    }

    #[test]
    fn subtotal_spans_multiple_refs() {
        // ref1 and ref2 both aggregate: SUM(1,2) over ref1 + 3 scalar = 6.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(9.0)),
                    Range(vec![num(1.0), num(2.0)]),
                    Scalar(num(3.0))
                ]
            ),
            num(6.0)
        );
    }

    #[test]
    fn selector_coerced_from_numeric_text() {
        // function_num given as the text "9" coerces to 9 → SUM.
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("9")), Range(vec![num(4.0), num(6.0)])]
            ),
            num(10.0)
        );
    }

    #[test]
    fn selector_coerced_from_bool() {
        // TRUE → 1 → AVERAGE of {2,4,6} = 4.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Bool(true)),
                    Range(vec![num(2.0), num(4.0), num(6.0)])
                ]
            ),
            num(4.0)
        );
    }

    #[test]
    fn range_error_cell_propagates_through_delegate() {
        // SUM's first-error short-circuit is inherited: a #DIV/0! cell in the
        // ref propagates out of SUBTOTAL.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(9.0)),
                    Range(vec![num(1.0), Value::Error(ErrorKind::Div0), num(3.0)])
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    // ---- Statistical aggregates (7,8,10,11) — OXP-120 -----------------------

    /// The exact A1:A5 = 10,20,30,40,50 probe of OXP-120.
    fn stat_range() -> TestArg {
        Range(vec![num(10.0), num(20.0), num(30.0), num(40.0), num(50.0)])
    }

    #[test]
    fn subtotal_7_is_stdev_sample() {
        // RUN-2026-07-11-oracle01 / OXP-120: =SUBTOTAL(7,A1:A5) = 15.811388300841896.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(7.0)), stat_range()]),
            num(15.811388300841896)
        );
    }

    #[test]
    fn subtotal_8_is_stdevp_population() {
        // RUN-2026-07-11-oracle01 / OXP-120: =SUBTOTAL(8,A1:A5) = 14.142135623730951.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(8.0)), stat_range()]),
            num(14.142135623730951)
        );
    }

    #[test]
    fn subtotal_10_is_var_sample() {
        // RUN-2026-07-11-oracle01 / OXP-120: =SUBTOTAL(10,A1:A5) = 250.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(10.0)), stat_range()]),
            num(250.0)
        );
    }

    #[test]
    fn subtotal_11_is_varp_population() {
        // RUN-2026-07-11-oracle01 / OXP-120: =SUBTOTAL(11,A1:A5) = 200.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(11.0)), stat_range()]),
            num(200.0)
        );
    }

    #[test]
    fn stat_too_few_numbers_is_unsupported() {
        // OXP-120 did not probe the degenerate case; the sample forms need ≥2
        // numbers (divisor n-1). Fewer stays loud rather than guessing #DIV/0!.
        for f in [7.0, 10.0] {
            assert_eq!(
                eval_direct(eval, vec![Scalar(num(f)), Range(vec![num(1.0)])]),
                Value::Error(ErrorKind::Unsupported),
                "sample form {f} with <2 numbers should stay #UNSUPPORTED!"
            );
        }
        // Population forms need ≥1 number; an empty range stays loud too.
        for f in [8.0, 11.0] {
            assert_eq!(
                eval_direct(eval, vec![Scalar(num(f)), Range(vec![txt("x")])]),
                Value::Error(ErrorKind::Unsupported),
                "population form {f} with 0 numbers should stay #UNSUPPORTED!"
            );
        }
    }

    // ---- The 101-111 range maps to its 1-11 twin (OXP-121) ------------------

    #[test]
    fn range_101_to_111_maps_to_its_1_to_11_twin() {
        // OXP-121 (RUN-2026-07-11-oracle01): 101–111 select the same aggregate as
        // their 1–11 twin (`code − 100`). This mock (`test_support`) carries no
        // hidden-row provenance — it refuses the tagged walk — so here they
        // compute exactly like their twin; the hidden-row *exclusion* is proven by
        // the tagged-mock tests below and end-to-end in xl-engine. None of them
        // return #VALUE!/#UNSUPPORTED! any longer.
        // 109 → SUM {1,2,3} = 6.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(109.0)),
                    Range(vec![num(1.0), num(2.0), num(3.0)])
                ]
            ),
            num(6.0)
        );
        // 101 → AVERAGE {2,4,6} = 4.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(101.0)),
                    Range(vec![num(2.0), num(4.0), num(6.0)])
                ]
            ),
            num(4.0)
        );
        // 104 → MAX {3,9,5} = 9.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(104.0)),
                    Range(vec![num(3.0), num(9.0), num(5.0)])
                ]
            ),
            num(9.0)
        );
        // 107 → STDEV (sample) over the OXP-120 A1:A5 range.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(107.0)),
                    Range(vec![num(10.0), num(20.0), num(30.0), num(40.0), num(50.0)])
                ]
            ),
            num(15.811388300841896)
        );
    }

    // ---- Invalid / non-integer selectors — OXP-122 -------------------------

    #[test]
    fn out_of_range_selector_is_value_error() {
        // RUN-2026-07-11-oracle01 / OXP-122: =SUBTOTAL(0,…) and =SUBTOTAL(12,…)
        // both observed #VALUE!. Other out-of-range integers share the arm.
        for f in [0.0, 12.0, 100.0, 112.0, -5.0] {
            assert_eq!(
                eval_direct(eval, vec![Scalar(num(f)), Range(vec![num(1.0), num(2.0)])]),
                Value::Error(ErrorKind::Value),
                "function_num {f} is invalid → #VALUE!"
            );
        }
    }

    #[test]
    fn non_integer_selector_is_truncated_toward_zero() {
        // RUN-2026-07-11-oracle01 / OXP-122: =SUBTOTAL(9.5, A1:A3) sums (9.5 → 9);
        // over {10,20,30} that is 60.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(9.5)),
                    Range(vec![num(10.0), num(20.0), num(30.0)])
                ]
            ),
            num(60.0)
        );
    }

    // ---- function_num error propagation ------------------------------------

    #[test]
    fn selector_error_propagates() {
        // An error function_num (e.g. #REF!) propagates its own kind, not a
        // generic #UNSUPPORTED!.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Error(ErrorKind::Ref)),
                    Range(vec![num(1.0), num(2.0)])
                ]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    #[test]
    fn non_numeric_text_selector_is_value_error() {
        // A non-numeric text selector fails to_number → #VALUE! (the frozen
        // coercion contract, not a guess about SUBTOTAL specifically).
        assert_eq!(
            eval_direct(
                eval,
                vec![Scalar(txt("abc")), Range(vec![num(1.0), num(2.0)])]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    // ---- Nested-SUBTOTAL exclusion (RFC 0002 / OXP-123) --------------------

    use crate::args::{ArgShape, CallArgs, CellFlags, RefRect};
    use crate::context::EvalContext;
    use std::ops::ControlFlow;

    /// A minimal [`CallArgs`] that *does* implement the RFC 0002 provenance
    /// channel: argument 0 is the `function_num` selector, argument 1 is a range
    /// whose cells carry `is_subtotal` **and** `is_hidden_row` flags. Everything
    /// else refuses, so this isolates the [`NestedSubtotalFilter`] behavior from
    /// the engine.
    struct TaggedArgs {
        selector: Value,
        /// `(value, is_subtotal, is_hidden_row)` per cell of the range at index 1.
        cells: Vec<(Value, bool, bool)>,
    }

    impl CallArgs for TaggedArgs {
        fn count(&self) -> usize {
            2
        }
        fn shape(&mut self, index: usize) -> ArgShape {
            match index {
                0 => ArgShape::Scalar,
                // A one-cell arg models a single-cell REFERENCE (1×1): scalar-
                // shaped but ref-extent-Some (see `arg_ref_extent`), so RFC-0010's
                // `eff_shape` routes it to the aggregate path just like a range.
                1 if self.cells.len() == 1 => ArgShape::Scalar,
                1 => ArgShape::Range,
                _ => ArgShape::Omitted,
            }
        }
        fn arg_ref_extent(&mut self, index: usize) -> Option<RefRect> {
            // The value arg is always a reference in these tests; a one-cell arg
            // is a single-cell (1×1) reference — the RFC-0010 flip case.
            if index == 1 && self.cells.len() == 1 {
                Some(RefRect {
                    row: 0,
                    col: 0,
                    height: 1,
                    width: 1,
                })
            } else {
                None
            }
        }
        fn eval_scalar(&mut self, index: usize) -> Value {
            match index {
                0 => self.selector.clone(),
                _ => Value::Blank,
            }
        }
        fn for_each_cell(
            &mut self,
            index: usize,
            visit: &mut dyn FnMut(&Value) -> ControlFlow<()>,
        ) {
            // Plain (untagged) fallback path: stream every value, no exclusion.
            if index == 1 {
                for (v, _, _) in &self.cells {
                    if visit(v).is_break() {
                        return;
                    }
                }
            }
        }
        fn dims(&mut self, _index: usize) -> Option<(u32, u32)> {
            None
        }
        fn for_each_row(
            &mut self,
            _index: usize,
            _visit: &mut dyn FnMut(&[Value]) -> ControlFlow<()>,
        ) -> Result<(), ErrorKind> {
            Err(ErrorKind::Unsupported)
        }
        fn for_each_cell_tagged(
            &mut self,
            index: usize,
            visit: &mut dyn FnMut(&Value, CellFlags) -> ControlFlow<()>,
        ) -> Result<(), ()> {
            // Only the range at index 1 carries provenance; refuse others so the
            // filter's fallback path is exercised for them.
            if index == 1 {
                for (v, is_subtotal, is_hidden_row) in &self.cells {
                    let flags = CellFlags {
                        is_subtotal: *is_subtotal,
                        is_hidden_row: *is_hidden_row,
                    };
                    if visit(v, flags).is_break() {
                        return Ok(());
                    }
                }
                Ok(())
            } else {
                Err(())
            }
        }
    }

    #[test]
    fn nested_subtotal_cell_is_excluded_from_sum() {
        // SUBTOTAL(9, {10, [=SUBTOTAL(...) → 100], 20}). Excel ignores the nested
        // SUBTOTAL cell, so the grand total is 10 + 20 = 30, NOT 130.
        let mut args = TaggedArgs {
            selector: num(9.0),
            cells: vec![
                (num(10.0), false, false),
                (num(100.0), true, false),
                (num(20.0), false, false),
            ],
        };
        assert_eq!(eval(&EvalContext::new(), &mut args), num(30.0));
    }

    #[test]
    fn nested_subtotal_cell_is_excluded_from_count_and_average() {
        // COUNT (2) skips the tagged cell: two plain numbers remain → 2.
        let mut count_args = TaggedArgs {
            selector: num(2.0),
            cells: vec![
                (num(10.0), false, false),
                (num(100.0), true, false),
                (num(20.0), false, false),
            ],
        };
        assert_eq!(eval(&EvalContext::new(), &mut count_args), num(2.0));

        // AVERAGE (1) over the surviving {10, 20} = 15 (the 100 is excluded from
        // both the sum and the divisor).
        let mut avg_args = TaggedArgs {
            selector: num(1.0),
            cells: vec![
                (num(10.0), false, false),
                (num(100.0), true, false),
                (num(20.0), false, false),
            ],
        };
        assert_eq!(eval(&EvalContext::new(), &mut avg_args), num(15.0));
    }

    #[test]
    fn all_nested_subtotal_cells_excluded_leaves_max_empty() {
        // MAX (4) over a range whose only cells are nested SUBTOTALs: every cell
        // is skipped, so MAX sees no numbers and returns 0 (its empty-range
        // result, inherited from func_max).
        let mut args = TaggedArgs {
            selector: num(4.0),
            cells: vec![(num(50.0), true, false), (num(70.0), true, false)],
        };
        assert_eq!(eval(&EvalContext::new(), &mut args), num(0.0));
    }

    #[test]
    fn no_nested_subtotal_leaves_plain_aggregate_unchanged() {
        // With no tagged cells the tagged walk still serves the range, but nothing
        // is excluded: SUM = 10 + 100 + 20 = 130 (identical to the plain path).
        let mut args = TaggedArgs {
            selector: num(9.0),
            cells: vec![
                (num(10.0), false, false),
                (num(100.0), false, false),
                (num(20.0), false, false),
            ],
        };
        assert_eq!(eval(&EvalContext::new(), &mut args), num(130.0));
    }

    // ---- Hidden-row exclusion for 101-111 (OXP-121, RUN-2026-07-11-oracle01) --

    /// The exact OXP-121 layout: A1:A5 = 10,20,30,40,50 with **row 3 hidden**
    /// (the third cell flagged `is_hidden_row`), as a tagged range at index 1.
    fn oxp121_cells() -> Vec<(Value, bool, bool)> {
        vec![
            (num(10.0), false, false),
            (num(20.0), false, false),
            (num(30.0), false, true), // row 3, manually hidden
            (num(40.0), false, false),
            (num(50.0), false, false),
        ]
    }

    #[test]
    fn subtotal_109_excludes_hidden_row_120_and_9_includes_it_150() {
        // RUN-2026-07-11-oracle01 / OXP-121, the pinned values:
        //   =SUBTOTAL(109, A1:A5) = 120  (SUM excludes the hidden 30)
        //   =SUBTOTAL(9,   A1:A5) = 150  (SUM includes it)
        let mut a109 = TaggedArgs {
            selector: num(109.0),
            cells: oxp121_cells(),
        };
        assert_eq!(
            eval(&EvalContext::new(), &mut a109),
            num(120.0),
            "SUBTOTAL(109) excludes the hidden row → 120"
        );

        let mut a9 = TaggedArgs {
            selector: num(9.0),
            cells: oxp121_cells(),
        };
        assert_eq!(
            eval(&EvalContext::new(), &mut a9),
            num(150.0),
            "SUBTOTAL(9) includes the hidden row → 150 (unchanged)"
        );
    }

    #[test]
    fn subtotal_109_bare_hidden_single_cell_ref_is_dropped_rfc0010() {
        // RFC-0010: a bare single-cell REFERENCE in a hidden row, under
        // SUBTOTAL(109), rides OXP-121's range rule (reference ≡ 1×1 range) and
        // is DROPPED — so the sum over the (now empty) set is 0. A *computed*
        // scalar (`arg_ref_extent` None) would instead stay included; this pins
        // the ref half that the module's scalar-peek comment now documents.
        let mut hidden = TaggedArgs {
            selector: num(109.0),
            cells: vec![(num(42.0), false, true)], // one cell → single-cell ref, hidden
        };
        assert_eq!(
            eval(&EvalContext::new(), &mut hidden),
            num(0.0),
            "hidden single-cell ref dropped by SUBTOTAL(109) → SUM() = 0"
        );

        // Contrast: the same single-cell ref, NOT hidden, is included → 42.
        let mut visible = TaggedArgs {
            selector: num(109.0),
            cells: vec![(num(42.0), false, false)],
        };
        assert_eq!(
            eval(&EvalContext::new(), &mut visible),
            num(42.0),
            "visible single-cell ref included → 42"
        );
    }

    #[test]
    fn subtotal_102_counts_only_visible_rows() {
        // COUNT twin makes the difference unambiguous: 102 drops the hidden row
        // (4 visible numbers), 2 keeps it (5). (AVERAGE would coincide at 30 for
        // this symmetric data, so COUNT is the sharper witness.)
        let mut a102 = TaggedArgs {
            selector: num(102.0),
            cells: oxp121_cells(),
        };
        assert_eq!(eval(&EvalContext::new(), &mut a102), num(4.0));

        let mut a2 = TaggedArgs {
            selector: num(2.0),
            cells: oxp121_cells(),
        };
        assert_eq!(eval(&EvalContext::new(), &mut a2), num(5.0));
    }

    #[test]
    fn subtotal_109_with_no_hidden_rows_is_unchanged() {
        // Regression: with no cell flagged hidden, 109 and 9 agree (both 150) —
        // the hidden-row exclusion is a no-op when nothing is hidden.
        let plain = || {
            vec![
                (num(10.0), false, false),
                (num(20.0), false, false),
                (num(30.0), false, false),
                (num(40.0), false, false),
                (num(50.0), false, false),
            ]
        };
        let mut a109 = TaggedArgs {
            selector: num(109.0),
            cells: plain(),
        };
        assert_eq!(eval(&EvalContext::new(), &mut a109), num(150.0));
        let mut a9 = TaggedArgs {
            selector: num(9.0),
            cells: plain(),
        };
        assert_eq!(eval(&EvalContext::new(), &mut a9), num(150.0));
    }

    #[test]
    fn subtotal_109_drops_both_hidden_and_nested_subtotal_cells() {
        // The two exclusions compose for 101–111: a nested-SUBTOTAL cell (always
        // dropped) and a hidden-row cell (dropped for 109) both go, leaving
        // 10 + 40 = 50.
        let mut args = TaggedArgs {
            selector: num(109.0),
            cells: vec![
                (num(10.0), false, false),
                (num(100.0), true, false), // nested SUBTOTAL → dropped
                (num(30.0), false, true),  // hidden row → dropped for 109
                (num(40.0), false, false),
            ],
        };
        assert_eq!(eval(&EvalContext::new(), &mut args), num(50.0));

        // The 1–11 twin drops only the nested SUBTOTAL, keeping the hidden 30:
        // 10 + 30 + 40 = 80.
        let mut twin = TaggedArgs {
            selector: num(9.0),
            cells: vec![
                (num(10.0), false, false),
                (num(100.0), true, false),
                (num(30.0), false, true),
                (num(40.0), false, false),
            ],
        };
        assert_eq!(eval(&EvalContext::new(), &mut twin), num(80.0));
    }

    /// A minimal [`CallArgs`] where the aggregated argument (index 1) is a
    /// **scalar single-cell reference** carrying `is_subtotal` provenance — the
    /// shape `SUBTOTAL(9, A3)` produces. Argument 0 is the selector.
    struct ScalarTaggedArgs {
        selector: Value,
        cell: Value,
        is_subtotal: bool,
    }

    impl CallArgs for ScalarTaggedArgs {
        fn count(&self) -> usize {
            2
        }
        fn shape(&mut self, index: usize) -> ArgShape {
            match index {
                0 | 1 => ArgShape::Scalar,
                _ => ArgShape::Omitted,
            }
        }
        fn eval_scalar(&mut self, index: usize) -> Value {
            match index {
                0 => self.selector.clone(),
                1 => self.cell.clone(),
                _ => Value::Blank,
            }
        }
        fn for_each_cell(
            &mut self,
            index: usize,
            visit: &mut dyn FnMut(&Value) -> ControlFlow<()>,
        ) {
            if index == 1 {
                let _ = visit(&self.cell);
            }
        }
        fn dims(&mut self, _index: usize) -> Option<(u32, u32)> {
            None
        }
        fn for_each_row(
            &mut self,
            _index: usize,
            _visit: &mut dyn FnMut(&[Value]) -> ControlFlow<()>,
        ) -> Result<(), ErrorKind> {
            Err(ErrorKind::Unsupported)
        }
        fn for_each_cell_tagged(
            &mut self,
            index: usize,
            visit: &mut dyn FnMut(&Value, CellFlags) -> ControlFlow<()>,
        ) -> Result<(), ()> {
            if index == 1 {
                let flags = CellFlags {
                    is_subtotal: self.is_subtotal,
                    is_hidden_row: false,
                };
                let _ = visit(&self.cell, flags);
                Ok(())
            } else {
                Err(())
            }
        }
    }

    #[test]
    fn scalar_nested_subtotal_is_excluded() {
        // RUN-2026-07-11-oracle01 / OXP-123: =SUBTOTAL(9, A3) where A3 is itself a
        // SUBTOTAL (value 30) → the scalar nested SUBTOTAL is dropped, so SUM over
        // nothing = 0 (Excel observed 0, NOT the double-counted 30).
        let mut args = ScalarTaggedArgs {
            selector: num(9.0),
            cell: num(30.0),
            is_subtotal: true,
        };
        assert_eq!(eval(&EvalContext::new(), &mut args), num(0.0));
    }

    #[test]
    fn scalar_non_subtotal_single_cell_is_summed() {
        // Control: a scalar single-cell reference that is NOT a SUBTOTAL is summed
        // normally (its scalar coercion path is untouched by the exclusion).
        let mut args = ScalarTaggedArgs {
            selector: num(9.0),
            cell: num(30.0),
            is_subtotal: false,
        };
        assert_eq!(eval(&EvalContext::new(), &mut args), num(30.0));
    }

    #[test]
    fn subtotal_9_ignores_text_and_blank_in_range() {
        // Deferred SUM range rule: text/blank skipped, 5 + 7 = 12.
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(9.0)),
                    Range(vec![num(5.0), txt("nope"), Value::Blank, num(7.0)])
                ]
            ),
            num(12.0)
        );
    }

    // ---- Recalc sentinels propagate via delegation (no SUBTOTAL change) ----

    #[test]
    fn subtotal_2_count_propagates_sentinel_via_delegation() {
        // COUNT (function_num 2) was fixed directly in `func_count.rs` to
        // propagate a Recalc sentinel instead of silently skipping it.
        // SUBTOTAL needed no code change of its own — selector 2 just
        // delegates to `func_count::eval`, which now does the right thing.
        // Verified here as a delegation regression, not a SUBTOTAL-specific
        // fix.
        for k in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(
                eval_direct(
                    eval,
                    vec![
                        Scalar(num(2.0)),
                        Range(vec![num(10.0), Value::Error(k), num(20.0)])
                    ]
                ),
                Value::Error(k),
                "{k:?} should propagate through SUBTOTAL(2, ...) via COUNT delegation"
            );
        }
    }

    // ---- RFC 0010: single-cell reference routing through SUBTOTAL -----------

    // stat_reduce path (selector 10 = VAR): a single-cell *reference* to text is
    // ignored (the range rule), so VAR is over the surviving {2,4,6} — mean 4,
    // Σ(x−4)²=8, /(3−1)=4 — instead of coercing the text to `#VALUE!`. This
    // exercises both the `stat_reduce` flip and the wrappers' `arg_ref_extent`
    // forwarding that makes the reroute reach the delegate.
    #[test]
    fn rfc0010_stat_reduce_text_reference_is_ignored() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(10.0)),
                    Range(vec![num(2.0), num(4.0), num(6.0)]),
                    CellRef(txt("x")),
                ]
            ),
            num(4.0)
        );
    }

    // Control: a scalar *literal* text in the same stat_reduce path still
    // coerces under `CoercionMode::Scalar` and errors → `#VALUE!`.
    #[test]
    fn rfc0010_stat_reduce_scalar_literal_still_coerces() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(num(10.0)),
                    Range(vec![num(2.0), num(4.0), num(6.0)]),
                    Scalar(txt("x")),
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    // Delegate path (selector 9 = SUM): the wrappers now forward the ref channel,
    // so a lone single-cell text reference is ignored by SUM's range arm → 0
    // (not the `#VALUE!` a scalar-literal text would give).
    #[test]
    fn rfc0010_delegate_sum_text_reference_is_ignored() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(9.0)), CellRef(txt("x"))]),
            num(0.0)
        );
        // Contrast: a scalar literal text still coerces to #VALUE! via SUM.
        assert_eq!(
            eval_direct(eval, vec![Scalar(num(9.0)), Scalar(txt("x"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    #[test]
    fn subtotal_3_and_103_counta_propagate_sentinel_via_delegation() {
        // COUNTA (function_num 3, and its hidden-row-excluding twin 103) was
        // fixed directly in `func_counta.rs`; SUBTOTAL inherits it unchanged.
        for selector in [3.0, 103.0] {
            assert_eq!(
                eval_direct(
                    eval,
                    vec![
                        Scalar(num(selector)),
                        Range(vec![
                            num(10.0),
                            Value::Error(ErrorKind::Unsupported),
                            num(20.0)
                        ])
                    ]
                ),
                Value::Error(ErrorKind::Unsupported),
                "selector {selector} should propagate through COUNTA delegation"
            );
        }
    }
}
