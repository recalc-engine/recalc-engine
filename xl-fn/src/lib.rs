//! `xl-fn` — the Recalc function library and scalar operators.
//!
//! Houses Excel's built-in functions (one module per function, registered in a
//! single static [`REGISTRY`] table) and the scalar formula
//! operators ([`apply_binary`]/[`apply_unary`]/[`apply_postfix`]). The v0
//! "minimal evaluator" scope started as **`SUM`, `IF`, and the operators**,
//! enough to make the Task-10 diff harness meaningful, and now also covers the
//! Tier-0 numeric-aggregation family (`AVERAGE`, `COUNT`, `COUNTA`, `MIN`,
//! `MAX`) and the date-component family (`YEAR`, `MONTH`, `DAY`, `DATE`,
//! `EOMONTH`, built on the `datecore` serial↔calendar core — including
//! Excel's 1900 leap-year bug). Every other function is simply absent from the registry and
//! therefore evaluates to `#UNSUPPORTED!` in the engine — never a guessed
//! result (the "never silently wrong" principle, the Recalc design rules /
//! `implementation-plan.md` §0).
//!
//! # Architecture
//! - `registry` — [`FnSpec`] metadata (name, arity, [`Volatility`], evaluator)
//!   plus [`lookup`]/[`is_volatile`]. Arity is the one declaration the engine
//!   consults generically ([`FnSpec::accepts_arity`]); coercion is owned by each
//!   function's `eval`, not re-declared (see [`FnSpec`]).
//! - `ops` — the scalar operators, all coercion routed through `xl-value`.
//! - `context` — [`EvalContext`], the clock/RNG sandbox seam (v0: everything
//!   `#UNSUPPORTED!`).
//! - `args` — [`CallArgs`], the lazy argument-access interface the engine
//!   implements so `IF` can skip its unselected branch and `SUM` can stream a
//!   range without materializing it.
//! - `func_sum` / `func_if` — the original two implemented functions.
//! - `func_average` / `func_count` / `func_counta` / `func_min` / `func_max` —
//!   the aggregation family, all following `func_sum`'s
//!   scalar-vs-range-aggregate coercion pattern.
//! - `datecore` — the serial↔calendar core (integer date math, the 1900
//!   leap-year bug, the 1900/1904 systems); `date_common` — shared arg coercion
//!   for the date functions; `func_year` / `func_month` / `func_day` /
//!   `func_date` / `func_eomonth` — the date-component family built on them.
//!
//! # Provenance
//! Function behavior is reconstructed clean-room from the per-function specs in
//! `docs/specs/` (each of which cites its Microsoft Learn page); all coercion is
//! deferred to `xl-value` (the frozen coercion contract), never re-derived
//! here. Semantics that a public source cannot pin are returned as
//! `#UNSUPPORTED!` and queued in `docs/oracle-experiments.md` under each
//! function's own registered numeric OXP id.
//!
//! # Tests are a specification snapshot
//! The unit tests below encode the documented tables from `docs/specs/SUM.md`
//! and `docs/specs/IF.md`. Like `xl-value`'s, they are expected to be
//! **superseded** by the Excel oracle grids once those exist (`implementation-plan.md`
//! §3); until then they pin the documented behavior and the oracle-deferred
//! `#UNSUPPORTED!` boundaries.

#![forbid(unsafe_code)]

mod args;
mod arrayshape;
mod context;
mod criteria;
mod date_common;
mod datecore;
mod func_abs;
mod func_and;
mod func_average;
mod func_averageif;
mod func_averageifs;
mod func_ceiling;
mod func_cell;
mod func_choose;
mod func_columns;
mod func_concat;
mod func_concatenate;
mod func_correl;
mod func_count;
mod func_counta;
mod func_countif;
mod func_countifs;
mod func_date;
mod func_datevalue;
mod func_day;
mod func_days360;
mod func_dsum;
mod func_edate;
mod func_eomonth;
mod func_erfc;
mod func_exp;
mod func_find;
mod func_hlookup;
mod func_hour;
mod func_hyperlink;
mod func_if;
mod func_iferror;
mod func_ifna;
mod func_ifs;
mod func_index;
mod func_int;
mod func_ipmt;
mod func_isblank;
mod func_iserr;
mod func_iserror;
mod func_isna;
mod func_isnumber;
mod func_istext;
mod func_left;
mod func_len;
mod func_ln;
mod func_lookup;
mod func_lower;
mod func_match;
mod func_max;
mod func_maxa;
mod func_maxifs;
mod func_mid;
mod func_min;
mod func_mina;
mod func_minifs;
mod func_mod;
mod func_month;
mod func_na;
mod func_not;
mod func_npv;
mod func_or;
mod func_pmt;
mod func_power;
mod func_ppmt;
mod func_product;
mod func_quotient;
mod func_right;
mod func_round;
mod func_rows;
mod func_sin;
mod func_sln;
mod func_sqrt;
mod func_stdev;
mod func_subtotal;
mod func_sum;
mod func_sumif;
mod func_sumifs;
mod func_sumproduct;
mod func_sumsq;
mod func_switch;
mod func_text;
mod func_textjoin;
mod func_trim;
mod func_upper;
mod func_value;
mod func_vlookup;
mod func_weekday;
mod func_workday;
mod func_xor;
mod func_year; // OXP-160 date-text
// Oracle-pinned numerics wave (OXP-153-158):
mod func_irr;
mod func_normdist;
mod func_norminv;
mod func_normsinv;
mod func_vdb;
mod func_xirr;
mod func_xnpv;
// Frequency-targeted implementation wave (stubs; specs written by agents):
mod func_address;
mod func_averagea;
mod func_column;
mod func_daverage;
mod func_exact;
mod func_false;
mod func_fv;
mod func_large;
mod func_median;
mod func_networkdays;
mod func_pi;
mod func_pv;
mod func_rank;
mod func_rept;
mod func_rounddown;
mod func_roundup;
mod func_row;
mod func_small;
mod func_substitute;
mod func_true;
mod func_yearfrac;
// M2 lane 3a — dynamic-array reshape / generate tranche (shared helpers in
// `arrayshape`); see docs/plans/2026-07-15-lane3a-probe-needed.md:
mod func_choosecols;
mod func_chooserows;
mod func_drop;
mod func_expand;
mod func_hstack;
mod func_randarray;
mod func_sequence;
mod func_take;
mod func_tocol;
mod func_torow;
mod func_vstack;
mod func_wrapcols;
mod func_wraprows;
// M2 Lane 3b — dynamic-array lookup/sort/filter tranche. Shared grid helpers in
// `dynarray`; refused edges queued in docs/plans/2026-07-15-lane3b-probe-needed.md.
mod dynarray;
mod func_filter;
mod func_sort;
mod func_sortby;
mod func_unique;
mod func_xlookup;
mod func_xmatch;
// Coverage wave 2 (v3 capstone `01bf4e6` unimplemented-function ranking): the
// top declined-cell math/text tail. Trig core (COS/ACOS/ASIN/ATAN) + TRUNC +
// LOG are unambiguous; FLOOR / SEARCH / PROPER serve their documented core and
// defer their quirky edges to oracle probes (OXP-223/224/225).
mod func_acos;
mod func_asin;
mod func_atan;
mod func_cos;
mod func_floor;
mod func_log;
mod func_proper;
mod func_search;
mod func_trunc;
mod lookup;
mod ops;
mod registry;
#[cfg(test)]
mod test_support;

pub use args::{ArgShape, CallArgs, CellFlags, RefRect};
pub use context::EvalContext;
pub use datecore::DateSystem;
pub use ops::{
    apply_binary, apply_postfix, apply_unary, broadcast_binary, broadcast_postfix, broadcast_unary,
};
pub use registry::{
    ARRAY_RETURNING_FNS, EvalFn, FnSpec, REGISTRY, Volatility, is_volatile, lookup, returns_array,
};

#[cfg(test)]
mod tests {

    use std::ops::ControlFlow;

    use super::*;
    use xl_value::{ErrorKind, Value};

    // ---- RFC-0014 R4: ARRAY_RETURNING_FNS is the explicit single source ----

    #[test]
    fn array_returning_fns_are_all_registered() {
        // Typo guard: every listed name resolves to a real function.
        for name in ARRAY_RETURNING_FNS {
            assert!(
                lookup(name).is_some(),
                "ARRAY_RETURNING_FNS lists `{name}`, which is not a registered function"
            );
        }
        // `returns_array` agrees with the list and is case-insensitive.
        assert!(returns_array("filter") && returns_array("SEQUENCE"));
        assert!(!returns_array("SUM") && !returns_array("IF"));
    }

    #[test]
    fn no_registered_fn_silently_returns_arrays_uncovered() {
        // Structural cross-check (RFC-0014 R4a): scan each `func_*.rs` module for
        // the array-construction paths — the `spill(` helper (used ONLY by
        // genuine top-level array returners, never by aggregators that merely
        // *read* arrays) — and assert its function is covered. This fails loudly
        // if a new array-returning function is added without listing it, closing
        // the "silent under-cover" gap the always-on runtime backstop would
        // otherwise absorb as lost parallelism.
        use std::collections::BTreeSet;
        use std::fs;

        // Modules that construct top-level arrays WITHOUT the `spill(` helper
        // (INDEX builds via `Array::new` directly) — enumerated so the scan can
        // require them too.
        let non_spill_helper_returners: &[(&str, &str)] = &[("func_index", "INDEX")];

        let covered: BTreeSet<String> = ARRAY_RETURNING_FNS
            .iter()
            .map(|s| s.to_ascii_uppercase())
            .collect();

        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        for entry in fs::read_dir(dir).expect("read src/") {
            let path = entry.expect("dir entry").path();
            let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !fname.starts_with("func_") || !fname.ends_with(".rs") {
                continue;
            }
            let module = fname.trim_end_matches(".rs");
            let src = fs::read_to_string(&path).expect("read module");
            // Discriminator: a top-level array returner calls the `spill(` helper
            // (imported from `arrayshape`/`dynarray`). Aggregators that read
            // arrays never call it. `use crate::...::spill` import lines are not
            // a call site, so require an actual `spill(` invocation.
            let calls_spill = src.contains("spill(")
                && (src.contains("arrayshape::") || src.contains("dynarray::"));
            let explicit = non_spill_helper_returners
                .iter()
                .find(|(m, _)| *m == module);
            if let Some((_, fname_upper)) = explicit {
                assert!(
                    covered.contains(*fname_upper),
                    "{module} constructs a top-level array but `{fname_upper}` is \
                     missing from ARRAY_RETURNING_FNS (RFC-0014 R4)"
                );
            }
            if calls_spill {
                // Map module -> canonical function name via its registered spec.
                // The module `func_xlookup` registers `XLOOKUP`, etc.; find any
                // registered name whose lowercased form matches the module tail.
                let tail = module.trim_start_matches("func_");
                let listed = ARRAY_RETURNING_FNS
                    .iter()
                    .any(|n| n.eq_ignore_ascii_case(tail));
                assert!(
                    listed,
                    "{module} calls the array `spill(` helper but its function is \
                     missing from ARRAY_RETURNING_FNS (RFC-0014 R4a): a new \
                     array-returner must be listed there"
                );
            }
        }
    }

    // ---- Test harness: a mock `CallArgs` ---------------------------------
    //
    // Lets a unit test hand fixed argument shapes/values to a function's
    // evaluator without standing up the whole engine. `Poison` panics if it is
    // ever evaluated — the mechanism for asserting `IF` laziness.
    //
    // `Range`/`Array` carry a flat value list: `Range` models an N×1 column and
    // `Array` a 1×N row (their dims). `Rect` carries an explicit `rows`×`cols`
    // rectangle in row-major order (blanks included at their positions) for the
    // positional `dims`/`for_each_row` path. `Unbounded` models a whole-column
    // range that the dense row walk must refuse.
    enum TestArg {
        Scalar(Value),
        Range(Vec<Value>),
        Array(Vec<Value>),
        Rect {
            rows: u32,
            cols: u32,
            data: Vec<Value>,
        },
        /// A whole-column range with **contiguous** single-column data at
        /// relative rows `0..n`: `for_each_cell` streams its cells and the
        /// used-extent `for_each_used_row` yields `(i, [vs[i]])`, but
        /// `dims`/`for_each_row` refuse it (the documented dense-walk policy).
        Unbounded(Vec<Value>),
        /// A whole-column range with **explicit** used-extent rows
        /// `(relative_row, cells)` — models gaps between populated rows and
        /// multi-column column spans. `dims`/`for_each_row` refuse it (unbounded);
        /// `for_each_used_row` yields the rows verbatim (they must be ascending).
        UsedRows(Vec<(u32, Vec<Value>)>),
        Omitted,
        /// Panics if forced; proves a branch/argument is *not* evaluated.
        Poison,
    }

    struct TestArgs {
        args: Vec<TestArg>,
    }

    impl CallArgs for TestArgs {
        fn count(&self) -> usize {
            self.args.len()
        }
        fn shape(&mut self, index: usize) -> ArgShape {
            match self.args.get(index) {
                Some(TestArg::Scalar(_)) | Some(TestArg::Poison) => ArgShape::Scalar,
                Some(TestArg::Range(_))
                | Some(TestArg::Unbounded(_))
                | Some(TestArg::UsedRows(_))
                | Some(TestArg::Rect { .. }) => ArgShape::Range,
                Some(TestArg::Array(_)) => ArgShape::Array,
                Some(TestArg::Omitted) | None => ArgShape::Omitted,
            }
        }
        fn eval_scalar(&mut self, index: usize) -> Value {
            match self.args.get(index) {
                Some(TestArg::Scalar(v)) => v.clone(),
                Some(TestArg::Omitted) | None => Value::Blank,
                // A multi-cell range/array in scalar context is #UNSUPPORTED!,
                // matching xl-value's rule.
                Some(TestArg::Range(_))
                | Some(TestArg::Array(_))
                | Some(TestArg::Rect { .. })
                | Some(TestArg::Unbounded(_))
                | Some(TestArg::UsedRows(_)) => Value::Error(ErrorKind::Unsupported),
                Some(TestArg::Poison) => panic!("poison argument {index} was evaluated"),
            }
        }
        fn for_each_cell(
            &mut self,
            index: usize,
            visit: &mut dyn FnMut(&Value) -> ControlFlow<()>,
        ) {
            match self.args.get(index) {
                Some(TestArg::Range(vs))
                | Some(TestArg::Array(vs))
                | Some(TestArg::Unbounded(vs)) => {
                    for v in vs {
                        if visit(v).is_break() {
                            return;
                        }
                    }
                }
                Some(TestArg::Rect { data, .. }) => {
                    for v in data {
                        if visit(v).is_break() {
                            return;
                        }
                    }
                }
                Some(TestArg::UsedRows(rows)) => {
                    for (_, cells) in rows {
                        for v in cells {
                            if visit(v).is_break() {
                                return;
                            }
                        }
                    }
                }
                Some(TestArg::Scalar(v)) => {
                    let _ = visit(v);
                }
                Some(TestArg::Poison) => panic!("poison argument {index} was streamed"),
                Some(TestArg::Omitted) | None => {}
            }
        }
        fn dims(&mut self, index: usize) -> Option<(u32, u32)> {
            match self.args.get(index) {
                Some(TestArg::Range(vs)) => Some((vs.len() as u32, 1)),
                Some(TestArg::Array(vs)) => Some((1, vs.len() as u32)),
                Some(TestArg::Rect { rows, cols, .. }) => Some((*rows, *cols)),
                // Unbounded / scalar / lazy / omitted have no materializable
                // bounded rectangle.
                Some(TestArg::Unbounded(_))
                | Some(TestArg::UsedRows(_))
                | Some(TestArg::Scalar(_))
                | Some(TestArg::Poison)
                | Some(TestArg::Omitted)
                | None => None,
            }
        }
        fn for_each_row(
            &mut self,
            index: usize,
            visit: &mut dyn FnMut(&[Value]) -> ControlFlow<()>,
        ) -> Result<(), ErrorKind> {
            match self.args.get(index) {
                // A column: one value per row.
                Some(TestArg::Range(vs)) => {
                    for v in vs {
                        if visit(std::slice::from_ref(v)).is_break() {
                            break;
                        }
                    }
                    Ok(())
                }
                // A single row.
                Some(TestArg::Array(vs)) => {
                    let _ = visit(vs);
                    Ok(())
                }
                Some(TestArg::Rect { cols, data, .. }) => {
                    for row in data.chunks(*cols as usize) {
                        if visit(row).is_break() {
                            break;
                        }
                    }
                    Ok(())
                }
                // A scalar is a 1×1 rectangle.
                Some(TestArg::Scalar(v)) => {
                    let _ = visit(std::slice::from_ref(v));
                    Ok(())
                }
                // Documented policy: the dense walk refuses an unbounded range.
                Some(TestArg::Unbounded(_)) | Some(TestArg::UsedRows(_)) => {
                    Err(ErrorKind::Unsupported)
                }
                Some(TestArg::Poison) => panic!("poison argument {index} was streamed"),
                Some(TestArg::Omitted) | None => Ok(()),
            }
        }
        fn for_each_used_row(
            &mut self,
            index: usize,
            visit: &mut dyn FnMut(u32, &[Value]) -> ControlFlow<()>,
        ) -> Result<(), ErrorKind> {
            match self.args.get(index) {
                // Whole-column single-column data at contiguous relative rows.
                Some(TestArg::Unbounded(vs)) => {
                    for (i, v) in vs.iter().enumerate() {
                        if visit(i as u32, std::slice::from_ref(v)).is_break() {
                            break;
                        }
                    }
                    Ok(())
                }
                // Explicit used-extent rows (relative_row, cells), verbatim.
                Some(TestArg::UsedRows(rows)) => {
                    for (rel, cells) in rows {
                        if visit(*rel, cells).is_break() {
                            break;
                        }
                    }
                    Ok(())
                }
                // Bounded shapes carry their dense relative index, mirroring
                // `EngineArgs` so a test can exercise the method directly.
                Some(TestArg::Range(vs)) => {
                    for (i, v) in vs.iter().enumerate() {
                        if visit(i as u32, std::slice::from_ref(v)).is_break() {
                            break;
                        }
                    }
                    Ok(())
                }
                Some(TestArg::Array(vs)) => {
                    let _ = visit(0, vs);
                    Ok(())
                }
                Some(TestArg::Rect { cols, data, .. }) => {
                    for (i, row) in data.chunks(*cols as usize).enumerate() {
                        if visit(i as u32, row).is_break() {
                            break;
                        }
                    }
                    Ok(())
                }
                Some(TestArg::Scalar(v)) => {
                    let _ = visit(0, std::slice::from_ref(v));
                    Ok(())
                }
                Some(TestArg::Poison) => panic!("poison argument {index} was streamed"),
                Some(TestArg::Omitted) | None => Ok(()),
            }
        }
    }

    fn call(name: &str, args: Vec<TestArg>) -> Value {
        let spec = lookup(name).expect("function registered");
        assert!(spec.accepts_arity(args.len()), "arity");
        let ctx = EvalContext::new();
        let mut ta = TestArgs { args };
        (spec.eval)(&ctx, &mut ta)
    }

    fn num(x: f64) -> Value {
        Value::number(x)
    }

    fn txt(s: &str) -> Value {
        Value::text(s)
    }

    /// Like [`call`] but with an explicit date system, for the 1904 cases.
    fn call_ds(name: &str, args: Vec<TestArg>, ds: DateSystem) -> Value {
        let spec = lookup(name).expect("function registered");
        assert!(spec.accepts_arity(args.len()), "arity");
        let ctx = EvalContext::with_date_system(ds);
        let mut ta = TestArgs { args };
        (spec.eval)(&ctx, &mut ta)
    }

    // SUM.md §Coercion / hit-list: direct scalar args coerce (TRUE→1, "2"→2).
    #[test]
    fn sum_scalar_coercion_asymmetry() {
        assert_eq!(
            call(
                "SUM",
                vec![
                    TestArg::Scalar(Value::Bool(true)),
                    TestArg::Scalar(txt("2"))
                ]
            ),
            num(3.0)
        );
    }

    // SUM.md §1/§Coercion: text/logicals/blanks inside a range are skipped.
    #[test]
    fn sum_range_skips_text_logicals_blanks() {
        let range = vec![
            num(1.0),
            Value::Bool(true),
            txt("2"),
            Value::Blank,
            num(4.0),
        ];
        assert_eq!(call("SUM", vec![TestArg::Range(range)]), num(5.0));
    }

    // SUM.md §Coercion: array constants follow RangeAggregate per element.
    #[test]
    fn sum_array_constant_per_element() {
        let arr = vec![num(1.0), Value::Bool(true), txt("2"), num(3.0)];
        assert_eq!(call("SUM", vec![TestArg::Array(arr)]), num(4.0));
    }

    // SUM.md §3/§4: omitted args contribute 0; no numbers → 0 (not an error).
    #[test]
    fn sum_omitted_and_empty() {
        assert_eq!(
            call("SUM", vec![TestArg::Scalar(num(5.0)), TestArg::Omitted]),
            num(5.0)
        );
        assert_eq!(
            call("SUM", vec![TestArg::Range(vec![Value::Blank])]),
            num(0.0)
        );
    }

    // SUM.md §Error behavior: a scalar non-numeric text is #VALUE!.
    #[test]
    fn sum_scalar_text_is_value_error() {
        assert_eq!(
            call("SUM", vec![TestArg::Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // SUM.md §Error behavior: an error inside a range propagates (the
    // "SUM range with #DIV/0! inside" case).
    #[test]
    fn sum_propagates_error_from_range() {
        let range = vec![num(1.0), Value::Error(ErrorKind::Div0), num(2.0)];
        assert_eq!(
            call("SUM", vec![TestArg::Range(range)]),
            Value::Error(ErrorKind::Div0)
        );
    }

    // SUM.md §Error behavior: overflow of the running total → #NUM!.
    #[test]
    fn sum_overflow_is_num() {
        assert_eq!(
            call(
                "SUM",
                vec![
                    TestArg::Scalar(num(f64::MAX)),
                    TestArg::Scalar(num(f64::MAX))
                ]
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    // AVERAGE.md §Coercion: direct scalar args coerce (TRUE→1, "2"→2), same
    // asymmetry family as SUM. mean([1,2,6]) = 3.
    #[test]
    fn average_scalar_coercion_asymmetry() {
        assert_eq!(
            call(
                "AVERAGE",
                vec![
                    TestArg::Scalar(Value::Bool(true)),
                    TestArg::Scalar(txt("2")),
                    TestArg::Scalar(num(6.0)),
                ]
            ),
            num(3.0)
        );
    }

    // AVERAGE.md §2/§3: text/logicals/blanks inside a range are excluded
    // from *both* the sum and the denominator (not counted as zero).
    #[test]
    fn average_range_excludes_text_logicals_blanks_from_denominator() {
        let range = vec![
            num(2.0),
            Value::Bool(true),
            txt("100"),
            Value::Blank,
            num(4.0),
        ];
        // Only 2.0 and 4.0 participate: mean = 3.0, denominator 2 (not 5).
        assert_eq!(call("AVERAGE", vec![TestArg::Range(range)]), num(3.0));
    }

    // AVERAGE.md §Coercion: array constants follow RangeAggregate per
    // element, mirroring SUM's array-constant rule.
    #[test]
    fn average_array_constant_per_element() {
        let arr = vec![num(2.0), Value::Bool(true), txt("2"), num(6.0)];
        assert_eq!(call("AVERAGE", vec![TestArg::Array(arr)]), num(4.0));
    }

    // AVERAGE.md §4/§Error behavior: no numeric values anywhere → #DIV/0!.
    #[test]
    fn average_empty_is_div0() {
        assert_eq!(
            call("AVERAGE", vec![TestArg::Range(vec![Value::Blank])]),
            Value::Error(ErrorKind::Div0)
        );
    }

    // AVERAGE.md §Error behavior: a scalar non-numeric text is #VALUE!.
    #[test]
    fn average_scalar_text_is_value_error() {
        assert_eq!(
            call("AVERAGE", vec![TestArg::Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // AVERAGE.md §Error behavior: an error inside a range propagates.
    #[test]
    fn average_propagates_error_from_range() {
        let range = vec![num(1.0), Value::Error(ErrorKind::Div0), num(2.0)];
        assert_eq!(
            call("AVERAGE", vec![TestArg::Range(range)]),
            Value::Error(ErrorKind::Div0)
        );
    }

    // OXP-083 (RESOLVED RUN-2026-07-11-oracle01): a scalar Blank argument
    // (bare empty-cell reference or an elided slot) is EXCLUDED from both the
    // sum and the denominator — not counted as 0. Observed `=AVERAGE(A1,5)` = 5.
    #[test]
    fn average_scalar_blank_excluded() {
        assert_eq!(
            call(
                "AVERAGE",
                vec![TestArg::Scalar(Value::Blank), TestArg::Scalar(num(5.0))]
            ),
            num(5.0)
        );
        // An elided argument slot evaluates to Blank too (CallArgs contract).
        assert_eq!(
            call("AVERAGE", vec![TestArg::Scalar(num(5.0)), TestArg::Omitted]),
            num(5.0)
        );
    }

    // COUNT.md §1/§2: only real numbers inside a range count; text, logical,
    // and blank cells are skipped without erroring.
    #[test]
    fn count_range_counts_only_numbers() {
        let range = vec![
            num(1.0),
            Value::Bool(true),
            txt("2"),
            Value::Blank,
            num(4.0),
        ];
        assert_eq!(call("COUNT", vec![TestArg::Range(range)]), num(2.0));
    }

    // COUNT.md §Error behavior: an error inside a range is NOT counted and
    // does NOT propagate (COUNT is documented error-tolerant on ranges).
    #[test]
    fn count_range_error_not_counted_not_propagated() {
        let range = vec![num(1.0), Value::Error(ErrorKind::Div0), num(2.0)];
        assert_eq!(call("COUNT", vec![TestArg::Range(range)]), num(2.0));
    }

    // COUNT.md §Semantics bullet 3: direct numbers and numeric text count.
    #[test]
    fn count_scalar_number_and_numeric_text_count() {
        assert_eq!(
            call(
                "COUNT",
                vec![TestArg::Scalar(num(1.0)), TestArg::Scalar(txt("2"))]
            ),
            num(2.0)
        );
    }

    // Complement of the documented countable-forms list: non-numeric text
    // and blank scalars do not count, and do not error.
    #[test]
    fn count_scalar_non_numeric_text_and_blank_do_not_count() {
        assert_eq!(
            call(
                "COUNT",
                vec![
                    TestArg::Scalar(num(1.0)),
                    TestArg::Scalar(txt("abc")),
                    TestArg::Scalar(Value::Blank),
                    TestArg::Omitted,
                ]
            ),
            num(1.0)
        );
    }

    // OXP-084 (RESOLVED RUN-2026-07-11-oracle01): a direct logical argument
    // IS counted. Observed `=COUNT(TRUE)` = 1, `=COUNT(TRUE, FALSE, 1)` = 3.
    #[test]
    fn count_scalar_bool_counted() {
        assert_eq!(
            call("COUNT", vec![TestArg::Scalar(Value::Bool(true))]),
            num(1.0)
        );
        assert_eq!(
            call(
                "COUNT",
                vec![
                    TestArg::Scalar(Value::Bool(true)),
                    TestArg::Scalar(Value::Bool(false)),
                    TestArg::Scalar(num(1.0)),
                ]
            ),
            num(3.0)
        );
    }

    // OXP-085 (RESOLVED RUN-2026-07-11-oracle01): a direct error argument is
    // silently skipped — neither counted nor propagated. Observed
    // `=COUNT(1, #DIV/0!)` = 1.
    #[test]
    fn count_scalar_error_skipped() {
        assert_eq!(
            call(
                "COUNT",
                vec![
                    TestArg::Scalar(num(1.0)),
                    TestArg::Scalar(Value::Error(ErrorKind::Na)),
                ]
            ),
            num(1.0)
        );
    }

    // COUNT of nothing numeric anywhere → 0, not an error.
    #[test]
    fn count_empty_is_zero() {
        assert_eq!(
            call("COUNT", vec![TestArg::Range(vec![Value::Blank, txt("x")])]),
            num(0.0)
        );
    }

    // COUNTA.md §1: numbers, text, logicals, and errors all count; only
    // Blank does not. Scalar and range/array behave identically (no
    // coercion at all).
    #[test]
    fn counta_counts_every_non_blank_type() {
        let range = vec![
            num(1.0),
            txt("x"),
            Value::Bool(false),
            Value::Error(ErrorKind::Na),
            Value::Blank,
        ];
        assert_eq!(call("COUNTA", vec![TestArg::Range(range)]), num(4.0));
    }

    // COUNTA.md §2: a computed `""` counts (it is Text, not Blank), but a
    // truly empty cell does not — the classic "" vs Blank distinction.
    #[test]
    fn counta_counts_empty_string_but_not_blank() {
        assert_eq!(
            call(
                "COUNTA",
                vec![TestArg::Scalar(txt("")), TestArg::Scalar(Value::Blank)]
            ),
            num(1.0)
        );
    }

    // An omitted argument slot evaluates to Blank and is not counted.
    #[test]
    fn counta_omitted_not_counted() {
        assert_eq!(
            call("COUNTA", vec![TestArg::Scalar(num(1.0)), TestArg::Omitted]),
            num(1.0)
        );
    }

    // COUNTA.md §Error behavior: an error inside a range counts and never
    // propagates (error-tolerant by design).
    #[test]
    fn counta_range_error_counts_no_propagation() {
        let range = vec![num(1.0), Value::Error(ErrorKind::Div0)];
        assert_eq!(call("COUNTA", vec![TestArg::Range(range)]), num(2.0));
    }

    // COUNTA of nothing (all blank) → 0.
    #[test]
    fn counta_empty_is_zero() {
        assert_eq!(
            call("COUNTA", vec![TestArg::Range(vec![Value::Blank])]),
            num(0.0)
        );
    }

    // MIN.md §Coercion: direct scalar args coerce (TRUE→1, "2"→2), same
    // family as SUM's direct-arg asymmetry.
    #[test]
    fn min_scalar_coercion_asymmetry() {
        assert_eq!(
            call(
                "MIN",
                vec![
                    TestArg::Scalar(Value::Bool(true)),
                    TestArg::Scalar(txt("2")),
                    TestArg::Scalar(num(5.0)),
                ]
            ),
            num(1.0)
        );
    }

    // MIN.md §1/§2: text/logicals/blanks inside a range are ignored.
    #[test]
    fn min_range_skips_text_logicals_blanks() {
        let range = vec![
            num(5.0),
            Value::Bool(false),
            txt("-100"),
            Value::Blank,
            num(2.0),
        ];
        assert_eq!(call("MIN", vec![TestArg::Range(range)]), num(2.0));
    }

    // MIN.md §3/§Error behavior: no numeric values anywhere → 0, not an
    // error (the documented contrast with AVERAGE's #DIV/0!).
    #[test]
    fn min_empty_is_zero() {
        assert_eq!(
            call("MIN", vec![TestArg::Range(vec![Value::Blank])]),
            num(0.0)
        );
    }

    // MIN.md §Error behavior: a scalar non-numeric text is #VALUE!.
    #[test]
    fn min_scalar_text_is_value_error() {
        assert_eq!(
            call("MIN", vec![TestArg::Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // MIN.md §Error behavior: an error inside a range propagates.
    #[test]
    fn min_propagates_error_from_range() {
        let range = vec![num(5.0), Value::Error(ErrorKind::Na), num(2.0)];
        assert_eq!(
            call("MIN", vec![TestArg::Range(range)]),
            Value::Error(ErrorKind::Na)
        );
    }

    // OXP-086: a scalar Blank argument is oracle-deferred (counts as 0 vs
    // excluded).
    #[test]
    fn min_scalar_blank_is_oracle_deferred() {
        assert_eq!(
            call(
                "MIN",
                vec![TestArg::Scalar(Value::Blank), TestArg::Scalar(num(5.0))]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // MAX.md: direct scalar args coerce, same family as MIN/SUM.
    #[test]
    fn max_scalar_coercion_asymmetry() {
        assert_eq!(
            call(
                "MAX",
                vec![
                    TestArg::Scalar(Value::Bool(true)),
                    TestArg::Scalar(txt("2")),
                    TestArg::Scalar(num(-5.0)),
                ]
            ),
            num(2.0)
        );
    }

    // MAX.md: text/logicals/blanks inside a range are ignored.
    #[test]
    fn max_range_skips_text_logicals_blanks() {
        let range = vec![
            num(5.0),
            Value::Bool(true),
            txt("100"),
            Value::Blank,
            num(8.0),
        ];
        assert_eq!(call("MAX", vec![TestArg::Range(range)]), num(8.0));
    }

    // MAX.md §3: no numeric values anywhere → 0, not an error.
    #[test]
    fn max_empty_is_zero() {
        assert_eq!(
            call("MAX", vec![TestArg::Range(vec![Value::Blank])]),
            num(0.0)
        );
    }

    // MAX.md: a scalar non-numeric text is #VALUE!.
    #[test]
    fn max_scalar_text_is_value_error() {
        assert_eq!(
            call("MAX", vec![TestArg::Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // MAX.md: an error inside a range propagates.
    #[test]
    fn max_propagates_error_from_range() {
        let range = vec![num(5.0), Value::Error(ErrorKind::Na), num(2.0)];
        assert_eq!(
            call("MAX", vec![TestArg::Range(range)]),
            Value::Error(ErrorKind::Na)
        );
    }

    // OXP-087: a scalar Blank argument is oracle-deferred, mirroring MIN's
    // OXP-086.
    #[test]
    fn max_scalar_blank_is_oracle_deferred() {
        assert_eq!(
            call(
                "MAX",
                vec![TestArg::Scalar(Value::Blank), TestArg::Scalar(num(5.0))]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // Cross-function registry sanity: all five aggregation functions are
    // registered with the documented 1..=255 arity.
    #[test]
    fn aggregation_family_registered_with_expected_arity() {
        for name in ["AVERAGE", "COUNT", "COUNTA", "MIN", "MAX"] {
            let spec = lookup(name).unwrap_or_else(|| panic!("{name} registered"));
            assert!(!spec.accepts_arity(0), "{name} requires at least 1 arg");
            assert!(spec.accepts_arity(1));
            assert!(spec.accepts_arity(255));
            assert!(!spec.accepts_arity(256));
            assert!(!is_volatile(name), "{name} is non-volatile");
        }
    }

    // IF.md §1: selects the branch matching the test.
    #[test]
    fn if_selects_branch() {
        assert_eq!(
            call(
                "IF",
                vec![
                    TestArg::Scalar(Value::Bool(true)),
                    TestArg::Scalar(num(10.0)),
                    TestArg::Scalar(num(20.0)),
                ]
            ),
            num(10.0)
        );
        assert_eq!(
            call(
                "IF",
                vec![
                    TestArg::Scalar(Value::Bool(false)),
                    TestArg::Scalar(num(10.0)),
                    TestArg::Scalar(num(20.0)),
                ]
            ),
            num(20.0)
        );
    }

    // IF.md §2: the unselected branch is NEVER evaluated (poison not forced),
    // even when it would divide by zero or be #UNSUPPORTED!.
    #[test]
    fn if_is_lazy_unselected_branch_not_evaluated() {
        // TRUE → take arg 1; arg 2 is poison and must not be touched.
        assert_eq!(
            call(
                "IF",
                vec![
                    TestArg::Scalar(Value::Bool(true)),
                    TestArg::Scalar(num(1.0)),
                    TestArg::Poison,
                ]
            ),
            num(1.0)
        );
        // FALSE → take arg 2; arg 1 is poison and must not be touched.
        assert_eq!(
            call(
                "IF",
                vec![
                    TestArg::Scalar(Value::Bool(false)),
                    TestArg::Poison,
                    TestArg::Scalar(num(2.0)),
                ]
            ),
            num(2.0)
        );
    }

    // IF.md §Coercion: test coerced via to_bool (nonzero → TRUE).
    #[test]
    fn if_test_coercion() {
        assert_eq!(
            call(
                "IF",
                vec![
                    TestArg::Scalar(num(7.0)),
                    TestArg::Scalar(txt("yes")),
                    TestArg::Scalar(txt("no")),
                ]
            ),
            txt("yes")
        );
    }

    // IF.md §Error behavior: an error in the test propagates (and neither
    // branch is evaluated).
    #[test]
    fn if_test_error_propagates() {
        assert_eq!(
            call(
                "IF",
                vec![
                    TestArg::Scalar(Value::Error(ErrorKind::Na)),
                    TestArg::Poison,
                    TestArg::Poison,
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // OXP-080 (RUN-2026-07-16-oracle01, Excel 16.0): selected-branch elision is
    // keyed on present-but-elided (→ number 0) vs absent-altogether (→ FALSE).
    #[test]
    fn if_oxp080_elided_selected_branch() {
        // H1 IF(TRUE,) — present-but-elided true-branch → 0.
        assert_eq!(
            call(
                "IF",
                vec![TestArg::Scalar(Value::Bool(true)), TestArg::Omitted]
            ),
            num(0.0)
        );
        // H2 IF(TRUE,,9) — elided true-branch selected → 0 (the 9 is unselected).
        assert_eq!(
            call(
                "IF",
                vec![
                    TestArg::Scalar(Value::Bool(true)),
                    TestArg::Omitted,
                    TestArg::Scalar(num(9.0)),
                ]
            ),
            num(0.0)
        );
        // H3 IF(FALSE,1) — false-branch ABSENT altogether → FALSE (a bool).
        assert_eq!(
            call(
                "IF",
                vec![
                    TestArg::Scalar(Value::Bool(false)),
                    TestArg::Scalar(num(1.0)),
                ]
            ),
            Value::Bool(false)
        );
        // H4 IF(FALSE,1,) — present-but-elided false-branch → 0.
        assert_eq!(
            call(
                "IF",
                vec![
                    TestArg::Scalar(Value::Bool(false)),
                    TestArg::Scalar(num(1.0)),
                    TestArg::Omitted,
                ]
            ),
            num(0.0)
        );
        // H5 IF(1,,) — both branches elided, truthy cond selects the elided
        // true-branch → 0.
        assert_eq!(
            call(
                "IF",
                vec![
                    TestArg::Scalar(num(1.0)),
                    TestArg::Omitted,
                    TestArg::Omitted
                ]
            ),
            num(0.0)
        );
    }

    #[test]
    fn operator_arithmetic_and_coercion() {
        use xl_ast::BinaryOp;
        // "1"+1 = 2 (scalar text coerces).
        assert_eq!(apply_binary(BinaryOp::Add, &txt("1"), &num(1.0)), num(2.0));
        // TRUE*3 = 3.
        assert_eq!(
            apply_binary(BinaryOp::Mul, &Value::Bool(true), &num(3.0)),
            num(3.0)
        );
        // Blank acts as 0.
        assert_eq!(
            apply_binary(BinaryOp::Add, &Value::Blank, &num(4.0)),
            num(4.0)
        );
    }

    #[test]
    fn operator_div_by_zero_is_div0() {
        use xl_ast::BinaryOp;
        assert_eq!(
            apply_binary(BinaryOp::Div, &num(1.0), &num(0.0)),
            Value::Error(ErrorKind::Div0)
        );
    }

    /// OXP-081 (RUN-2026-07-11-oracle01): the `^` operator's edge cases, one
    /// assertion per probed cell.
    #[test]
    fn operator_power_oxp081_resolved() {
        use xl_ast::BinaryOp;
        let pow = |b: f64, e: f64| apply_binary(BinaryOp::Pow, &num(b), &num(e));

        // Ordinary power still works.
        assert_eq!(pow(2.0, 10.0), num(1024.0));

        // A1 `=0^0` → #NUM!
        assert_eq!(pow(0.0, 0.0), Value::Error(ErrorKind::Num));
        // A2 `=0^-1` → #DIV/0!  A3 `=0^-2` → #DIV/0!
        assert_eq!(pow(0.0, -1.0), Value::Error(ErrorKind::Div0));
        assert_eq!(pow(0.0, -2.0), Value::Error(ErrorKind::Div0));
        // A5 `=(-2)^0.5` → #NUM! (even reciprocal → complex root)
        assert_eq!(pow(-2.0, 0.5), Value::Error(ErrorKind::Num));
        // A6 `=(-2)^2` → 4 ; A7 `=(-2)^-3` → -0.125 (integer exponents)
        assert_eq!(pow(-2.0, 2.0), num(4.0));
        assert_eq!(pow(-2.0, -3.0), num(-0.125));

        // A4 `=(-8)^(1/3)` → -1.9999999999999998 (Excel x86 `pow`); this
        // platform's `powf` may give exactly -2.0, within float tolerance.
        match pow(-8.0, 1.0 / 3.0) {
            Value::Number(n) => {
                assert!(
                    (n - -1.999_999_999_999_999_8_f64).abs() < 1e-12,
                    "(-8)^(1/3) = {n}, expected ≈ -1.9999999999999998"
                );
            }
            other => panic!("(-8)^(1/3) should be a real number, got {other:?}"),
        }
    }

    #[test]
    fn operator_concat_uses_to_text() {
        use xl_ast::BinaryOp;
        assert_eq!(
            apply_binary(BinaryOp::Concat, &txt("a"), &num(1.0)),
            txt("a1")
        );
        assert_eq!(
            apply_binary(BinaryOp::Concat, &Value::Bool(true), &txt("!")),
            txt("TRUE!")
        );
    }

    #[test]
    fn operator_comparison_and_blank_bool_morph() {
        use xl_ast::BinaryOp;
        assert_eq!(
            apply_binary(BinaryOp::Lt, &num(1.0), &num(2.0)),
            Value::Bool(true)
        );
        // Case-insensitive text equality.
        assert_eq!(
            apply_binary(BinaryOp::Eq, &txt("abc"), &txt("ABC")),
            Value::Bool(true)
        );
        // OXP-030 (RUN-2026-07-11-oracle01, in xl-value): a Blank compared with
        // a Bool morphs to Bool(false), so `<Blank> = TRUE` is FALSE.
        assert_eq!(
            apply_binary(BinaryOp::Eq, &Value::Blank, &Value::Bool(true)),
            Value::Bool(false)
        );
        // Error propagates leftmost.
        assert_eq!(
            apply_binary(BinaryOp::Gt, &Value::Error(ErrorKind::Na), &num(1.0)),
            Value::Error(ErrorKind::Na)
        );
    }

    #[test]
    fn operator_fuzzy_comparison_rfc0009() {
        // RFC 0009 / OXP-182: the six comparison OPERATORS use Excel's ~15-sig-fig
        // fuzz. A fuzz-equal pair (1 vs 1+4e-15) — the pinned six-operator row,
        // asserted through the switched surface (`apply_binary`).
        use xl_ast::BinaryOp;
        let (a, b) = (num(1.0), num(1.0 + 4e-15));
        let t = Value::Bool(true);
        let f = Value::Bool(false);
        assert_eq!(apply_binary(BinaryOp::Eq, &a, &b), t); // =  TRUE
        assert_eq!(apply_binary(BinaryOp::Le, &a, &b), t); // <= TRUE
        assert_eq!(apply_binary(BinaryOp::Ge, &a, &b), t); // >= TRUE
        assert_eq!(apply_binary(BinaryOp::Lt, &a, &b), f); // <  FALSE
        assert_eq!(apply_binary(BinaryOp::Gt, &a, &b), f); // >  FALSE
        assert_eq!(apply_binary(BinaryOp::Ne, &a, &b), f); // <> FALSE
        // Canonical: 0.1+0.2 = 0.3 is TRUE for the operator.
        assert_eq!(
            apply_binary(BinaryOp::Eq, &num(0.1 + 0.2), &num(0.3)),
            Value::Bool(true)
        );
        // A clearly-different pair still orders exactly.
        assert_eq!(
            apply_binary(BinaryOp::Lt, &num(1.0), &num(2.0)),
            Value::Bool(true)
        );
    }

    #[test]
    fn operator_unary_and_postfix() {
        use xl_ast::{PostfixOp, UnaryOp};
        // Unary `-` coerces to a number and negates.
        assert_eq!(apply_unary(UnaryOp::Neg, &num(3.0)), num(-3.0));
        assert_eq!(apply_unary(UnaryOp::Neg, &txt("1")), num(-1.0));
        assert_eq!(apply_unary(UnaryOp::Neg, &Value::Bool(true)), num(-1.0));
        assert_eq!(
            apply_unary(UnaryOp::Neg, &txt("abc")),
            Value::Error(ErrorKind::Value)
        );
        // Unary `+` is a TYPE-PRESERVING no-op (OXP-175, Excel build 16.0):
        // text (numeric or not), bool, number, and error pass through
        // unchanged; only Blank coerces to 0.
        assert_eq!(apply_unary(UnaryOp::Plus, &txt("1")), txt("1")); // NOT num(1.0)
        assert_eq!(apply_unary(UnaryOp::Plus, &txt("abc")), txt("abc"));
        assert_eq!(apply_unary(UnaryOp::Plus, &txt("")), txt("")); // H11: "" not Blank/0
        assert_eq!(
            apply_unary(UnaryOp::Plus, &Value::Bool(true)),
            Value::Bool(true)
        );
        assert_eq!(apply_unary(UnaryOp::Plus, &num(3.0)), num(3.0));
        assert_eq!(
            apply_unary(UnaryOp::Plus, &Value::Error(ErrorKind::Na)),
            Value::Error(ErrorKind::Na)
        );
        assert_eq!(apply_unary(UnaryOp::Plus, &Value::Blank), num(0.0));
        assert_eq!(apply_postfix(PostfixOp::Percent, &num(50.0)), num(0.5));
    }

    // OXP-176 (RUN-2026-07-16-oracle01, Excel 16.0): unary `+` over a **1×1
    // array literal** preserves the element's type, mirroring the scalar
    // OXP-175 rule — `=+{"abc"}` → "abc", `=+{5}` → 5, `=+{TRUE}` → TRUE. A 1×1
    // array collapses to its element and re-applies the scalar `+` rule.
    #[test]
    fn operator_unary_plus_over_1x1_array_preserves_type_oxp176() {
        use xl_ast::UnaryOp;
        let one = |v: Value| Value::Array(xl_value::Array::new(1, 1, vec![v]).unwrap());
        // A1 =+{"abc"} → the text "abc" (NOT #VALUE!, NOT the number path).
        assert_eq!(apply_unary(UnaryOp::Plus, &one(txt("abc"))), txt("abc"));
        // A2 =+{5} → 5.
        assert_eq!(apply_unary(UnaryOp::Plus, &one(num(5.0))), num(5.0));
        // A3 =+{TRUE} → TRUE (a bool, not 1).
        assert_eq!(
            apply_unary(UnaryOp::Plus, &one(Value::Bool(true))),
            Value::Bool(true)
        );
    }

    #[test]
    fn registry_lookup_and_arity() {
        assert!(lookup("sum").is_some(), "case-insensitive");
        assert!(lookup("SUM").is_some());
        assert!(lookup("NOTAFUNCTION").is_none());

        let sum = lookup("SUM").unwrap();
        assert!(!sum.accepts_arity(0));
        assert!(sum.accepts_arity(1));
        assert!(sum.accepts_arity(255));
        assert!(!sum.accepts_arity(256));

        let iff = lookup("IF").unwrap();
        assert!(!iff.accepts_arity(1));
        assert!(iff.accepts_arity(2));
        assert!(iff.accepts_arity(3));
        assert!(!iff.accepts_arity(4));
    }

    #[test]
    fn volatile_names_marked_even_when_unimplemented() {
        // Not implemented, but must still flag volatility for the graph.
        assert!(is_volatile("NOW"));
        assert!(is_volatile("today"));
        assert!(is_volatile("RAND"));
        assert!(is_volatile("OFFSET"));
        assert!(lookup("NOW").is_none());
        // Implemented + non-volatile.
        assert!(!is_volatile("SUM"));
        assert!(!is_volatile("IF"));
        // RANDARRAY (lane 3a) is REGISTERED but refuses in v0, so its FnSpec is
        // NonVolatile — yet `is_volatile` must still report it volatile via
        // VOLATILE_NAMES (the graph must mark its cell volatile). This is the
        // registered-but-refusing coherence the contract review's B3 asks be pinned.
        assert!(lookup("RANDARRAY").is_some(), "RANDARRAY is registered");
        assert!(
            is_volatile("RANDARRAY"),
            "RANDARRAY stays volatile via VOLATILE_NAMES despite a NonVolatile FnSpec"
        );
        assert!(is_volatile("randarray"), "case-insensitive");
    }

    #[test]
    fn eval_context_capabilities_are_unsupported_v0() {
        let ctx = EvalContext::new();
        assert_eq!(ctx.now(), Err(ErrorKind::Unsupported));
        assert_eq!(ctx.today(), Err(ErrorKind::Unsupported));
        assert_eq!(ctx.rand(), Err(ErrorKind::Unsupported));
    }

    #[test]
    fn eval_context_defaults_to_1900() {
        assert_eq!(EvalContext::new().date_system(), DateSystem::Excel1900);
        assert_eq!(EvalContext::default().date_system(), DateSystem::Excel1900);
        assert_eq!(
            EvalContext::with_date_system(DateSystem::Excel1904).date_system(),
            DateSystem::Excel1904
        );
    }

    // YEAR/MONTH/DAY.md §leap-bug: serial 60 is the fictitious 1900-02-29.
    // This is the marquee fidelity probe — pin it hard.
    #[test]
    fn date_components_hit_the_1900_leap_bug_at_serial_60() {
        assert_eq!(call("YEAR", vec![TestArg::Scalar(num(60.0))]), num(1900.0));
        assert_eq!(call("MONTH", vec![TestArg::Scalar(num(60.0))]), num(2.0));
        assert_eq!(call("DAY", vec![TestArg::Scalar(num(60.0))]), num(29.0));
        // The seam neighbours: 59 → 1900-02-28, 61 → 1900-03-01.
        assert_eq!(call("DAY", vec![TestArg::Scalar(num(59.0))]), num(28.0));
        assert_eq!(call("MONTH", vec![TestArg::Scalar(num(61.0))]), num(3.0));
        assert_eq!(call("DAY", vec![TestArg::Scalar(num(61.0))]), num(1.0));
    }

    // YEAR.md §1: ordinary date. Serial 43831 = 2020-01-01.
    #[test]
    fn date_components_ordinary_date() {
        let s = num(43831.0);
        assert_eq!(call("YEAR", vec![TestArg::Scalar(s.clone())]), num(2020.0));
        assert_eq!(call("MONTH", vec![TestArg::Scalar(s.clone())]), num(1.0));
        assert_eq!(call("DAY", vec![TestArg::Scalar(s)]), num(1.0));
    }

    // Coercion (YEAR.md §Coercion): numeric text and bool coerce via xl-value;
    // a fractional serial floors to the day.
    #[test]
    fn date_components_coercion_and_flooring() {
        // "60" (numeric text) → serial 60.
        assert_eq!(call("DAY", vec![TestArg::Scalar(txt("60"))]), num(29.0));
        // 60.9 floors to serial 60.
        assert_eq!(call("DAY", vec![TestArg::Scalar(num(60.9))]), num(29.0));
        // TRUE → serial 1 → 1900-01-01.
        assert_eq!(
            call("YEAR", vec![TestArg::Scalar(Value::Bool(true))]),
            num(1900.0)
        );
    }

    // YEAR.md §Error behavior: negative serial → #NUM!.
    #[test]
    fn date_components_negative_serial_is_num() {
        assert_eq!(
            call("YEAR", vec![TestArg::Scalar(num(-1.0))]),
            Value::Error(ErrorKind::Num)
        );
        // Non-numeric text → #VALUE! (via xl-value coercion).
        assert_eq!(
            call("MONTH", vec![TestArg::Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // OXP-090 RESOLVED (RUN-2026-07-11-oracle01): serial 0 ("January 0, 1900")
    // — and a Blank arg (coerces to 0) — reads as YEAR=1900, MONTH=1, DAY=0.
    #[test]
    fn date_components_serial_zero_is_january_zero_1900() {
        assert_eq!(call("YEAR", vec![TestArg::Scalar(num(0.0))]), num(1900.0));
        assert_eq!(call("MONTH", vec![TestArg::Scalar(num(0.0))]), num(1.0));
        assert_eq!(call("DAY", vec![TestArg::Scalar(num(0.0))]), num(0.0));
        // A blank YEAR/DAY argument coerces to serial 0 and reads the same.
        assert_eq!(
            call("YEAR", vec![TestArg::Scalar(Value::Blank)]),
            num(1900.0)
        );
        assert_eq!(call("DAY", vec![TestArg::Scalar(Value::Blank)]), num(0.0));
    }

    // DATE.md §6 / 1904: serial 60 in the 1904 system is a *real* date
    // (1904-03-01), no phantom leap day — proves the date system is threaded.
    #[test]
    fn date_components_respect_1904_system() {
        assert_eq!(
            call_ds(
                "YEAR",
                vec![TestArg::Scalar(num(0.0))],
                DateSystem::Excel1904
            ),
            num(1904.0)
        );
        // 1904 serial 59 is the real 1904-02-29 (1904 is a genuine leap year).
        assert_eq!(
            call_ds(
                "DAY",
                vec![TestArg::Scalar(num(59.0))],
                DateSystem::Excel1904
            ),
            num(29.0)
        );
        assert_eq!(
            call_ds(
                "MONTH",
                vec![TestArg::Scalar(num(60.0))],
                DateSystem::Excel1904
            ),
            num(3.0)
        );
    }

    // DATE.md §5: DATE(1900,2,29) = serial 60 (forward proof of the bug).
    #[test]
    fn date_builds_the_phantom_leap_day() {
        assert_eq!(
            call(
                "DATE",
                vec![
                    TestArg::Scalar(num(1900.0)),
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(num(29.0)),
                ]
            ),
            num(60.0)
        );
    }

    // DATE.md §1: ordinary construction. DATE(2020,1,1) = 43831.
    #[test]
    fn date_ordinary_construction() {
        assert_eq!(
            call(
                "DATE",
                vec![
                    TestArg::Scalar(num(2020.0)),
                    TestArg::Scalar(num(1.0)),
                    TestArg::Scalar(num(1.0)),
                ]
            ),
            num(43831.0)
        );
    }

    // DATE.md §3/§4: month and day overflow normalization (documented cases).
    #[test]
    fn date_overflow_normalization() {
        let date = |y: f64, m: f64, d: f64| {
            call(
                "DATE",
                vec![
                    TestArg::Scalar(num(y)),
                    TestArg::Scalar(num(m)),
                    TestArg::Scalar(num(d)),
                ],
            )
        };
        assert_eq!(date(2020.0, 13.0, 1.0), date(2021.0, 1.0, 1.0));
        assert_eq!(date(2020.0, 0.0, 1.0), date(2019.0, 12.0, 1.0));
        assert_eq!(date(2020.0, 1.0, 32.0), date(2020.0, 2.0, 1.0));
        assert_eq!(date(2020.0, 1.0, 0.0), date(2019.0, 12.0, 31.0));
    }

    // DATE.md §2: a year in 0..=1899 has 1900 added (DATE(105,1,2) → 2005).
    #[test]
    fn date_year_auto_add_1900() {
        assert_eq!(
            call(
                "DATE",
                vec![
                    TestArg::Scalar(num(105.0)),
                    TestArg::Scalar(num(1.0)),
                    TestArg::Scalar(num(2.0)),
                ]
            ),
            call(
                "DATE",
                vec![
                    TestArg::Scalar(num(2005.0)),
                    TestArg::Scalar(num(1.0)),
                    TestArg::Scalar(num(2.0)),
                ]
            )
        );
    }

    // DATE.md §Error behavior: a result past 9999-12-31 → #NUM!.
    #[test]
    fn date_out_of_range_is_num() {
        assert_eq!(
            call(
                "DATE",
                vec![
                    TestArg::Scalar(num(10000.0)),
                    TestArg::Scalar(num(1.0)),
                    TestArg::Scalar(num(1.0)),
                ]
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    // OXP-091 (RUN-2026-07-11-oracle01): a non-integer year/month/day argument
    // FLOORS toward negative infinity. =DATE(2020,-1.5,1) -> 43739 (month → -2,
    // NOT -1); =DATE(2020,1.5,1) -> 43831 (month → 1).
    #[test]
    fn date_non_integer_argument_floors() {
        assert_eq!(
            call(
                "DATE",
                vec![
                    TestArg::Scalar(num(2020.0)),
                    TestArg::Scalar(num(1.5)),
                    TestArg::Scalar(num(1.0)),
                ]
            ),
            num(43831.0)
        );
        assert_eq!(
            call(
                "DATE",
                vec![
                    TestArg::Scalar(num(2020.0)),
                    TestArg::Scalar(num(-1.5)),
                    TestArg::Scalar(num(1.0)),
                ]
            ),
            num(43739.0)
        );
    }

    // EOMONTH.md §6: EOMONTH(DATE(1900,1,1),1) = serial 60 (Feb 1900 = 29 days).
    #[test]
    fn eomonth_hits_the_1900_leap_bug() {
        assert_eq!(
            call(
                "EOMONTH",
                vec![TestArg::Scalar(num(1.0)), TestArg::Scalar(num(1.0))]
            ),
            num(60.0)
        );
    }

    // EOMONTH.md §1-4: forward / zero / backward from 2020-01-15 (serial 43845).
    #[test]
    fn eomonth_forward_zero_backward() {
        let jan15 = 43845.0;
        // months=0 → 2020-01-31 (serial 43861).
        assert_eq!(
            call(
                "EOMONTH",
                vec![TestArg::Scalar(num(jan15)), TestArg::Scalar(num(0.0))]
            ),
            num(43861.0)
        );
        // months=1 → 2020-02-29 (leap; serial 43890).
        assert_eq!(
            call(
                "EOMONTH",
                vec![TestArg::Scalar(num(jan15)), TestArg::Scalar(num(1.0))]
            ),
            num(43890.0)
        );
        // months=-1 → 2019-12-31 (serial 43830).
        assert_eq!(
            call(
                "EOMONTH",
                vec![TestArg::Scalar(num(jan15)), TestArg::Scalar(num(-1.0))]
            ),
            num(43830.0)
        );
    }

    // OXP-092 (RUN-2026-07-11-oracle01): a non-integer `months` TRUNCATES toward
    // zero. =EOMONTH(DATE(2020,1,15),1.9) -> 43890 (months → 1);
    // =EOMONTH(DATE(2020,1,15),-1.9) -> 43830 (months → -1, NOT -2).
    #[test]
    fn eomonth_non_integer_months_truncates() {
        assert_eq!(
            call(
                "EOMONTH",
                vec![TestArg::Scalar(num(43845.0)), TestArg::Scalar(num(1.9))]
            ),
            num(43890.0)
        );
        assert_eq!(
            call(
                "EOMONTH",
                vec![TestArg::Scalar(num(43845.0)), TestArg::Scalar(num(-1.9))]
            ),
            num(43830.0)
        );
    }

    // The date family is registered with the documented arities and is
    // non-volatile.
    #[test]
    fn date_family_registered_with_expected_arity() {
        for (name, min, max) in [
            ("YEAR", 1, 1),
            ("MONTH", 1, 1),
            ("DAY", 1, 1),
            ("DATE", 3, 3),
            ("EOMONTH", 2, 2),
        ] {
            let spec = lookup(name).unwrap_or_else(|| panic!("{name} registered"));
            assert!(!spec.accepts_arity(min - 1), "{name} min arity");
            assert!(spec.accepts_arity(min));
            assert!(spec.accepts_arity(max));
            assert!(!spec.accepts_arity(max + 1), "{name} max arity");
            assert!(!is_volatile(name), "{name} is non-volatile");
        }
    }

    /// A row-major `rows`×`cols` table argument.
    fn table(rows: u32, cols: u32, data: Vec<Value>) -> TestArg {
        TestArg::Rect { rows, cols, data }
    }

    // VLOOKUP.md §2: exact match (range_lookup=FALSE) returns the col_index_num
    // value of the first equal first-column cell.
    #[test]
    fn vlookup_exact_hit() {
        let t = table(
            3,
            2,
            vec![
                txt("apple"),
                num(10.0),
                txt("banana"),
                num(20.0),
                txt("cherry"),
                num(30.0),
            ],
        );
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(txt("banana")),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            num(20.0)
        );
    }

    // VLOOKUP.md §4: exact match with no equal key → #N/A.
    #[test]
    fn vlookup_exact_miss_is_na() {
        let t = table(2, 2, vec![txt("a"), num(1.0), txt("b"), num(2.0)]);
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(txt("zzz")),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // VLOOKUP.md §2: exact-match text comparison is case-insensitive (deferred
    // to xl-value's `values_equal`).
    #[test]
    fn vlookup_exact_case_insensitive_text() {
        let t = table(2, 2, vec![txt("Apple"), num(1.0), txt("Banana"), num(2.0)]);
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(txt("BANANA")),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            num(2.0)
        );
    }

    // VLOOKUP.md §2: duplicate keys → the first (topmost) match wins.
    #[test]
    fn vlookup_exact_first_of_duplicates() {
        let t = table(
            3,
            2,
            vec![
                txt("k"),
                num(100.0),
                txt("k"),
                num(200.0),
                txt("k"),
                num(300.0),
            ],
        );
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(txt("k")),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            num(100.0)
        );
    }

    // VLOOKUP.md §Coercion / §Semantics: the search is the FIRST column only. A
    // value that appears only in a later column is not found.
    #[test]
    fn vlookup_searches_first_column_only() {
        // `find` sits in column 2, never column 1.
        let t = table(2, 2, vec![txt("a"), txt("find"), txt("b"), num(9.0)]);
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(txt("find")),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // VLOOKUP.md §Coercion: cross-type equality is xl-value's — number 1 does
    // NOT equal text "1", so exact match misses (→ #N/A).
    #[test]
    fn vlookup_exact_cross_type_number_vs_text_no_match() {
        let t = table(2, 2, vec![txt("1"), num(11.0), txt("2"), num(22.0)]);
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(num(1.0)),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // VLOOKUP.md §3: approximate match (range_lookup omitted → TRUE) on sorted
    // data returns the largest key <= lookup_value. Golden probe: keys 1,3,5,7.
    #[test]
    fn vlookup_approx_sorted_largest_le() {
        let sorted = || {
            table(
                4,
                2,
                vec![
                    num(1.0),
                    txt("one"),
                    num(3.0),
                    txt("three"),
                    num(5.0),
                    txt("five"),
                    num(7.0),
                    txt("seven"),
                ],
            )
        };
        // lookup 4 → largest <= is 3 → "three".
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(num(4.0)),
                    sorted(),
                    TestArg::Scalar(num(2.0))
                ]
            ),
            txt("three")
        );
        // lookup 100 → largest <= is 7 → "seven".
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(num(100.0)),
                    sorted(),
                    TestArg::Scalar(num(2.0))
                ]
            ),
            txt("seven")
        );
    }

    // VLOOKUP.md §3: an *exact* key present in approximate mode is found on the
    // boundary (the binary search lands on the equal key).
    #[test]
    fn vlookup_approx_exact_boundary_hit() {
        let t = table(
            4,
            2,
            vec![
                num(1.0),
                txt("one"),
                num(3.0),
                txt("three"),
                num(5.0),
                txt("five"),
                num(7.0),
                txt("seven"),
            ],
        );
        assert_eq!(
            call(
                "VLOOKUP",
                vec![TestArg::Scalar(num(5.0)), t, TestArg::Scalar(num(2.0))]
            ),
            txt("five")
        );
    }

    // VLOOKUP.md §4: approximate match where lookup_value is smaller than every
    // key → #N/A.
    #[test]
    fn vlookup_approx_smaller_than_all_is_na() {
        let t = table(
            3,
            2,
            vec![
                num(10.0),
                num(1.0),
                num(20.0),
                num(2.0),
                num(30.0),
                num(3.0),
            ],
        );
        assert_eq!(
            call(
                "VLOOKUP",
                vec![TestArg::Scalar(num(5.0)), t, TestArg::Scalar(num(2.0))]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // VLOOKUP.md §6: col_index_num < 1 → #VALUE!.
    #[test]
    fn vlookup_col_index_below_one_is_value() {
        let t = table(1, 2, vec![num(1.0), num(2.0)]);
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(num(1.0)),
                    t,
                    TestArg::Scalar(num(0.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    // VLOOKUP.md §5: col_index_num greater than the table width → #REF!.
    #[test]
    fn vlookup_col_index_past_width_is_ref() {
        let t = table(1, 2, vec![num(1.0), num(2.0)]);
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(num(1.0)),
                    t,
                    TestArg::Scalar(num(3.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    // VLOOKUP.md §Error behavior: an error in lookup_value propagates.
    #[test]
    fn vlookup_error_lookup_value_propagates() {
        let t = table(1, 2, vec![num(1.0), num(2.0)]);
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(Value::Error(ErrorKind::Na)),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // VLOOKUP.md §Error behavior: an error sitting in the returned cell of the
    // matched row propagates as VLOOKUP's result.
    #[test]
    fn vlookup_error_in_returned_cell_propagates() {
        let t = table(
            2,
            2,
            vec![txt("a"), Value::Error(ErrorKind::Div0), txt("b"), num(2.0)],
        );
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(txt("a")),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    // RFC 0001: a whole-column table_array is served via the used-extent walk
    // (populated rows only), not refused. Exact match finds the key.
    #[test]
    fn vlookup_whole_column_table_used_extent() {
        // Single-column A:A [1,2,3] with a second column would be UsedRows; here
        // model A:B via explicit used-extent rows so col_index 2 is meaningful.
        let table = TestArg::UsedRows(vec![
            (0, vec![num(1.0), num(10.0)]),
            (1, vec![num(2.0), num(20.0)]),
            (2, vec![num(3.0), num(30.0)]),
        ]);
        // Exact match of 2 → row 1 → col 2 = 20.
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(num(2.0)),
                    table,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            num(20.0)
        );
    }

    // RFC 0001: a whole-column table with GAPS (populated rows 0 and 5 only).
    // Exact match skips the absent (blank) rows correctly; a missing key → #N/A.
    #[test]
    fn vlookup_whole_column_gaps_exact() {
        let table = TestArg::UsedRows(vec![
            (0, vec![num(1.0), num(10.0)]),
            (5, vec![num(9.0), num(90.0)]),
        ]);
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(num(9.0)),
                    TestArg::UsedRows(vec![
                        (0, vec![num(1.0), num(10.0)]),
                        (5, vec![num(9.0), num(90.0)]),
                    ]),
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            num(90.0)
        );
        // A key not present → #N/A (not a spurious match).
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(num(5.0)),
                    table,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // RFC 0001 / OXP-104 H3 (RUN-2026-07-11-oracle01, updated by L2-A): a Blank
    // lookup_value over a whole column whose populated cells are all confirmed
    // no-matches is pinned to #N/A — OXP-104 also pins that a Blank key matches
    // no truly-blank (absent) row, so the invisible rows cannot hide a match.
    // (The pre-L2-A blanket defer here was an extra-conservative engine policy,
    // not the oracle answer.)
    #[test]
    fn vlookup_whole_column_blank_lookup_no_match_is_na() {
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(Value::Blank),
                    TestArg::Unbounded(vec![num(1.0), num(2.0)]),
                    TestArg::Scalar(num(1.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // OXP-089 (RUN-2026-07-11-oracle01): a text lookup_value carrying a wildcard
    // in exact mode is honored — "ap*" matches "apple" (first) → its column-2
    // value.
    #[test]
    fn vlookup_exact_wildcard_matches_first() {
        let t = table(2, 2, vec![txt("apple"), num(1.0), txt("apricot"), num(2.0)]);
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(txt("ap*")),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            num(1.0)
        );
    }

    // OXP-089 (RUN-2026-07-11-oracle01): a non-integer col_index_num truncates
    // toward zero — 2.5 → column 2.
    #[test]
    fn vlookup_non_integer_col_index_truncates_toward_zero() {
        let t = table(1, 3, vec![num(1.0), num(2.0), num(3.0)]);
        assert_eq!(
            call(
                "VLOOKUP",
                vec![
                    TestArg::Scalar(num(1.0)),
                    t,
                    TestArg::Scalar(num(2.5)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            num(2.0)
        );
    }

    // VLOOKUP is registered with the documented 3..=4 arity and is non-volatile.
    #[test]
    fn vlookup_registered_with_expected_arity() {
        let spec = lookup("VLOOKUP").expect("VLOOKUP registered");
        assert!(!spec.accepts_arity(2), "min 3 args");
        assert!(spec.accepts_arity(3));
        assert!(spec.accepts_arity(4));
        assert!(!spec.accepts_arity(5), "max 4 args");
        assert!(!is_volatile("VLOOKUP"));
    }

    // `dims` distinguishes scalar (None) from a bounded range/array (Some).
    #[test]
    fn dims_of_scalar_range_array_and_rect() {
        let mut ta = TestArgs {
            args: vec![
                TestArg::Scalar(num(1.0)),
                TestArg::Range(vec![num(1.0), num(2.0), num(3.0)]),
                TestArg::Array(vec![num(1.0), num(2.0)]),
                TestArg::Rect {
                    rows: 2,
                    cols: 3,
                    data: vec![num(1.0); 6],
                },
                TestArg::Omitted,
            ],
        };
        assert_eq!(ta.dims(0), None, "scalar has no rectangle");
        assert_eq!(ta.dims(1), Some((3, 1)), "column");
        assert_eq!(ta.dims(2), Some((1, 2)), "row");
        assert_eq!(ta.dims(3), Some((2, 3)), "explicit rect");
        assert_eq!(ta.dims(4), None, "omitted has no rectangle");
        assert_eq!(ta.dims(9), None, "out of range");
    }

    // Documented whole-column policy: `dims` reports None and `for_each_row`
    // refuses an unbounded range.
    #[test]
    fn unbounded_range_refuses_dense_walk() {
        let mut ta = TestArgs {
            args: vec![TestArg::Unbounded(vec![num(1.0), num(2.0)])],
        };
        assert_eq!(ta.dims(0), None);
        let mut rows = 0usize;
        let res = ta.for_each_row(0, &mut |_| {
            rows += 1;
            ControlFlow::Continue(())
        });
        assert_eq!(res, Err(ErrorKind::Unsupported));
        assert_eq!(rows, 0, "no row is visited when the walk is refused");
    }

    // `for_each_row` surfaces blanks positionally (unlike `for_each_cell`,
    // which elides them).
    #[test]
    fn for_each_row_yields_blanks_in_rect() {
        // A 2×2 rectangle with a hole at (0,1) and (1,0).
        let mut ta = TestArgs {
            args: vec![TestArg::Rect {
                rows: 2,
                cols: 2,
                data: vec![num(1.0), Value::Blank, Value::Blank, num(4.0)],
            }],
        };
        let mut seen: Vec<Vec<Value>> = Vec::new();
        let res = ta.for_each_row(0, &mut |row| {
            seen.push(row.to_vec());
            ControlFlow::Continue(())
        });
        assert_eq!(res, Ok(()));
        assert_eq!(
            seen,
            vec![vec![num(1.0), Value::Blank], vec![Value::Blank, num(4.0)],]
        );
    }

    // `ControlFlow::Break` stops the row walk after the current row.
    #[test]
    fn for_each_row_early_termination() {
        let mut ta = TestArgs {
            args: vec![TestArg::Rect {
                rows: 3,
                cols: 1,
                data: vec![num(1.0), num(2.0), num(3.0)],
            }],
        };
        let mut visited = 0usize;
        let res = ta.for_each_row(0, &mut |_| {
            visited += 1;
            ControlFlow::Break(()) // stop after the first row
        });
        assert_eq!(res, Ok(()));
        assert_eq!(visited, 1, "walk stopped after the first row");
    }

    // RFC 0001: `for_each_used_row` over a sparse (gapped, multi-column) range
    // yields the populated rows in ascending order, each as a full-column-span
    // slice with blanks within the span, carrying the relative row index.
    #[test]
    fn for_each_used_row_sparse_rows_in_order() {
        let mut ta = TestArgs {
            args: vec![TestArg::UsedRows(vec![
                (0, vec![num(1.0), Value::Blank]),
                (3, vec![Value::Blank, num(4.0)]),
                (7, vec![num(7.0), num(8.0)]),
            ])],
        };
        let mut seen: Vec<(u32, Vec<Value>)> = Vec::new();
        let res = ta.for_each_used_row(0, &mut |rel, row| {
            seen.push((rel, row.to_vec()));
            ControlFlow::Continue(())
        });
        assert_eq!(res, Ok(()));
        assert_eq!(
            seen,
            vec![
                (0, vec![num(1.0), Value::Blank]),
                (3, vec![Value::Blank, num(4.0)]),
                (7, vec![num(7.0), num(8.0)]),
            ]
        );
        // Ascending relative-row order is preserved.
        assert!(seen.windows(2).all(|w| w[0].0 < w[1].0));
    }

    // RFC 0001: `for_each_row_or_used` prefers the dense walk for a bounded
    // argument (reporting `Ok(false)`) and falls back to the used-extent walk
    // for a whole-column one (reporting `Ok(true)`), yielding the same rows.
    #[test]
    fn for_each_row_or_used_dense_then_used_extent() {
        use crate::args::for_each_row_or_used;
        // Bounded rect → dense path.
        let mut bounded = TestArgs {
            args: vec![TestArg::Rect {
                rows: 2,
                cols: 1,
                data: vec![num(1.0), num(2.0)],
            }],
        };
        let mut dense_rows: Vec<(u32, Vec<Value>)> = Vec::new();
        let used = for_each_row_or_used(&mut bounded, 0, &mut |rel, row| {
            dense_rows.push((rel, row.to_vec()));
            ControlFlow::Continue(())
        });
        assert_eq!(used, Ok(false), "bounded argument uses the dense path");
        assert_eq!(dense_rows, vec![(0, vec![num(1.0)]), (1, vec![num(2.0)])]);

        // Whole-column → used-extent path (dense walk refuses, then falls back).
        let mut whole = TestArgs {
            args: vec![TestArg::Unbounded(vec![num(6.0), num(7.0)])],
        };
        let mut ext_rows: Vec<(u32, Vec<Value>)> = Vec::new();
        let used = for_each_row_or_used(&mut whole, 0, &mut |rel, row| {
            ext_rows.push((rel, row.to_vec()));
            ControlFlow::Continue(())
        });
        assert_eq!(
            used,
            Ok(true),
            "whole-column argument uses the used-extent path"
        );
        assert_eq!(ext_rows, vec![(0, vec![num(6.0)]), (1, vec![num(7.0)])]);
    }

    // A counting probe proving SUM short-circuits `for_each_cell` at the first
    // error instead of visiting-and-ignoring the rest of the range.
    struct CountingArgs {
        values: Vec<Value>,
        visited: usize,
    }

    impl CallArgs for CountingArgs {
        fn count(&self) -> usize {
            1
        }
        fn shape(&mut self, index: usize) -> ArgShape {
            if index == 0 {
                ArgShape::Range
            } else {
                ArgShape::Omitted
            }
        }
        fn eval_scalar(&mut self, _index: usize) -> Value {
            Value::Error(ErrorKind::Unsupported)
        }
        fn for_each_cell(
            &mut self,
            index: usize,
            visit: &mut dyn FnMut(&Value) -> ControlFlow<()>,
        ) {
            if index != 0 {
                return;
            }
            // Clone out first to avoid borrowing `self` across the counter bump.
            let values = self.values.clone();
            for v in &values {
                self.visited += 1;
                if visit(v).is_break() {
                    return;
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
            Ok(())
        }
    }

    #[test]
    fn sum_short_circuits_range_at_first_error() {
        let ctx = EvalContext::new();
        let mut ca = CountingArgs {
            // Error at position 1; positions 2 and 3 must never be visited.
            values: vec![num(1.0), Value::Error(ErrorKind::Div0), num(2.0), num(3.0)],
            visited: 0,
        };
        let spec = lookup("SUM").unwrap();
        let out = (spec.eval)(&ctx, &mut ca);
        assert_eq!(out, Value::Error(ErrorKind::Div0));
        assert_eq!(
            ca.visited, 2,
            "SUM stopped right after the erroring cell (visited 2, not 4)"
        );
    }

    // HLOOKUP.md: exact match (range_lookup=FALSE) returns the row_index_num
    // value of the first equal first-row cell.
    #[test]
    fn hlookup_exact_hit() {
        // Row 0: apple, banana, cherry — row 1: 10, 20, 30.
        let t = table(
            2,
            3,
            vec![
                txt("apple"),
                txt("banana"),
                txt("cherry"),
                num(10.0),
                num(20.0),
                num(30.0),
            ],
        );
        assert_eq!(
            call(
                "HLOOKUP",
                vec![
                    TestArg::Scalar(txt("banana")),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            num(20.0)
        );
    }

    // HLOOKUP.md: exact match with no equal key → #N/A.
    #[test]
    fn hlookup_exact_miss_is_na() {
        let t = table(2, 2, vec![txt("a"), txt("b"), num(1.0), num(2.0)]);
        assert_eq!(
            call(
                "HLOOKUP",
                vec![
                    TestArg::Scalar(txt("zzz")),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // HLOOKUP.md: exact-match text comparison is case-insensitive (deferred
    // to xl-value's `values_equal`).
    #[test]
    fn hlookup_exact_case_insensitive_text() {
        let t = table(2, 2, vec![txt("Apple"), txt("Banana"), num(1.0), num(2.0)]);
        assert_eq!(
            call(
                "HLOOKUP",
                vec![
                    TestArg::Scalar(txt("BANANA")),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            num(2.0)
        );
    }

    // HLOOKUP.md: duplicate keys → the first (leftmost) match wins.
    #[test]
    fn hlookup_exact_first_of_duplicates() {
        let t = table(
            2,
            3,
            vec![
                txt("k"),
                txt("k"),
                txt("k"),
                num(100.0),
                num(200.0),
                num(300.0),
            ],
        );
        assert_eq!(
            call(
                "HLOOKUP",
                vec![
                    TestArg::Scalar(txt("k")),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            num(100.0)
        );
    }

    // HLOOKUP.md: the search is the FIRST row only. A value that appears
    // only in a later row is not found.
    #[test]
    fn hlookup_searches_first_row_only() {
        let t = table(2, 2, vec![txt("a"), txt("b"), txt("find"), num(9.0)]);
        assert_eq!(
            call(
                "HLOOKUP",
                vec![
                    TestArg::Scalar(txt("find")),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // HLOOKUP.md: cross-type equality is xl-value's — number 1 does NOT
    // equal text "1", so exact match misses (→ #N/A).
    #[test]
    fn hlookup_exact_cross_type_number_vs_text_no_match() {
        let t = table(2, 2, vec![txt("1"), txt("2"), num(11.0), num(22.0)]);
        assert_eq!(
            call(
                "HLOOKUP",
                vec![
                    TestArg::Scalar(num(1.0)),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // HLOOKUP.md: approximate match (range_lookup omitted → TRUE) on sorted
    // data returns the largest key <= lookup_value. Golden probe: keys
    // 1,3,5,7 across the first row.
    #[test]
    fn hlookup_approx_sorted_largest_le() {
        let sorted = || {
            table(
                2,
                4,
                vec![
                    num(1.0),
                    num(3.0),
                    num(5.0),
                    num(7.0),
                    txt("one"),
                    txt("three"),
                    txt("five"),
                    txt("seven"),
                ],
            )
        };
        // lookup 4 → largest <= is 3 → "three".
        assert_eq!(
            call(
                "HLOOKUP",
                vec![
                    TestArg::Scalar(num(4.0)),
                    sorted(),
                    TestArg::Scalar(num(2.0))
                ]
            ),
            txt("three")
        );
        // lookup 100 → largest <= is 7 → "seven".
        assert_eq!(
            call(
                "HLOOKUP",
                vec![
                    TestArg::Scalar(num(100.0)),
                    sorted(),
                    TestArg::Scalar(num(2.0))
                ]
            ),
            txt("seven")
        );
    }

    // HLOOKUP.md: an *exact* key present in approximate mode is found on the
    // boundary.
    #[test]
    fn hlookup_approx_exact_boundary_hit() {
        let t = table(
            2,
            4,
            vec![
                num(1.0),
                num(3.0),
                num(5.0),
                num(7.0),
                txt("one"),
                txt("three"),
                txt("five"),
                txt("seven"),
            ],
        );
        assert_eq!(
            call(
                "HLOOKUP",
                vec![TestArg::Scalar(num(5.0)), t, TestArg::Scalar(num(2.0))]
            ),
            txt("five")
        );
    }

    // HLOOKUP.md: approximate match where lookup_value is smaller than every
    // key → #N/A.
    #[test]
    fn hlookup_approx_smaller_than_all_is_na() {
        let t = table(
            2,
            3,
            vec![
                num(10.0),
                num(20.0),
                num(30.0),
                num(1.0),
                num(2.0),
                num(3.0),
            ],
        );
        assert_eq!(
            call(
                "HLOOKUP",
                vec![TestArg::Scalar(num(5.0)), t, TestArg::Scalar(num(2.0))]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // HLOOKUP.md: row_index_num < 1 → #VALUE!.
    #[test]
    fn hlookup_row_index_below_one_is_value() {
        let t = table(2, 1, vec![num(1.0), num(2.0)]);
        assert_eq!(
            call(
                "HLOOKUP",
                vec![
                    TestArg::Scalar(num(1.0)),
                    t,
                    TestArg::Scalar(num(0.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    // HLOOKUP.md: row_index_num greater than the table height → #REF!.
    #[test]
    fn hlookup_row_index_past_height_is_ref() {
        let t = table(2, 1, vec![num(1.0), num(2.0)]);
        assert_eq!(
            call(
                "HLOOKUP",
                vec![
                    TestArg::Scalar(num(1.0)),
                    t,
                    TestArg::Scalar(num(3.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    // HLOOKUP.md: an error in lookup_value propagates.
    #[test]
    fn hlookup_error_lookup_value_propagates() {
        let t = table(2, 1, vec![num(1.0), num(2.0)]);
        assert_eq!(
            call(
                "HLOOKUP",
                vec![
                    TestArg::Scalar(Value::Error(ErrorKind::Na)),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // HLOOKUP.md: an error sitting in the returned cell of the matched
    // column propagates as HLOOKUP's result.
    #[test]
    fn hlookup_error_in_returned_cell_propagates() {
        let t = table(
            2,
            2,
            vec![txt("a"), txt("b"), Value::Error(ErrorKind::Div0), num(2.0)],
        );
        assert_eq!(
            call(
                "HLOOKUP",
                vec![
                    TestArg::Scalar(txt("a")),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    // An unbounded arg that resolves to **no reference extent** (the `Unbounded`
    // mock provides no `arg_ref_extent`) still defers → #UNSUPPORTED!: RFC 0006's
    // whole-column HLOOKUP needs a resolvable single-area reference (served in the
    // engine via `arg_ref_extent`+`cell_at`; see the xl-engine registry tests).
    #[test]
    fn hlookup_whole_column_table_is_unsupported() {
        assert_eq!(
            call(
                "HLOOKUP",
                vec![
                    TestArg::Scalar(num(1.0)),
                    TestArg::Unbounded(vec![num(1.0), num(2.0)]),
                    TestArg::Scalar(num(1.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // OXP-089: a text lookup_value carrying a wildcard in exact mode defers.
    #[test]
    fn hlookup_exact_wildcard_is_oracle_deferred() {
        let t = table(2, 2, vec![txt("apple"), txt("apricot"), num(1.0), num(2.0)]);
        assert_eq!(
            call(
                "HLOOKUP",
                vec![
                    TestArg::Scalar(txt("ap*")),
                    t,
                    TestArg::Scalar(num(2.0)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // OXP-089: a non-integer row_index_num's truncation direction is
    // oracle-deferred → #UNSUPPORTED!.
    #[test]
    fn hlookup_non_integer_row_index_is_oracle_deferred() {
        let t = table(3, 1, vec![num(1.0), num(2.0), num(3.0)]);
        assert_eq!(
            call(
                "HLOOKUP",
                vec![
                    TestArg::Scalar(num(1.0)),
                    t,
                    TestArg::Scalar(num(2.5)),
                    TestArg::Scalar(Value::Bool(false)),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // HLOOKUP is registered with the documented 3..=4 arity and is
    // non-volatile.
    #[test]
    fn hlookup_registered_with_expected_arity() {
        let spec = lookup("HLOOKUP").expect("HLOOKUP registered");
        assert!(!spec.accepts_arity(2), "min 3 args");
        assert!(spec.accepts_arity(3));
        assert!(spec.accepts_arity(4));
        assert!(!spec.accepts_arity(5), "max 4 args");
        assert!(!is_volatile("HLOOKUP"));
    }

    fn column(vs: Vec<Value>) -> TestArg {
        TestArg::Range(vs)
    }

    fn row(vs: Vec<Value>) -> TestArg {
        TestArg::Array(vs)
    }

    // MATCH.md §3: match_type=0 returns the 1-based POSITION of the first
    // exact match (not the value itself).
    #[test]
    fn match_exact_returns_position() {
        assert_eq!(
            call(
                "MATCH",
                vec![
                    TestArg::Scalar(txt("banana")),
                    column(vec![txt("apple"), txt("banana"), txt("cherry")]),
                    TestArg::Scalar(num(0.0)),
                ]
            ),
            num(2.0)
        );
    }

    // MATCH.md §5: no exact match → #N/A.
    #[test]
    fn match_exact_miss_is_na() {
        assert_eq!(
            call(
                "MATCH",
                vec![
                    TestArg::Scalar(txt("zzz")),
                    column(vec![txt("a"), txt("b")]),
                    TestArg::Scalar(num(0.0)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // MATCH.md §Coercion: exact-mode text comparison is case-insensitive.
    #[test]
    fn match_exact_case_insensitive_text() {
        assert_eq!(
            call(
                "MATCH",
                vec![
                    TestArg::Scalar(txt("BANANA")),
                    column(vec![txt("Apple"), txt("Banana")]),
                    TestArg::Scalar(num(0.0)),
                ]
            ),
            num(2.0)
        );
    }

    // MATCH.md §3: duplicate keys → the first (topmost) match's position.
    #[test]
    fn match_exact_first_of_duplicates() {
        assert_eq!(
            call(
                "MATCH",
                vec![
                    TestArg::Scalar(txt("k")),
                    column(vec![txt("k"), txt("k"), txt("k")]),
                    TestArg::Scalar(num(0.0)),
                ]
            ),
            num(1.0)
        );
    }

    // MATCH.md §2: match_type=1 (default) returns the position of the
    // largest value <= lookup_value on ascending-sorted data.
    #[test]
    fn match_ascending_largest_le() {
        let arr = || column(vec![num(1.0), num(3.0), num(5.0), num(7.0)]);
        assert_eq!(
            call("MATCH", vec![TestArg::Scalar(num(4.0)), arr()]),
            num(2.0) // position of 3
        );
        assert_eq!(
            call("MATCH", vec![TestArg::Scalar(num(100.0)), arr()]),
            num(4.0) // position of 7
        );
    }

    // MATCH.md §2: an exact key on the boundary is found.
    #[test]
    fn match_ascending_exact_boundary_hit() {
        let arr = column(vec![num(1.0), num(3.0), num(5.0), num(7.0)]);
        assert_eq!(
            call("MATCH", vec![TestArg::Scalar(num(5.0)), arr]),
            num(3.0)
        );
    }

    // MATCH.md §2/§5: lookup_value smaller than every key → #N/A.
    #[test]
    fn match_ascending_smaller_than_all_is_na() {
        let arr = column(vec![num(10.0), num(20.0), num(30.0)]);
        assert_eq!(
            call("MATCH", vec![TestArg::Scalar(num(5.0)), arr]),
            Value::Error(ErrorKind::Na)
        );
    }

    // MATCH.md §4: match_type=-1 returns the position of the smallest value
    // >= lookup_value on descending-sorted data.
    #[test]
    fn match_descending_smallest_ge() {
        let arr = || column(vec![num(10.0), num(8.0), num(6.0), num(4.0), num(2.0)]);
        assert_eq!(
            call(
                "MATCH",
                vec![TestArg::Scalar(num(5.0)), arr(), TestArg::Scalar(num(-1.0))]
            ),
            num(3.0) // position of 6, the smallest value >= 5
        );
        assert_eq!(
            call(
                "MATCH",
                vec![
                    TestArg::Scalar(num(10.0)),
                    arr(),
                    TestArg::Scalar(num(-1.0))
                ]
            ),
            num(1.0) // exact boundary hit
        );
    }

    // MATCH.md §4/§5: lookup_value larger than every key (descending mode)
    // → #N/A (no element is >= lookup_value).
    #[test]
    fn match_descending_larger_than_all_is_na() {
        let arr = column(vec![num(10.0), num(8.0), num(6.0)]);
        assert_eq!(
            call(
                "MATCH",
                vec![TestArg::Scalar(num(11.0)), arr, TestArg::Scalar(num(-1.0))]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // MATCH works identically over a row-shaped lookup_array (1×N), not
    // just a column (N×1).
    #[test]
    fn match_works_over_row_array() {
        assert_eq!(
            call(
                "MATCH",
                vec![
                    TestArg::Scalar(txt("c")),
                    row(vec![txt("a"), txt("b"), txt("c")]),
                    TestArg::Scalar(num(0.0)),
                ]
            ),
            num(3.0)
        );
    }

    // RFC 0001: MATCH over a whole-column lookup_array returns the matched
    // cell's position = RELATIVE row + 1 (contiguous data → the natural index).
    #[test]
    fn match_whole_column_used_extent_contiguous() {
        // A:A = [1,3,5,7] at rows 0..3; exact match of 5 → position 3.
        assert_eq!(
            call(
                "MATCH",
                vec![
                    TestArg::Scalar(num(5.0)),
                    TestArg::Unbounded(vec![num(1.0), num(3.0), num(5.0), num(7.0)]),
                    TestArg::Scalar(num(0.0)),
                ]
            ),
            num(3.0)
        );
    }

    // RFC 0001 / OXP-104: with GAPS the returned position is the absolute
    // (relative-to-top) row, not the compacted index. Data at rows 2 and 6.
    #[test]
    fn match_whole_column_used_extent_gaps_position_is_relative_row() {
        let arr = TestArg::UsedRows(vec![(2, vec![num(10.0)]), (6, vec![num(20.0)])]);
        // Exact match of 20 sits at relative row 6 → position 7 (not 2).
        assert_eq!(
            call(
                "MATCH",
                vec![TestArg::Scalar(num(20.0)), arr, TestArg::Scalar(num(0.0))]
            ),
            num(7.0)
        );
    }

    // RFC 0001 / OXP-104 H1 (RUN-2026-07-11-oracle01, updated by L2-A): a Blank
    // lookup_value over a whole column whose populated cells are all confirmed
    // no-matches is pinned to #N/A (`=MATCH(C1,A:A,0)` over {1,2,<blank>,4} →
    // #N/A); the pre-L2-A blanket defer was engine policy, not the oracle
    // answer.
    #[test]
    fn match_whole_column_blank_lookup_no_match_is_na() {
        assert_eq!(
            call(
                "MATCH",
                vec![
                    TestArg::Scalar(Value::Blank),
                    TestArg::Unbounded(vec![num(1.0), num(2.0)]),
                    TestArg::Scalar(num(0.0)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // RFC 0001: a multi-column whole-column range is genuinely 2-D for MATCH.
    #[test]
    fn match_whole_column_multicol_is_2d_unsupported() {
        let arr = TestArg::UsedRows(vec![
            (0, vec![num(1.0), num(9.0)]),
            (1, vec![num(2.0), num(8.0)]),
        ]);
        assert_eq!(
            call(
                "MATCH",
                vec![TestArg::Scalar(num(2.0)), arr, TestArg::Scalar(num(0.0))]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // MATCH.md §Error behavior: an error in lookup_value propagates.
    #[test]
    fn match_error_lookup_value_propagates() {
        assert_eq!(
            call(
                "MATCH",
                vec![
                    TestArg::Scalar(Value::Error(ErrorKind::Na)),
                    column(vec![num(1.0)]),
                    TestArg::Scalar(num(0.0)),
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // Invalid match_type (documented as unconfirmed behavior) is
    // oracle-deferred rather than guessed.
    #[test]
    fn match_invalid_match_type_is_oracle_deferred() {
        assert_eq!(
            call(
                "MATCH",
                vec![
                    TestArg::Scalar(num(1.0)),
                    column(vec![num(1.0), num(2.0)]),
                    TestArg::Scalar(num(2.0)),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // OXP-089: a text lookup_value carrying a wildcard in exact mode defers.
    #[test]
    fn match_exact_wildcard_is_oracle_deferred() {
        assert_eq!(
            call(
                "MATCH",
                vec![
                    TestArg::Scalar(txt("ap*")),
                    column(vec![txt("apple"), txt("apricot")]),
                    TestArg::Scalar(num(0.0)),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // A genuinely 2-D lookup_array (more than one row and more than one
    // column) has no documented flattening order → #UNSUPPORTED!.
    #[test]
    fn match_2d_array_is_oracle_deferred() {
        let t = table(2, 2, vec![num(1.0), num(2.0), num(3.0), num(4.0)]);
        assert_eq!(
            call(
                "MATCH",
                vec![TestArg::Scalar(num(1.0)), t, TestArg::Scalar(num(0.0)),]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // MATCH is registered with the documented 2..=3 arity, default
    // match_type=1, and is non-volatile.
    #[test]
    fn match_registered_with_expected_arity() {
        let spec = lookup("MATCH").expect("MATCH registered");
        assert!(!spec.accepts_arity(1), "min 2 args");
        assert!(spec.accepts_arity(2));
        assert!(spec.accepts_arity(3));
        assert!(!spec.accepts_arity(4), "max 3 args");
        assert!(!is_volatile("MATCH"));
        // Omitted match_type defaults to 1 (ascending largest-<=).
        assert_eq!(
            call(
                "MATCH",
                vec![
                    TestArg::Scalar(num(4.0)),
                    column(vec![num(1.0), num(3.0), num(5.0)]),
                ]
            ),
            num(2.0)
        );
    }

    // INDEX.md §1: both indices nonzero and in bounds returns the single
    // value at that 1-based position.
    #[test]
    fn index_basic_lookup() {
        let t = table(
            2,
            3,
            vec![num(1.0), num(2.0), num(3.0), num(4.0), num(5.0), num(6.0)],
        );
        assert_eq!(
            call(
                "INDEX",
                vec![t, TestArg::Scalar(num(2.0)), TestArg::Scalar(num(3.0))]
            ),
            num(6.0)
        );
    }

    // INDEX.md §2: row_num=0 returns the entire column as an array.
    #[test]
    fn index_row_num_zero_returns_column() {
        let t = table(2, 2, vec![num(1.0), num(2.0), num(3.0), num(4.0)]);
        assert_eq!(
            call(
                "INDEX",
                vec![t, TestArg::Scalar(num(0.0)), TestArg::Scalar(num(2.0))]
            ),
            Value::Array(xl_value::Array::new(2, 1, vec![num(2.0), num(4.0)]).unwrap())
        );
    }

    // INDEX.md §3: col_num=0 returns the entire row as an array.
    #[test]
    fn index_col_num_zero_returns_row() {
        let t = table(2, 2, vec![num(1.0), num(2.0), num(3.0), num(4.0)]);
        assert_eq!(
            call(
                "INDEX",
                vec![t, TestArg::Scalar(num(2.0)), TestArg::Scalar(num(0.0))]
            ),
            Value::Array(xl_value::Array::new(1, 2, vec![num(3.0), num(4.0)]).unwrap())
        );
    }

    // row_num=0 and col_num=0 together is undocumented → #UNSUPPORTED!.
    #[test]
    fn index_both_zero_is_oracle_deferred() {
        let t = table(2, 2, vec![num(1.0), num(2.0), num(3.0), num(4.0)]);
        assert_eq!(
            call(
                "INDEX",
                vec![t, TestArg::Scalar(num(0.0)), TestArg::Scalar(num(0.0))]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // INDEX.md §6: out-of-bounds row_num/col_num → #REF!.
    #[test]
    fn index_out_of_bounds_is_ref() {
        let t = table(2, 2, vec![num(1.0), num(2.0), num(3.0), num(4.0)]);
        assert_eq!(
            call(
                "INDEX",
                vec![t, TestArg::Scalar(num(3.0)), TestArg::Scalar(num(1.0))]
            ),
            Value::Error(ErrorKind::Ref)
        );
        let t2 = table(2, 2, vec![num(1.0), num(2.0), num(3.0), num(4.0)]);
        assert_eq!(
            call(
                "INDEX",
                vec![t2, TestArg::Scalar(num(1.0)), TestArg::Scalar(num(3.0))]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    // A negative row_num/col_num is oracle-deferred (exact error type
    // unconfirmed) → #UNSUPPORTED!.
    #[test]
    fn index_negative_index_is_oracle_deferred() {
        let t = table(2, 2, vec![num(1.0), num(2.0), num(3.0), num(4.0)]);
        assert_eq!(
            call(
                "INDEX",
                vec![t, TestArg::Scalar(num(-1.0)), TestArg::Scalar(num(1.0))]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // INDEX.md §4: a single-column array makes col_num optional (defaults
    // to 1 — ordinary indexing by row_num alone).
    #[test]
    fn index_single_column_col_num_optional() {
        let arr = column(vec![num(10.0), num(20.0), num(30.0)]);
        assert_eq!(
            call("INDEX", vec![arr, TestArg::Scalar(num(2.0))]),
            num(20.0)
        );
    }

    // INDEX.md §4: a single-row array makes col_num optional — the supplied
    // row_num argument is reinterpreted as the column index.
    #[test]
    fn index_single_row_col_num_optional_reinterprets_row_num_as_col() {
        let arr = row(vec![num(10.0), num(20.0), num(30.0)]);
        assert_eq!(
            call("INDEX", vec![arr, TestArg::Scalar(num(3.0))]),
            num(30.0)
        );
    }

    // A genuinely 2-D array with col_num omitted has no documented
    // resolution → #UNSUPPORTED!.
    #[test]
    fn index_2d_array_col_num_omitted_is_oracle_deferred() {
        let t = table(2, 2, vec![num(1.0), num(2.0), num(3.0), num(4.0)]);
        assert_eq!(
            call("INDEX", vec![t, TestArg::Scalar(num(1.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // INDEX.md §Error behavior: an error value stored in the indexed cell
    // is returned as-is, not "caught".
    #[test]
    fn index_error_in_cell_returned_as_is() {
        let t = table(1, 2, vec![num(1.0), Value::Error(ErrorKind::Div0)]);
        assert_eq!(
            call(
                "INDEX",
                vec![t, TestArg::Scalar(num(1.0)), TestArg::Scalar(num(2.0))]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    // An unbounded `array` that resolves to **no reference extent** (the
    // `Unbounded` mock provides no `arg_ref_extent`) still defers → #UNSUPPORTED!.
    // RFC 0006's whole-column INDEX needs a resolvable single-area reference, then
    // reads by absolute position (served in the engine via `arg_ref_extent`+
    // `cell_at`; see the xl-engine `registry_index_whole_column_absolute_read`).
    #[test]
    fn index_whole_column_array_is_unsupported() {
        assert_eq!(
            call(
                "INDEX",
                vec![
                    TestArg::Unbounded(vec![num(1.0), num(2.0)]),
                    TestArg::Scalar(num(1.0)),
                    TestArg::Scalar(num(1.0)),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // INDEX is registered with the documented 2..=3 arity and is
    // non-volatile.
    #[test]
    fn index_registered_with_expected_arity() {
        let spec = lookup("INDEX").expect("INDEX registered");
        assert!(!spec.accepts_arity(1), "min 2 args");
        assert!(spec.accepts_arity(2));
        assert!(spec.accepts_arity(3));
        assert!(!spec.accepts_arity(4), "max 3 args");
        assert!(!is_volatile("INDEX"));
    }

    // AND.md §1/§Coercion: scalar args coerce via to_bool (number, "TRUE"
    // text); all-true -> TRUE.
    #[test]
    fn and_scalar_all_true_coercion() {
        assert_eq!(
            call(
                "AND",
                vec![
                    TestArg::Scalar(Value::Bool(true)),
                    TestArg::Scalar(num(7.0)),
                    TestArg::Scalar(txt("true")),
                ]
            ),
            Value::Bool(true)
        );
    }

    // AND.md §1: any falsy scalar (0, "FALSE") makes the whole call FALSE.
    #[test]
    fn and_scalar_any_false_is_false() {
        assert_eq!(
            call(
                "AND",
                vec![
                    TestArg::Scalar(Value::Bool(true)),
                    TestArg::Scalar(num(0.0)),
                    TestArg::Scalar(txt("FALSE")),
                ]
            ),
            Value::Bool(false)
        );
    }

    // OXP-208 (RUN-2026-07-16-oracle01) + OXP-184: a Number cell in a range
    // PARTICIPATES in AND (0 → FALSE, nonzero → TRUE); Text and Blank stay
    // ignored. Pre-OXP-208 this file asserted numbers were ignored — the farm
    // run overturned that reading.
    #[test]
    fn and_range_number_participates_text_ignored() {
        // The 0-number participates (→ FALSE), so AND is FALSE despite the two
        // TRUE bools. Pre-OXP-208 the 0 was ignored and this was TRUE.
        let range = vec![
            Value::Bool(true),
            num(0.0),
            txt("FALSE"),
            Value::Blank,
            Value::Bool(true),
        ];
        assert_eq!(call("AND", vec![TestArg::Range(range)]), Value::Bool(false));
        // Text is still ignored: {TRUE, "FALSE"-text} → TRUE (had the text
        // coerced, the FALSE would flip it).
        let range2 = vec![Value::Bool(true), txt("FALSE")];
        assert_eq!(call("AND", vec![TestArg::Range(range2)]), Value::Bool(true));
    }

    // A Bool(false) cell inside a range still makes AND false, alongside a
    // now-participating number (99 → TRUE) and an ignored... nothing here.
    #[test]
    fn and_range_false_bool_cell_wins() {
        let range = vec![Value::Bool(true), num(99.0), Value::Bool(false)];
        assert_eq!(call("AND", vec![TestArg::Range(range)]), Value::Bool(false));
    }

    // AND.md §3/§Error behavior + OXP-208 H6: zero contributing values ->
    // #VALUE! (a range holding ONLY text/blank — no Bool and, post-OXP-208, no
    // Number cells, since a Number would now participate).
    #[test]
    fn and_zero_logical_values_is_value_error() {
        let range = vec![txt("x"), Value::Blank];
        assert_eq!(
            call("AND", vec![TestArg::Range(range)]),
            Value::Error(ErrorKind::Value)
        );
    }

    // AND.md §Error behavior: an error (scalar or in a range) propagates.
    #[test]
    fn and_error_propagates() {
        assert_eq!(
            call(
                "AND",
                vec![
                    TestArg::Scalar(Value::Bool(true)),
                    TestArg::Scalar(Value::Error(ErrorKind::Div0)),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
        let range = vec![Value::Bool(true), Value::Error(ErrorKind::Na)];
        assert_eq!(
            call("AND", vec![TestArg::Range(range)]),
            Value::Error(ErrorKind::Na)
        );
    }

    // AND.md §Coercion: non-coercible scalar text -> #VALUE!.
    #[test]
    fn and_scalar_non_coercible_text_is_value_error() {
        assert_eq!(
            call("AND", vec![TestArg::Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // OXP-094 RESOLVED (RUN-2026-07-11-oracle01): a scalar Blank argument (or an
    // elided argument slot) is EXCLUDED from the reduction, like a range-blank
    // cell — =AND(A1,TRUE) with A1 empty → TRUE.
    #[test]
    fn and_scalar_blank_is_excluded() {
        assert_eq!(
            call(
                "AND",
                vec![
                    TestArg::Scalar(Value::Blank),
                    TestArg::Scalar(Value::Bool(true))
                ]
            ),
            Value::Bool(true)
        );
        // An omitted slot is also Blank, so it too is excluded (the TRUE still
        // satisfies the "at least one logical value" rule).
        assert_eq!(
            call(
                "AND",
                vec![TestArg::Scalar(Value::Bool(true)), TestArg::Omitted]
            ),
            Value::Bool(true)
        );
    }

    // OR.md §1: mirror of AND — any truthy scalar makes the whole call TRUE.
    #[test]
    fn or_scalar_any_true_is_true() {
        assert_eq!(
            call(
                "OR",
                vec![
                    TestArg::Scalar(Value::Bool(false)),
                    TestArg::Scalar(num(0.0)),
                    TestArg::Scalar(txt("TRUE")),
                ]
            ),
            Value::Bool(true)
        );
    }

    // OR.md §1: every argument falsy -> FALSE.
    #[test]
    fn or_scalar_all_false_is_false() {
        assert_eq!(
            call(
                "OR",
                vec![
                    TestArg::Scalar(Value::Bool(false)),
                    TestArg::Scalar(num(0.0)),
                    TestArg::Scalar(txt("false")),
                ]
            ),
            Value::Bool(false)
        );
    }

    // OXP-208 (RUN-2026-07-16-oracle01) + OXP-174: a Number cell in a range
    // PARTICIPATES in OR (0 → FALSE, nonzero → TRUE). Text/blank stay ignored.
    // Pre-OXP-208 this file asserted a numeric cell "never flips OR to TRUE".
    #[test]
    fn or_range_number_participates_text_ignored() {
        // The number 1 (truthy) participates → OR is TRUE, even though the only
        // Bool present is FALSE. Pre-OXP-208 this asserted FALSE.
        let range = vec![num(1.0), txt("TRUE"), Value::Blank, Value::Bool(false)];
        assert_eq!(call("OR", vec![TestArg::Range(range)]), Value::Bool(true));
        // A 0-number participates as FALSE; the "TRUE" TEXT is still ignored, so
        // {0, "TRUE", FALSE} → FALSE (had the text coerced, it would be TRUE).
        let range2 = vec![num(0.0), txt("TRUE"), Value::Bool(false)];
        assert_eq!(call("OR", vec![TestArg::Range(range2)]), Value::Bool(false));
    }

    // OR.md §Error behavior + OXP-208: zero contributing values (only text/
    // blank; no Bool and, post-OXP-208, no Number) anywhere -> #VALUE!.
    #[test]
    fn or_zero_logical_values_is_value_error() {
        let range = vec![txt("x"), Value::Blank];
        assert_eq!(
            call("OR", vec![TestArg::Range(range)]),
            Value::Error(ErrorKind::Value)
        );
    }

    // OR.md §Error behavior: an error (scalar or in a range) propagates.
    #[test]
    fn or_error_propagates() {
        assert_eq!(
            call(
                "OR",
                vec![
                    TestArg::Scalar(Value::Bool(false)),
                    TestArg::Scalar(Value::Error(ErrorKind::Ref)),
                ]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    // OXP-094 RESOLVED (shared with AND/NOT): a scalar Blank argument is
    // EXCLUDED from the reduction — =OR(A1,FALSE) with A1 empty → FALSE.
    #[test]
    fn or_scalar_blank_is_excluded() {
        assert_eq!(
            call(
                "OR",
                vec![
                    TestArg::Scalar(Value::Blank),
                    TestArg::Scalar(Value::Bool(false))
                ]
            ),
            Value::Bool(false)
        );
    }

    // NOT.md §1: plain negation, both directions.
    #[test]
    fn not_negates_bool() {
        assert_eq!(
            call("NOT", vec![TestArg::Scalar(Value::Bool(true))]),
            Value::Bool(false)
        );
        assert_eq!(
            call("NOT", vec![TestArg::Scalar(Value::Bool(false))]),
            Value::Bool(true)
        );
    }

    // NOT.md §Coercion: number and "TRUE"/"FALSE" text coerce like AND/OR's
    // scalar rule.
    #[test]
    fn not_coerces_number_and_text() {
        assert_eq!(
            call("NOT", vec![TestArg::Scalar(num(0.0))]),
            Value::Bool(true)
        );
        assert_eq!(
            call("NOT", vec![TestArg::Scalar(num(5.0))]),
            Value::Bool(false)
        );
        assert_eq!(
            call("NOT", vec![TestArg::Scalar(txt("false"))]),
            Value::Bool(true)
        );
    }

    // NOT.md §Error behavior: non-coercible text -> #VALUE!; an error
    // argument propagates.
    #[test]
    fn not_error_and_bad_text() {
        assert_eq!(
            call("NOT", vec![TestArg::Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
        assert_eq!(
            call("NOT", vec![TestArg::Scalar(Value::Error(ErrorKind::Num))]),
            Value::Error(ErrorKind::Num)
        );
    }

    // OXP-094 RESOLVED (shared with AND/OR): NOT's single Blank argument coerces
    // through to_bool (Blank → FALSE), so =NOT(A1) with A1 empty → TRUE.
    #[test]
    fn not_blank_coerces_to_false() {
        assert_eq!(
            call("NOT", vec![TestArg::Scalar(Value::Blank)]),
            Value::Bool(true)
        );
    }

    // ISNUMBER.md §1/§2/§3: TRUE only for a real Number; FALSE for
    // numeric-looking text, booleans, blank.
    #[test]
    fn isnumber_true_only_for_number() {
        assert_eq!(
            call("ISNUMBER", vec![TestArg::Scalar(num(5.0))]),
            Value::Bool(true)
        );
        assert_eq!(
            call("ISNUMBER", vec![TestArg::Scalar(txt("5"))]),
            Value::Bool(false)
        );
        assert_eq!(
            call("ISNUMBER", vec![TestArg::Scalar(Value::Bool(true))]),
            Value::Bool(false)
        );
        assert_eq!(
            call("ISNUMBER", vec![TestArg::Scalar(Value::Blank)]),
            Value::Bool(false)
        );
    }

    // ISNUMBER.md §4/§Error behavior: an error argument -> FALSE, never
    // propagated.
    #[test]
    fn isnumber_error_argument_is_false_not_propagated() {
        assert_eq!(
            call(
                "ISNUMBER",
                vec![TestArg::Scalar(Value::Error(ErrorKind::Div0))]
            ),
            Value::Bool(false)
        );
    }

    // ISERROR.md §1: TRUE for every documented Excel error kind.
    #[test]
    fn iserror_true_for_every_excel_error_kind() {
        for kind in [
            ErrorKind::Null,
            ErrorKind::Div0,
            ErrorKind::Value,
            ErrorKind::Ref,
            ErrorKind::Name,
            ErrorKind::Num,
            ErrorKind::Na,
            ErrorKind::GettingData,
        ] {
            assert_eq!(
                call("ISERROR", vec![TestArg::Scalar(Value::Error(kind))]),
                Value::Bool(true),
                "{kind:?} should be caught by ISERROR"
            );
        }
    }

    // Recalc sentinels are Recalc's own admission that `value` was never
    // computed, not a genuine error value ISERROR can answer a predicate
    // about — propagated unchanged rather than guessed TRUE (see
    // func_iserror.rs's "Recalc sentinels are propagated, not answered").
    #[test]
    fn iserror_propagates_recalc_specific_error_kinds() {
        for kind in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(
                call("ISERROR", vec![TestArg::Scalar(Value::Error(kind))]),
                Value::Error(kind)
            );
        }
    }

    // ISERROR.md §3: FALSE for every non-error kind. This also proves
    // containment: ISERROR never re-throws.
    #[test]
    fn iserror_false_for_non_error_values() {
        assert_eq!(
            call("ISERROR", vec![TestArg::Scalar(num(1.0))]),
            Value::Bool(false)
        );
        assert_eq!(
            call("ISERROR", vec![TestArg::Scalar(txt("x"))]),
            Value::Bool(false)
        );
        assert_eq!(
            call("ISERROR", vec![TestArg::Scalar(Value::Bool(false))]),
            Value::Bool(false)
        );
        assert_eq!(
            call("ISERROR", vec![TestArg::Scalar(Value::Blank)]),
            Value::Bool(false)
        );
    }

    // ISBLANK.md §1: TRUE only for a genuinely Blank value.
    #[test]
    fn isblank_true_for_blank() {
        assert_eq!(
            call("ISBLANK", vec![TestArg::Scalar(Value::Blank)]),
            Value::Bool(true)
        );
    }

    // ISBLANK.md §2 — the explicit hit-list item: ISBLANK("") = FALSE,
    // distinct from a truly empty cell.
    #[test]
    fn isblank_empty_string_is_false() {
        assert_eq!(
            call("ISBLANK", vec![TestArg::Scalar(txt(""))]),
            Value::Bool(false)
        );
    }

    // ISBLANK.md §3: FALSE for 0/FALSE/other non-Blank values.
    #[test]
    fn isblank_false_for_zero_and_false() {
        assert_eq!(
            call("ISBLANK", vec![TestArg::Scalar(num(0.0))]),
            Value::Bool(false)
        );
        assert_eq!(
            call("ISBLANK", vec![TestArg::Scalar(Value::Bool(false))]),
            Value::Bool(false)
        );
    }

    // ISBLANK.md §4/§Error behavior: an error argument -> FALSE, not
    // propagated.
    #[test]
    fn isblank_error_argument_is_false_not_propagated() {
        assert_eq!(
            call(
                "ISBLANK",
                vec![TestArg::Scalar(Value::Error(ErrorKind::Ref))]
            ),
            Value::Bool(false)
        );
    }

    // ISTEXT.md §1/§3: TRUE for text, including numeric-looking text and
    // "" — the direct complement of ISBLANK("")=FALSE.
    #[test]
    fn istext_true_for_text_including_empty_string() {
        assert_eq!(
            call("ISTEXT", vec![TestArg::Scalar(txt("hi"))]),
            Value::Bool(true)
        );
        assert_eq!(
            call("ISTEXT", vec![TestArg::Scalar(txt("5"))]),
            Value::Bool(true)
        );
        assert_eq!(
            call("ISTEXT", vec![TestArg::Scalar(txt(""))]),
            Value::Bool(true)
        );
    }

    // ISTEXT.md §2: FALSE for numbers, booleans, blank; error argument ->
    // FALSE, not propagated.
    #[test]
    fn istext_false_for_non_text() {
        assert_eq!(
            call("ISTEXT", vec![TestArg::Scalar(num(1.0))]),
            Value::Bool(false)
        );
        assert_eq!(
            call("ISTEXT", vec![TestArg::Scalar(Value::Bool(true))]),
            Value::Bool(false)
        );
        assert_eq!(
            call("ISTEXT", vec![TestArg::Scalar(Value::Blank)]),
            Value::Bool(false)
        );
        assert_eq!(
            call("ISTEXT", vec![TestArg::Scalar(Value::Error(ErrorKind::Na))]),
            Value::Bool(false)
        );
    }

    // Clean-room ISNA: TRUE only for #N/A.
    #[test]
    fn isna_true_only_for_na() {
        assert_eq!(
            call("ISNA", vec![TestArg::Scalar(Value::Error(ErrorKind::Na))]),
            Value::Bool(true)
        );
    }

    // Every other genuine-Excel-error kind, and every non-error value, is
    // FALSE — and none of it propagates.
    #[test]
    fn isna_false_for_other_errors_and_values() {
        assert_eq!(
            call("ISNA", vec![TestArg::Scalar(Value::Error(ErrorKind::Div0))]),
            Value::Bool(false)
        );
        assert_eq!(
            call("ISNA", vec![TestArg::Scalar(num(1.0))]),
            Value::Bool(false)
        );
        assert_eq!(
            call("ISNA", vec![TestArg::Scalar(Value::Blank)]),
            Value::Bool(false)
        );
    }

    // A Recalc sentinel is not a genuine error ISNA can answer #N/A-or-not
    // about — propagated unchanged rather than guessed FALSE (see
    // func_isna.rs's "Recalc sentinels ... are propagated").
    #[test]
    fn isna_propagates_recalc_specific_error_kinds() {
        for kind in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(
                call("ISNA", vec![TestArg::Scalar(Value::Error(kind))]),
                Value::Error(kind)
            );
        }
    }

    // AND/OR accept 1..=255 args; NOT/IS-functions accept exactly 1; all
    // are non-volatile.
    #[test]
    fn logical_and_is_family_registered_with_expected_arity() {
        for name in ["AND", "OR"] {
            let spec = lookup(name).unwrap_or_else(|| panic!("{name} registered"));
            assert!(!spec.accepts_arity(0), "{name} requires at least 1 arg");
            assert!(spec.accepts_arity(1));
            assert!(spec.accepts_arity(255));
            assert!(!spec.accepts_arity(256));
            assert!(!is_volatile(name), "{name} is non-volatile");
        }
        for name in ["NOT", "ISNUMBER", "ISERROR", "ISBLANK", "ISTEXT", "ISNA"] {
            let spec = lookup(name).unwrap_or_else(|| panic!("{name} registered"));
            assert!(!spec.accepts_arity(0), "{name} requires exactly 1 arg");
            assert!(spec.accepts_arity(1));
            assert!(!spec.accepts_arity(2), "{name} rejects a 2nd arg");
            assert!(!is_volatile(name), "{name} is non-volatile");
        }
    }

    // ABS.md §1/§Coercion: magnitude, with the usual scalar coercions.
    #[test]
    fn abs_basic_and_coercion() {
        assert_eq!(call("ABS", vec![TestArg::Scalar(num(-5.0))]), num(5.0));
        assert_eq!(call("ABS", vec![TestArg::Scalar(num(5.0))]), num(5.0));
        assert_eq!(
            call("ABS", vec![TestArg::Scalar(Value::Bool(true))]),
            num(1.0)
        );
        assert_eq!(call("ABS", vec![TestArg::Scalar(txt("-3.5"))]), num(3.5));
    }

    // ABS.md §Error behavior: non-numeric text -> #VALUE!; error propagates.
    #[test]
    fn abs_error_behavior() {
        assert_eq!(
            call("ABS", vec![TestArg::Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
        assert_eq!(
            call("ABS", vec![TestArg::Scalar(Value::Error(ErrorKind::Na))]),
            Value::Error(ErrorKind::Na)
        );
    }

    // INT.md §1: the headline distinction — floors toward -inf, not
    // truncation toward zero.
    #[test]
    fn int_floors_toward_negative_infinity() {
        assert_eq!(call("INT", vec![TestArg::Scalar(num(-8.9))]), num(-9.0));
        assert_eq!(call("INT", vec![TestArg::Scalar(num(8.9))]), num(8.0));
        assert_eq!(call("INT", vec![TestArg::Scalar(num(-1.0))]), num(-1.0));
    }

    #[test]
    fn int_error_behavior() {
        assert_eq!(
            call("INT", vec![TestArg::Scalar(txt("abc"))]),
            Value::Error(ErrorKind::Value)
        );
    }

    // MOD.md §2/§Hit-list: the result takes the sign of the divisor, not the
    // dividend, in every sign combination.
    #[test]
    fn mod_sign_follows_divisor() {
        assert_eq!(
            call(
                "MOD",
                vec![TestArg::Scalar(num(-3.0)), TestArg::Scalar(num(2.0))]
            ),
            num(1.0)
        );
        assert_eq!(
            call(
                "MOD",
                vec![TestArg::Scalar(num(3.0)), TestArg::Scalar(num(-2.0))]
            ),
            num(-1.0)
        );
        assert_eq!(
            call(
                "MOD",
                vec![TestArg::Scalar(num(3.0)), TestArg::Scalar(num(2.0))]
            ),
            num(1.0)
        );
        assert_eq!(
            call(
                "MOD",
                vec![TestArg::Scalar(num(-3.0)), TestArg::Scalar(num(-2.0))]
            ),
            num(-1.0)
        );
    }

    // MOD.md §3: divisor 0 -> #DIV/0!, an explicit documented case.
    #[test]
    fn mod_divisor_zero_is_div0() {
        assert_eq!(
            call(
                "MOD",
                vec![TestArg::Scalar(num(5.0)), TestArg::Scalar(num(0.0))]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn mod_error_behavior() {
        assert_eq!(
            call(
                "MOD",
                vec![
                    TestArg::Scalar(Value::Error(ErrorKind::Na)),
                    TestArg::Poison
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
        assert_eq!(
            call(
                "MOD",
                vec![TestArg::Scalar(txt("abc")), TestArg::Scalar(num(2.0))]
            ),
            Value::Error(ErrorKind::Value)
        );
    }

    // CONCATENATE.md §1/§Coercion: joins text representations, numbers via
    // General formatting, booleans as TRUE/FALSE, blank as "".
    #[test]
    fn concatenate_coercion_and_join() {
        assert_eq!(
            call(
                "CONCATENATE",
                vec![
                    TestArg::Scalar(txt("a")),
                    TestArg::Scalar(num(1.0)),
                    TestArg::Scalar(Value::Bool(true)),
                    TestArg::Scalar(Value::Blank),
                    TestArg::Scalar(txt("z")),
                ]
            ),
            txt("a1TRUEz")
        );
    }

    // An omitted argument slot evaluates to Blank -> "" (same as an explicit
    // blank), so it contributes no characters.
    #[test]
    fn concatenate_omitted_contributes_empty_string() {
        assert_eq!(
            call(
                "CONCATENATE",
                vec![TestArg::Scalar(txt("x")), TestArg::Omitted]
            ),
            txt("x")
        );
    }

    // CONCATENATE.md §Error behavior: any erroring argument propagates.
    #[test]
    fn concatenate_error_propagates() {
        assert_eq!(
            call(
                "CONCATENATE",
                vec![
                    TestArg::Scalar(txt("a")),
                    TestArg::Scalar(Value::Error(ErrorKind::Ref)),
                    TestArg::Poison,
                ]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    // A genuine multi-cell range argument in CONCATENATE's scalar context is
    // #UNSUPPORTED! (CONCATENATE.md §2's flagged, unconfirmed cell-selection
    // question) rather than this module guessing a cell.
    #[test]
    fn concatenate_multi_cell_range_argument_is_unsupported() {
        assert_eq!(
            call(
                "CONCATENATE",
                vec![TestArg::Range(vec![txt("a"), txt("b")])]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // ROUND.md §5: half away from zero, at zero and negative digits.
    #[test]
    fn round_half_away_from_zero() {
        assert_eq!(
            call(
                "ROUND",
                vec![TestArg::Scalar(num(2.5)), TestArg::Scalar(num(0.0))]
            ),
            num(3.0)
        );
        assert_eq!(
            call(
                "ROUND",
                vec![TestArg::Scalar(num(-2.5)), TestArg::Scalar(num(0.0))]
            ),
            num(-3.0)
        );
    }

    // ROUND.md §2/§3: ordinary positive-digit rounding.
    #[test]
    fn round_ordinary_digits() {
        assert_eq!(
            call(
                "ROUND",
                vec![TestArg::Scalar(num(12.3456)), TestArg::Scalar(num(2.0))]
            ),
            num(12.35)
        );
    }

    // ROUND.md §4: negative num_digits rounds left of the decimal point.
    #[test]
    fn round_negative_digits() {
        // MS's own canonical negative-digits example: 626.3 -> nearest
        // thousand -> 1000.
        assert_eq!(
            call(
                "ROUND",
                vec![TestArg::Scalar(num(626.3)), TestArg::Scalar(num(-3.0))]
            ),
            num(1000.0)
        );
        assert_eq!(
            call(
                "ROUND",
                vec![TestArg::Scalar(num(21.5)), TestArg::Scalar(num(-1.0))]
            ),
            num(20.0)
        );
    }

    // OXP-095 (RESOLVED, RUN-2026-07-11-oracle01): the binary/decimal
    // half-way ambiguity — Excel rounds the displayed decimal literal away
    // from zero, so `ROUND(1.005,2)` = `1.01` (not the naive IEEE-754 `1.00`).
    #[test]
    fn round_binary_decimal_halfway_ambiguity_rounds_decimal_literal() {
        assert_eq!(
            call(
                "ROUND",
                vec![TestArg::Scalar(num(1.005)), TestArg::Scalar(num(2.0))]
            ),
            num(1.01)
        );
    }

    // OXP-098 (RESOLVED, RUN-2026-07-11-oracle01): a non-integer num_digits is
    // truncated toward zero, so `ROUND(12.3456,2.9)` truncates `2.9` -> `2`
    // and yields `12.35`.
    #[test]
    fn round_non_integer_digits_truncates_toward_zero() {
        assert_eq!(
            call(
                "ROUND",
                vec![TestArg::Scalar(num(12.3456)), TestArg::Scalar(num(2.9))]
            ),
            num(12.35)
        );
    }

    // ROUND.md §Coercion/§Error behavior.
    #[test]
    fn round_error_behavior() {
        assert_eq!(
            call(
                "ROUND",
                vec![TestArg::Scalar(txt("abc")), TestArg::Scalar(num(2.0))]
            ),
            Value::Error(ErrorKind::Value)
        );
        assert_eq!(
            call(
                "ROUND",
                vec![
                    TestArg::Scalar(Value::Error(ErrorKind::Na)),
                    TestArg::Poison
                ]
            ),
            Value::Error(ErrorKind::Na)
        );
    }

    // Default return_type (omitted arg): 1=Sunday..7=Saturday. Serial 1
    // (1900-01-01) anchors Sunday; serial 43831 (2020-01-01, a real-world
    // Wednesday, the same fixture DATE/YEAR/MONTH/DAY's own tests use)
    // cross-checks the anchor independently.
    #[test]
    fn weekday_default_return_type_and_known_date() {
        assert_eq!(call("WEEKDAY", vec![TestArg::Scalar(num(1.0))]), num(1.0));
        assert_eq!(
            call("WEEKDAY", vec![TestArg::Scalar(num(43831.0))]),
            num(4.0)
        );
    }

    // return_type 2 (Monday=1..Sunday=7) and 3 (Monday=0..Sunday=6) on the
    // same known Wednesday.
    #[test]
    fn weekday_return_types_2_and_3() {
        assert_eq!(
            call(
                "WEEKDAY",
                vec![TestArg::Scalar(num(43831.0)), TestArg::Scalar(num(2.0))]
            ),
            num(3.0)
        );
        assert_eq!(
            call(
                "WEEKDAY",
                vec![TestArg::Scalar(num(43831.0)), TestArg::Scalar(num(3.0))]
            ),
            num(2.0)
        );
    }

    // A documented-by-Microsoft but out-of-scope return_type (11..=17) is
    // #UNSUPPORTED!, not a fabricated #NUM!.
    #[test]
    fn weekday_documented_unimplemented_return_type_is_unsupported() {
        assert_eq!(
            call(
                "WEEKDAY",
                vec![TestArg::Scalar(num(43831.0)), TestArg::Scalar(num(11.0))]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // A genuinely invalid return_type is a domain error -> #NUM!.
    #[test]
    fn weekday_invalid_return_type_is_num() {
        assert_eq!(
            call(
                "WEEKDAY",
                vec![TestArg::Scalar(num(43831.0)), TestArg::Scalar(num(99.0))]
            ),
            Value::Error(ErrorKind::Num)
        );
        assert_eq!(
            call(
                "WEEKDAY",
                vec![TestArg::Scalar(num(43831.0)), TestArg::Scalar(num(0.0))]
            ),
            Value::Error(ErrorKind::Num)
        );
    }

    // Omitted vs. present-but-blank return_type: omitted defaults to 1;
    // present-but-blank goes through ordinary scalar coercion (Blank -> 0),
    // which is not a valid return_type -> #NUM!. These must differ.
    #[test]
    fn weekday_omitted_vs_blank_return_type_differ() {
        assert_eq!(
            call("WEEKDAY", vec![TestArg::Scalar(num(1.0))]),
            num(1.0),
            "omitted defaults to return_type 1"
        );
        assert_eq!(
            call(
                "WEEKDAY",
                vec![TestArg::Scalar(num(1.0)), TestArg::Scalar(Value::Blank)]
            ),
            Value::Error(ErrorKind::Num),
            "present-but-blank coerces to 0, not the omitted default"
        );
    }

    // OXP-097 (RUN-2026-07-11-oracle01): a non-integer return_type TRUNCATES
    // toward zero. =WEEKDAY(DATE(2020,1,1),1.9) -> 4 (type → 1);
    // =WEEKDAY(DATE(2020,1,1),2.1) -> 3 (type → 2).
    #[test]
    fn weekday_non_integer_return_type_truncates() {
        assert_eq!(
            call(
                "WEEKDAY",
                vec![TestArg::Scalar(num(43831.0)), TestArg::Scalar(num(1.9))]
            ),
            num(4.0)
        );
        assert_eq!(
            call(
                "WEEKDAY",
                vec![TestArg::Scalar(num(43831.0)), TestArg::Scalar(num(2.1))]
            ),
            num(3.0)
        );
    }

    // Serial domain rules mirror YEAR/MONTH/DAY: negative -> #NUM!; serial 0
    // -> #UNSUPPORTED! (OXP-090).
    #[test]
    fn weekday_serial_domain_matches_date_family() {
        assert_eq!(
            call("WEEKDAY", vec![TestArg::Scalar(num(-1.0))]),
            Value::Error(ErrorKind::Num)
        );
        assert_eq!(
            call("WEEKDAY", vec![TestArg::Scalar(num(0.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // Text/math + WEEKDAY functions are all registered with the expected
    // arity and are non-volatile.
    #[test]
    fn text_math_and_weekday_registered_with_expected_arity() {
        for (name, min, max) in [
            ("CONCATENATE", 1, 255),
            ("ROUND", 2, 2),
            ("ABS", 1, 1),
            ("INT", 1, 1),
            ("MOD", 2, 2),
            ("WEEKDAY", 1, 2),
        ] {
            let spec = lookup(name).unwrap_or_else(|| panic!("{name} registered"));
            if min > 0 {
                assert!(!spec.accepts_arity(min - 1), "{name} min arity");
            }
            assert!(spec.accepts_arity(min));
            assert!(spec.accepts_arity(max));
            assert!(!spec.accepts_arity(max + 1), "{name} max arity");
            assert!(!is_volatile(name), "{name} is non-volatile");
        }
    }

    // COUNTIF.md §1: numeric comparison criteria count matching cells.
    #[test]
    fn countif_numeric_comparison() {
        let range = vec![num(1.0), num(5.0), num(6.0), num(10.0)];
        assert_eq!(
            call(
                "COUNTIF",
                vec![TestArg::Range(range), TestArg::Scalar(txt(">5"))]
            ),
            num(2.0)
        );
    }

    // COUNTIF.md §1: bare-text literal equality is case-insensitive; a matching
    // cell of any type counts once.
    #[test]
    fn countif_literal_case_insensitive() {
        let range = vec![txt("Apple"), txt("apple"), txt("banana"), num(1.0)];
        assert_eq!(
            call(
                "COUNTIF",
                vec![TestArg::Range(range), TestArg::Scalar(txt("APPLE"))]
            ),
            num(2.0)
        );
    }

    // COUNTIF.md §1: wildcard criteria (`*`) match text cells.
    #[test]
    fn countif_wildcard() {
        let range = vec![txt("apple"), txt("apricot"), txt("banana"), txt("avocado")];
        assert_eq!(
            call(
                "COUNTIF",
                vec![TestArg::Range(range), TestArg::Scalar(txt("a*"))]
            ),
            num(3.0)
        );
    }

    // COUNTIF.md §3: the `""` criterion counts blank cells (surfaced
    // positionally by `for_each_row`); a Rect with holes exercises this.
    #[test]
    fn countif_empty_criteria_counts_blanks() {
        let rect = TestArg::Rect {
            rows: 2,
            cols: 2,
            data: vec![num(1.0), Value::Blank, Value::Blank, txt("x")],
        };
        assert_eq!(
            call("COUNTIF", vec![rect, TestArg::Scalar(txt(""))]),
            num(2.0)
        );
    }

    // Error criteria propagates (COUNTIF.md §Error behavior).
    #[test]
    fn countif_error_criteria_propagates() {
        let range = vec![num(1.0)];
        assert_eq!(
            call(
                "COUNTIF",
                vec![
                    TestArg::Range(range),
                    TestArg::Scalar(Value::Error(ErrorKind::Ref))
                ]
            ),
            Value::Error(ErrorKind::Ref)
        );
    }

    // OXP-101/162 (RUN-2026-07-11-oracle01): a **full-date** ordering operand
    // now PARSES to its serial (43831) and counts numerically — the date-text
    // coercion (OXP-160) and currency (OXP-010) that OXP-101 needed are resolved.
    // `=COUNTIF(C1:C5,">1/1/2020")` = 3 is now reproducible; here a small range
    // demonstrates the parse-and-compare.
    #[test]
    fn countif_date_ordering_now_counts() {
        let range = vec![num(43830.0), num(43831.0), num(43832.0)];
        assert_eq!(
            call(
                "COUNTIF",
                vec![TestArg::Range(range), TestArg::Scalar(txt(">1/1/2020"))]
            ),
            num(1.0) // only 43832 is > 43831
        );
    }

    // OXP-101 (RUN-2026-07-11-oracle01): a text ordering operand is a
    // case-insensitive lexicographic comparison. Models the run's
    // `=COUNTIF(A1:A5,">apple")` = 4 (apple excluded as equal).
    #[test]
    fn countif_text_ordering_operand() {
        let range = vec![
            txt("apple"),
            txt("banana"),
            txt("cherry"),
            txt("avocado"),
            txt("blueberry"),
        ];
        assert_eq!(
            call(
                "COUNTIF",
                vec![TestArg::Range(range), TestArg::Scalar(txt(">apple"))]
            ),
            num(4.0)
        );
    }

    // OXP-102 (RUN-2026-07-11-oracle01): a blank criteria *value* matches
    // numeric-0 cells. Models `=COUNTIF(A1:A5,C1)` (C1 empty) = 3.
    #[test]
    fn countif_blank_criteria_value_matches_zero() {
        let range = vec![num(0.0), num(5.0), num(0.0), num(10.0), num(0.0)];
        assert_eq!(
            call(
                "COUNTIF",
                vec![TestArg::Range(range), TestArg::Scalar(Value::Blank)]
            ),
            num(3.0)
        );
    }

    // RFC 0001: a whole-column range is counted over its populated cells.
    #[test]
    fn countif_whole_column_used_extent() {
        // [6,7,3,8], ">5" matches 6,7,8 → 3.
        assert_eq!(
            call(
                "COUNTIF",
                vec![
                    TestArg::Unbounded(vec![num(6.0), num(7.0), num(3.0), num(8.0)]),
                    TestArg::Scalar(txt(">5"))
                ]
            ),
            num(3.0)
        );
    }

    // RFC 0001 / OXP-104: a blank-matching criterion over a whole column defers
    // (the unbounded absent cells cannot be counted from the populated set).
    #[test]
    fn countif_whole_column_blank_criterion_defers() {
        assert_eq!(
            call(
                "COUNTIF",
                vec![
                    TestArg::Unbounded(vec![num(6.0), num(7.0)]),
                    TestArg::Scalar(txt("")) // matches blank cells
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // SUMIF.md §1: sum_range omitted → the matching range cells sum themselves.
    #[test]
    fn sumif_self_range() {
        let range = vec![num(1.0), num(5.0), num(6.0), num(10.0)];
        // ">5" matches 6 and 10 → 16.
        assert_eq!(
            call(
                "SUMIF",
                vec![TestArg::Range(range), TestArg::Scalar(txt(">5"))]
            ),
            num(16.0)
        );
    }

    // SUMIF.md §1: with a same-shape sum_range, the positionally-corresponding
    // sum_range cells are added.
    #[test]
    fn sumif_with_sum_range_same_shape() {
        let range = vec![txt("a"), txt("b"), txt("a"), txt("c")];
        let sum_range = vec![num(10.0), num(20.0), num(30.0), num(40.0)];
        // criteria "a" matches rows 0 and 2 → 10 + 30 = 40.
        assert_eq!(
            call(
                "SUMIF",
                vec![
                    TestArg::Range(range),
                    TestArg::Scalar(txt("a")),
                    TestArg::Range(sum_range),
                ]
            ),
            num(40.0)
        );
    }

    // SUMIF.md §2: sum_range larger than range → the top-left range-shaped
    // sub-rectangle is used (the anchor/resize rule).
    #[test]
    fn sumif_sum_range_larger_uses_top_left() {
        let range = vec![num(1.0), num(9.0)]; // 2×1
        let sum_range = vec![num(100.0), num(200.0), num(300.0), num(400.0)]; // 4×1
        // ">0" matches both range rows → sum_range[0]+sum_range[1] = 300.
        assert_eq!(
            call(
                "SUMIF",
                vec![
                    TestArg::Range(range),
                    TestArg::Scalar(txt(">0")),
                    TestArg::Range(sum_range),
                ]
            ),
            num(300.0)
        );
    }

    // OXP-101 (RUN-2026-07-11-oracle01): text-ordering criterion in SUMIF.
    // Models `=SUMIF(A1:A5,">apple",B1:B5)` = 140 (banana/cherry/avocado/
    // blueberry match; apple excluded as equal).
    #[test]
    fn sumif_text_ordering_operand() {
        let range = vec![
            txt("apple"),
            txt("banana"),
            txt("cherry"),
            txt("avocado"),
            txt("blueberry"),
        ];
        let sum_range = vec![num(10.0), num(20.0), num(30.0), num(40.0), num(50.0)];
        assert_eq!(
            call(
                "SUMIF",
                vec![
                    TestArg::Range(range),
                    TestArg::Scalar(txt(">apple")),
                    TestArg::Range(sum_range),
                ]
            ),
            num(140.0)
        );
    }

    // OXP-102 (RUN-2026-07-11-oracle01): blank criteria *value* matches the
    // range's `0` cells. Models `=SUMIF(A1:A5,C1,A1:A5)` (C1 empty) = 0.
    #[test]
    fn sumif_blank_criteria_value_matches_zero() {
        let range = vec![num(0.0), num(5.0), num(0.0), num(10.0), num(0.0)];
        assert_eq!(
            call(
                "SUMIF",
                vec![
                    TestArg::Range(range.clone()),
                    TestArg::Scalar(Value::Blank),
                    TestArg::Range(range),
                ]
            ),
            num(0.0)
        );
    }

    // OXP-103: sum_range smaller than range would read cells outside the
    // argument → #UNSUPPORTED! rather than a guess.
    #[test]
    fn sumif_sum_range_smaller_is_unsupported() {
        let range = vec![num(1.0), num(2.0), num(3.0)]; // 3×1
        let sum_range = vec![num(100.0)]; // 1×1
        assert_eq!(
            call(
                "SUMIF",
                vec![
                    TestArg::Range(range),
                    TestArg::Scalar(txt(">0")),
                    TestArg::Range(sum_range),
                ]
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // SUMIF.md §Coercion: only numeric sum_range cells contribute; a matched
    // text/blank sum cell contributes 0.
    #[test]
    fn sumif_sum_range_non_numeric_contributes_zero() {
        let range = vec![txt("a"), txt("a"), txt("a")];
        let sum_range = vec![num(10.0), txt("x"), Value::Blank];
        // All three rows match "a", but only the numeric sum cell counts → 10.
        assert_eq!(
            call(
                "SUMIF",
                vec![
                    TestArg::Range(range),
                    TestArg::Scalar(txt("a")),
                    TestArg::Range(sum_range),
                ]
            ),
            num(10.0)
        );
    }

    // SUMIF.md §Error behavior: an error in a summed (matched) cell propagates.
    #[test]
    fn sumif_error_in_summed_cell_propagates() {
        let range = vec![txt("a"), txt("a")];
        let sum_range = vec![num(10.0), Value::Error(ErrorKind::Div0)];
        assert_eq!(
            call(
                "SUMIF",
                vec![
                    TestArg::Range(range),
                    TestArg::Scalar(txt("a")),
                    TestArg::Range(sum_range),
                ]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    // Dynamic `">"&value` criteria arrive already concatenated as a string; the
    // engine hands SUMIF the final ">5".
    #[test]
    fn sumif_pre_concatenated_dynamic_criteria() {
        let range = vec![num(3.0), num(6.0), num(8.0)];
        assert_eq!(
            call(
                "SUMIF",
                vec![TestArg::Range(range), TestArg::Scalar(txt(">5"))]
            ),
            num(14.0)
        );
    }

    // An omitted third argument slot behaves like sum_range omitted.
    #[test]
    fn sumif_omitted_sum_range_slot_sums_self() {
        let range = vec![num(1.0), num(6.0)];
        assert_eq!(
            call(
                "SUMIF",
                vec![
                    TestArg::Range(range),
                    TestArg::Scalar(txt(">5")),
                    TestArg::Omitted,
                ]
            ),
            num(6.0)
        );
    }

    // RFC 0001: a whole-column self-range is summed over its populated cells.
    #[test]
    fn sumif_whole_column_self_range_used_extent() {
        // [6,7,3,10], ">5" matches 6,7,10 → 23.
        assert_eq!(
            call(
                "SUMIF",
                vec![
                    TestArg::Unbounded(vec![num(6.0), num(7.0), num(3.0), num(10.0)]),
                    TestArg::Scalar(txt(">5"))
                ]
            ),
            num(23.0)
        );
    }

    // RFC 0001: SUMIF(A:A, crit, B:B) — both whole columns, aligned by relative
    // row. range col contains the criteria key, second col ignored in matching.
    #[test]
    fn sumif_whole_column_with_sum_range_used_extent() {
        // range A:A rows: (0)->10, (1)->5, (2)->20 ; sum B:B rows: (0)->1,(1)->2,(2)->3.
        let range = TestArg::UsedRows(vec![
            (0, vec![num(10.0)]),
            (1, vec![num(5.0)]),
            (2, vec![num(20.0)]),
        ]);
        let sum = TestArg::UsedRows(vec![
            (0, vec![num(1.0)]),
            (1, vec![num(2.0)]),
            (2, vec![num(3.0)]),
        ]);
        // ">5" matches rows 0 and 2 → sum cells 1 + 3 = 4.
        assert_eq!(
            call("SUMIF", vec![range, TestArg::Scalar(txt(">5")), sum]),
            num(4.0)
        );
    }

    // RFC 0001: a bounded range against a taller whole-column sum_range is
    // supported (sum cells beyond the range fill as blank / are unused).
    #[test]
    fn sumif_bounded_range_whole_column_sum_range() {
        let range = TestArg::Range(vec![num(10.0), num(1.0), num(20.0)]);
        let sum = TestArg::UsedRows(vec![
            (0, vec![num(100.0)]),
            (1, vec![num(200.0)]),
            (2, vec![num(300.0)]),
        ]);
        // ">5" matches rows 0,2 → 100 + 300 = 400.
        assert_eq!(
            call("SUMIF", vec![range, TestArg::Scalar(txt(">5")), sum]),
            num(400.0)
        );
    }

    // RFC 0001 / OXP-103: a whole-column range against a bounded (shorter)
    // sum_range defers (the resize would read outside the sum_range argument).
    #[test]
    fn sumif_whole_column_range_bounded_sum_range_defers() {
        let range = TestArg::Unbounded(vec![num(6.0), num(7.0)]);
        let sum = TestArg::Range(vec![num(1.0), num(2.0)]);
        assert_eq!(
            call("SUMIF", vec![range, TestArg::Scalar(txt(">5")), sum]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // RFC 0001 / OXP-104: a blank-matching criterion over a whole-column range
    // *with* a sum_range defers (absent rows match but their sum cells are
    // unenumerable). The self-range form has no such issue (tested elsewhere).
    #[test]
    fn sumif_whole_column_blank_criterion_with_sum_range_defers() {
        let range = TestArg::UsedRows(vec![(0, vec![num(6.0)]), (1, vec![num(7.0)])]);
        let sum = TestArg::UsedRows(vec![(0, vec![num(1.0)]), (1, vec![num(2.0)])]);
        assert_eq!(
            call("SUMIF", vec![range, TestArg::Scalar(txt("")), sum]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    // SUMIF/COUNTIF are registered with the documented arities and are
    // non-volatile.
    #[test]
    fn conditional_family_registered_with_expected_arity() {
        let sumif = lookup("SUMIF").expect("SUMIF registered");
        assert!(!sumif.accepts_arity(1), "SUMIF min 2");
        assert!(sumif.accepts_arity(2));
        assert!(sumif.accepts_arity(3));
        assert!(!sumif.accepts_arity(4), "SUMIF max 3");
        assert!(!is_volatile("SUMIF"));

        let countif = lookup("COUNTIF").expect("COUNTIF registered");
        assert!(!countif.accepts_arity(1), "COUNTIF min 2");
        assert!(countif.accepts_arity(2));
        assert!(!countif.accepts_arity(3), "COUNTIF max 2");
        assert!(!is_volatile("COUNTIF"));
    }
}
