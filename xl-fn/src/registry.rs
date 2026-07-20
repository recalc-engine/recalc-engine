//! The function registry: one [`FnSpec`] per function, collected in a single
//! static table, looked up by name.
//!
//! # Registration mechanism (and how it scales to 40 functions)
//! Each function is a module (`func_sum`, `func_if`, …) exposing a private
//! `eval` fn; its metadata is a `const FnSpec` declared here and listed in the
//! [`REGISTRY`] slice. Adding a function is therefore two edits: a new module,
//! and one line in `REGISTRY`. No macro and **no external registration crate**
//! (`inventory`/`linkme`) is used — the zero-dependency rule (a Recalc design rule) forbids
//! new deps, and a hand-listed `static` slice is the simplest thing that stays
//! `const`, order-deterministic, and greppable.
//!
//! [`lookup`] is a linear ASCII-case-insensitive scan. For the Tier-0 fan-out
//! (~40 functions) that is trivially fast and keeps the table a plain array; if
//! profiling ever shows it hot, the scan can be swapped for a compile-time
//! perfect-hash or a `BTreeMap` built once — a change fully behind [`lookup`]'s
//! signature, invisible to `xl-engine` and to every `FnSpec`. The AST already
//! canonicalizes names to uppercase ([`xl_ast::FuncName::canonical`]), so the
//! case-folding is belt-and-suspenders.

use xl_value::Value;

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::{
    func_abs,
    func_acos,
    func_address,
    func_and,
    func_asin,
    func_atan,
    func_average,
    func_averagea,
    func_averageif,
    func_averageifs,
    func_ceiling,
    func_cell,
    func_choose,
    func_choosecols,
    func_chooserows,
    func_column,
    // Pre-wired stubs for the parallel implementation blitz (specs exist):
    func_columns,
    func_concat,
    func_concatenate,
    func_correl,
    func_cos,
    func_count,
    func_counta,
    func_countif,
    func_countifs,
    func_date,
    func_datevalue,
    func_daverage,
    func_day,
    func_days360,
    func_drop,
    func_dsum,
    func_edate,
    func_eomonth,
    func_erfc,
    func_exact,
    func_exp,
    func_expand,
    func_false,
    func_filter,
    func_find,
    func_floor,
    func_fv,
    func_hlookup,
    func_hour,
    func_hstack,
    func_hyperlink,
    func_if,
    func_iferror,
    func_ifna,
    func_ifs,
    func_index,
    func_int,
    func_ipmt,
    func_irr,
    func_isblank,
    func_iserr,
    func_iserror,
    func_isna,
    func_isnumber,
    func_istext,
    func_large,
    func_left,
    func_len,
    func_ln,
    func_log,
    func_lookup,
    func_lower,
    func_match,
    func_max,
    func_maxa,
    func_maxifs,
    func_median,
    func_mid,
    func_min,
    func_mina,
    func_minifs,
    func_mod,
    func_month,
    func_na,
    func_networkdays,
    func_normdist,
    func_norminv,
    func_normsinv,
    func_not,
    func_npv,
    func_or,
    func_pi,
    func_pmt,
    func_power,
    func_ppmt,
    func_product,
    func_proper,
    func_pv,
    func_quotient,
    func_randarray,
    func_rank,
    func_rept,
    func_right,
    func_round,
    func_rounddown,
    func_roundup,
    func_row,
    func_rows,
    func_search,
    func_sequence,
    func_sin,
    func_sln,
    func_small,
    func_sort,
    func_sortby,
    func_sqrt,
    func_stdev,
    func_substitute,
    func_subtotal,
    func_sum,
    func_sumif,
    func_sumifs,
    func_sumproduct,
    func_sumsq,
    func_switch,
    func_take,
    func_text,
    func_textjoin,
    func_tocol,
    func_torow,
    func_trim,
    func_true,
    func_trunc,
    func_unique,
    func_upper,
    func_value,
    func_vdb,
    func_vlookup,
    func_vstack,
    func_weekday,
    func_workday,
    func_wrapcols,
    func_wraprows,
    func_xirr,
    func_xlookup,
    func_xmatch,
    func_xnpv,
    func_xor,
    func_year,
    func_yearfrac,
};

/// Whether a function recomputes on every recalculation.
///
/// The graph records the outcome via `xl_graph::DepGraph::register_volatile`;
/// this is `xl-fn`'s declaration of it (`implementation-plan.md` §2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Volatility {
    /// Recomputes every recalc (`NOW`, `TODAY`, `RAND*`, `OFFSET`, `INDIRECT`,
    /// `CELL`, `INFO`). Most are not yet implemented in v0 (they mark their cell
    /// volatile via `VOLATILE_NAMES`); `CELL` **is** implemented (partial —
    /// see `func_cell`) and carries this flag directly. Either way the flag
    /// governs graph scheduling — see [`is_volatile`].
    Volatile,
    /// Recomputes only when a precedent changes (the common case).
    NonVolatile,
}

/// The signature type of a function's evaluator.
///
/// It receives the shared [`EvalContext`] (the sandbox seam) and a mutable
/// [`CallArgs`] to pull operands from. The context is `&` (not `&mut`) so the
/// engine can pass the *same* context to nested argument evaluations without
/// `&mut` aliasing — see the [`EvalContext`] docs.
pub type EvalFn = fn(&EvalContext, &mut dyn CallArgs) -> Value;

/// Everything the engine needs to dispatch a function call: its name, arity,
/// volatility, and evaluator.
///
/// # Why there is no per-argument coercion metadata
/// Earlier revisions carried a declarative `ArgCoercion`/`ArgSpec` table (one
/// entry per argument position) alongside the evaluator. Nothing consumed it:
/// the engine drives arity from [`min_args`](FnSpec::min_args)/[`max_args`](FnSpec::max_args)
/// (see [`accepts_arity`](FnSpec::accepts_arity), the only registry metadata the
/// engine reads), and each function performs its *own* coercion inside `eval`
/// via [`CallArgs`](crate::CallArgs)/`xl-value`. A declaration that no code
/// checks against the `eval` body is a second source of truth that can silently
/// drift from behavior, so it was removed (the "wire it or cut it" review call:
/// cut, until a generic coercion driver actually consumes it). Arity remains the
/// one wired declaration — a single generic check, not hand-rolled per function.
#[derive(Clone, Copy, Debug)]
pub struct FnSpec {
    /// The canonical (uppercase) function name.
    pub name: &'static str,
    /// Minimum argument count Excel accepts at entry.
    pub min_args: u8,
    /// Maximum argument count; `None` means unbounded (truly variadic).
    pub max_args: Option<u8>,
    /// Whether the function is volatile.
    pub volatility: Volatility,
    /// The evaluator.
    pub eval: EvalFn,
}

impl FnSpec {
    /// Whether `n` arguments satisfy this function's arity.
    #[must_use]
    pub fn accepts_arity(&self, n: usize) -> bool {
        if n < self.min_args as usize {
            return false;
        }
        match self.max_args {
            Some(max) => n <= max as usize,
            None => true,
        }
    }
}

/// `SUM(number1, [number2], ...)` — 1..=255 args, non-volatile; each argument is
/// a shape-dependent aggregate (scalar coercion for scalars, range-aggregate per
/// element for ranges/arrays), applied inside `func_sum`. See `func_sum`.
const SUM_SPEC: FnSpec = FnSpec {
    name: "SUM",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_sum::eval,
};

/// `IF(logical_test, value_if_true, [value_if_false])` — 2..=3 args,
/// non-volatile; the test is scalar, the branches lazy (enforced inside
/// `func_if`). See `func_if`.
const IF_SPEC: FnSpec = FnSpec {
    name: "IF",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_if::eval,
};

/// `AVERAGE(number1, [number2], ...)` — 1..=255 args, non-volatile; the same
/// shape-dependent aggregate as `SUM`, tracking both a sum and a count. See
/// `func_average`.
const AVERAGE_SPEC: FnSpec = FnSpec {
    name: "AVERAGE",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_average::eval,
};

/// `COUNT(value1, [value2], ...)` — 1..=255 args, non-volatile; counts cells/
/// values holding an actual number. See `func_count`.
const COUNT_SPEC: FnSpec = FnSpec {
    name: "COUNT",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_count::eval,
};

/// `COUNTA(value1, [value2], ...)` — 1..=255 args, non-volatile; counts
/// non-blank cells/values (no numeric coercion). See `func_counta`.
const COUNTA_SPEC: FnSpec = FnSpec {
    name: "COUNTA",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_counta::eval,
};

/// `MIN(number1, [number2], ...)` — 1..=255 args, non-volatile; the smallest
/// numeric value across all arguments. See `func_min`.
const MIN_SPEC: FnSpec = FnSpec {
    name: "MIN",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_min::eval,
};

/// `MAX(number1, [number2], ...)` — 1..=255 args, non-volatile; the largest
/// numeric value across all arguments. See `func_max`.
const MAX_SPEC: FnSpec = FnSpec {
    name: "MAX",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_max::eval,
};

/// `YEAR(serial_number)` — 1 arg, non-volatile; the calendar year of a date
/// serial (respecting the 1900 leap-year bug). See `func_year`.
const YEAR_SPEC: FnSpec = FnSpec {
    name: "YEAR",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_year::eval,
};

/// `MONTH(serial_number)` — 1 arg, non-volatile; the month `1..=12`. See
/// `func_month`.
const MONTH_SPEC: FnSpec = FnSpec {
    name: "MONTH",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_month::eval,
};

/// `DAY(serial_number)` — 1 arg, non-volatile; the day-of-month `1..=31`. See
/// `func_day`.
const DAY_SPEC: FnSpec = FnSpec {
    name: "DAY",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_day::eval,
};

/// `DATE(year, month, day)` — 3 args, non-volatile; constructs a date serial
/// with overflow normalization and the 1900 leap-year bug. See `func_date`.
const DATE_SPEC: FnSpec = FnSpec {
    name: "DATE",
    min_args: 3,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_date::eval,
};

/// `EOMONTH(start_date, months)` — 2 args, non-volatile; last day of the month
/// `months` from `start_date`. See `func_eomonth`.
const EOMONTH_SPEC: FnSpec = FnSpec {
    name: "EOMONTH",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_eomonth::eval,
};

/// `VLOOKUP(lookup_value, table_array, col_index_num, [range_lookup])` — 3..=4
/// args, non-volatile; searches `table_array`'s first column (exact linear scan
/// or approximate binary search) and returns the matched row's `col_index_num`
/// column. All comparison deferred to `xl-value`. See `func_vlookup`.
const VLOOKUP_SPEC: FnSpec = FnSpec {
    name: "VLOOKUP",
    min_args: 3,
    max_args: Some(4),
    volatility: Volatility::NonVolatile,
    eval: func_vlookup::eval,
};

/// `HLOOKUP(lookup_value, table_array, row_index_num, [range_lookup])` —
/// 3..=4 args, non-volatile; the row/column transpose of `VLOOKUP`: searches
/// `table_array`'s first row (exact linear scan or approximate binary
/// search) and returns the matched column's `row_index_num` row. All
/// comparison deferred to `xl-value`. See `func_hlookup`.
const HLOOKUP_SPEC: FnSpec = FnSpec {
    name: "HLOOKUP",
    min_args: 3,
    max_args: Some(4),
    volatility: Volatility::NonVolatile,
    eval: func_hlookup::eval,
};

/// `MATCH(lookup_value, lookup_array, [match_type])` — 2..=3 args,
/// non-volatile; returns the 1-based **position** of `lookup_value` within a
/// 1-D `lookup_array` (exact linear scan for `match_type=0`, or an
/// ascending/descending binary search for `match_type=1`/`-1`, reusing
/// `VLOOKUP`'s search-algorithm shapes). See `func_match`.
const MATCH_SPEC: FnSpec = FnSpec {
    name: "MATCH",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_match::eval,
};

/// `INDEX(array, row_num, [col_num])` — 2..=3 args, non-volatile; the
/// array/value-returning form only (the reference form is out of scope for
/// v0). Returns the value at `(row_num, col_num)`, or the entire row/column
/// as a `Value::Array` when the other index is `0`. See `func_index`.
const INDEX_SPEC: FnSpec = FnSpec {
    name: "INDEX",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_index::eval,
};

/// `AND(logical1, [logical2], ...)` — 1..=255 args, non-volatile; boolean
/// conjunction, scalar coercion via `to_bool`, range/array arguments only
/// let `Bool` cells participate. See `func_and`.
const AND_SPEC: FnSpec = FnSpec {
    name: "AND",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_and::eval,
};

/// `OR(logical1, [logical2], ...)` — 1..=255 args, non-volatile; boolean
/// disjunction, the mirror image of `AND`. See `func_or`.
const OR_SPEC: FnSpec = FnSpec {
    name: "OR",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_or::eval,
};

/// `NOT(logical)` — 1 arg, non-volatile; boolean negation via `to_bool`.
/// See `func_not`.
const NOT_SPEC: FnSpec = FnSpec {
    name: "NOT",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_not::eval,
};

/// `ISNUMBER(value)` — 1 arg, non-volatile; kind predicate, never
/// propagates. See `func_isnumber`.
const ISNUMBER_SPEC: FnSpec = FnSpec {
    name: "ISNUMBER",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_isnumber::eval,
};

/// `ISERROR(value)` — 1 arg, non-volatile; TRUE for any error kind,
/// containing rather than propagating it. See `func_iserror`.
const ISERROR_SPEC: FnSpec = FnSpec {
    name: "ISERROR",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_iserror::eval,
};

/// `ISBLANK(value)` — 1 arg, non-volatile; TRUE only for `Blank`, `FALSE`
/// for `Text("")` (the ""-vs-Blank hit-list distinction). See
/// `func_isblank`.
const ISBLANK_SPEC: FnSpec = FnSpec {
    name: "ISBLANK",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_isblank::eval,
};

/// `ISTEXT(value)` — 1 arg, non-volatile; kind predicate, never
/// propagates. See `func_istext`.
const ISTEXT_SPEC: FnSpec = FnSpec {
    name: "ISTEXT",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_istext::eval,
};

/// `ISNA(value)` — 1 arg, non-volatile; TRUE only for `#N/A`, the
/// narrowest predicate in the ISERROR/ISERR/ISNA family. No
/// `docs/specs/ISNA.md`; clean-room from Microsoft's public "IS
/// functions" page (`func_isna` module docs). See `func_isna`.
const ISNA_SPEC: FnSpec = FnSpec {
    name: "ISNA",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_isna::eval,
};

/// `WEEKDAY(serial_number, [return_type])` — 1..=2 args, non-volatile; the
/// day of the week of a date serial, numbered per `return_type` (1/2/3
/// supported; other documented-but-out-of-scope types `#UNSUPPORTED!`, truly
/// invalid types `#NUM!`). See `func_weekday`.
const WEEKDAY_SPEC: FnSpec = FnSpec {
    name: "WEEKDAY",
    min_args: 1,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_weekday::eval,
};

/// `CONCATENATE(text1, [text2], ...)` — 1..=255 args, non-volatile; joins
/// each argument's text representation with no separator, each coerced in
/// scalar context. See `func_concatenate`.
const CONCATENATE_SPEC: FnSpec = FnSpec {
    name: "CONCATENATE",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_concatenate::eval,
};

/// `ROUND(number, num_digits)` — 2 args, non-volatile; rounds half away from
/// zero to `num_digits` decimal places. See `func_round`.
const ROUND_SPEC: FnSpec = FnSpec {
    name: "ROUND",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_round::eval,
};

/// `ABS(number)` — 1 arg, non-volatile; the absolute value. See `func_abs`.
const ABS_SPEC: FnSpec = FnSpec {
    name: "ABS",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_abs::eval,
};

/// `INT(number)` — 1 arg, non-volatile; floors toward negative infinity. See
/// `func_int`.
const INT_SPEC: FnSpec = FnSpec {
    name: "INT",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_int::eval,
};

/// `MOD(number, divisor)` — 2 args, non-volatile; remainder with the sign of
/// `divisor`. See `func_mod`.
const MOD_SPEC: FnSpec = FnSpec {
    name: "MOD",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_mod::eval,
};

/// `SUMIF(range, criteria, [sum_range])` — 2..=3 args, non-volatile; sums the
/// `sum_range` cells (or `range` itself) whose `range` cell matches the
/// criteria mini-language. See `func_sumif` / `criteria`.
const SUMIF_SPEC: FnSpec = FnSpec {
    name: "SUMIF",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_sumif::eval,
};

/// `COUNTIF(range, criteria)` — 2 args, non-volatile; counts the `range` cells
/// matching the criteria mini-language. See `func_countif` / `criteria`.
const COUNTIF_SPEC: FnSpec = FnSpec {
    name: "COUNTIF",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_countif::eval,
};

/// `AVERAGEIF(range, criteria, [average_range])` — 2..=3 args, non-volatile;
/// mirrors `SUMIF`'s signature/geometry exactly, averaging instead of summing.
/// See `func_averageif` / `criteria`.
const AVERAGEIF_SPEC: FnSpec = FnSpec {
    name: "AVERAGEIF",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_averageif::eval,
};

/// `NA()` — 0 args, non-volatile; the constant `#N/A` error value. See `func_na`.
const NA_SPEC: FnSpec = FnSpec {
    name: "NA",
    min_args: 0,
    max_args: Some(0),
    volatility: Volatility::NonVolatile,
    eval: func_na::eval,
};

/// `CELL(info_type, [reference])` — 1..=2 args, **volatile**; information about
/// a cell's location/contents. Partial `info_type` support (`row`, `col`,
/// `type`, non-blank `contents`); every other type, an omitted reference, and a
/// non-single-area reference refuse loudly (OXP-218). See `func_cell`.
const CELL_SPEC: FnSpec = FnSpec {
    name: "CELL",
    min_args: 1,
    max_args: Some(2),
    volatility: Volatility::Volatile,
    eval: func_cell::eval,
};

/// `HYPERLINK(link_location, [friendly_name])` — 1..=2 args, non-volatile; the
/// cell's computed value is the `friendly_name` when supplied, else the
/// `link_location`, type preserved (the jump is inert in a headless engine). A
/// blank selected operand defers loudly pending OXP-217. See `func_hyperlink`.
const HYPERLINK_SPEC: FnSpec = FnSpec {
    name: "HYPERLINK",
    min_args: 1,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_hyperlink::eval,
};

/// `RANK(number, ref, [order])` — 2..=3 args, non-volatile; the rank of
/// `number` among the numbers in `ref` (descending default, ascending for
/// nonzero `order`); ties share the top rank; `#N/A` if absent. See
/// `func_rank`.
const RANK_SPEC: FnSpec = FnSpec {
    name: "RANK",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_rank::eval,
};

/// `RANK.EQ(number, ref, [order])` — the 2010+ rename of `RANK`; identical
/// top-of-tie behavior, one shared evaluator. See `func_rank`.
const RANK_EQ_SPEC: FnSpec = FnSpec {
    name: "RANK.EQ",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_rank::eval,
};

/// `ERFC(x)` — 1 arg, non-volatile; the complementary error function
/// `erfc(x) = 1 − erf(x)` via Cody's rational-Chebyshev `CALERF`. Serves the
/// unambiguous `x ≥ 0` core; a negative argument refuses loudly pending
/// OXP-215 (legacy `#NUM!` vs modern value). See `func_erfc`.
const ERFC_SPEC: FnSpec = FnSpec {
    name: "ERFC",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_erfc::eval,
};

/// `TRUE()` — 0 args, non-volatile; the logical constant `TRUE`. Only the
/// explicit call form reaches the registry (a bare `TRUE` is a parser literal).
/// See `func_true`.
const TRUE_SPEC: FnSpec = FnSpec {
    name: "TRUE",
    min_args: 0,
    max_args: Some(0),
    volatility: Volatility::NonVolatile,
    eval: func_true::eval,
};

/// `FALSE()` — 0 args, non-volatile; the logical constant `FALSE`. The mirror
/// of `TRUE()`. See `func_false`.
const FALSE_SPEC: FnSpec = FnSpec {
    name: "FALSE",
    min_args: 0,
    max_args: Some(0),
    volatility: Volatility::NonVolatile,
    eval: func_false::eval,
};

/// `SIN(number)` — 1 arg, non-volatile; sine of an angle in radians via
/// `f64::sin`. Always finite (result in `[-1, 1]`), so no `#NUM!` path. See
/// `func_sin`.
const SIN_SPEC: FnSpec = FnSpec {
    name: "SIN",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_sin::eval,
};

/// `SQRT(number)` — 1 arg, non-volatile; positive square root, `#NUM!` for a
/// negative argument. See `func_sqrt`.
const SQRT_SPEC: FnSpec = FnSpec {
    name: "SQRT",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_sqrt::eval,
};

/// `PRODUCT(number1, [number2], ...)` — 1..=255 args, non-volatile; the product
/// of all numeric arguments (scalar-coerced scalars, range-aggregate for
/// ranges/arrays). See `func_product`.
const PRODUCT_SPEC: FnSpec = FnSpec {
    name: "PRODUCT",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_product::eval,
};

/// `LN(number)` — 1 arg, non-volatile; natural logarithm, `#NUM!` for `<= 0`.
/// See `func_ln`.
const LN_SPEC: FnSpec = FnSpec {
    name: "LN",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_ln::eval,
};

/// `POWER(number, power)` — 2 args, non-volatile; `number ^ power`. See
/// `func_power`.
const POWER_SPEC: FnSpec = FnSpec {
    name: "POWER",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_power::eval,
};

/// `EXP(number)` — 1 arg, non-volatile; `e ^ number`. See `func_exp`.
const EXP_SPEC: FnSpec = FnSpec {
    name: "EXP",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_exp::eval,
};

/// `ISERR(value)` — 1 arg, non-volatile; TRUE for any error kind *except*
/// `#N/A` (the narrower sibling of `ISERROR`). See `func_iserr`.
const ISERR_SPEC: FnSpec = FnSpec {
    name: "ISERR",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_iserr::eval,
};

/// `LEFT(text, [num_chars])` — 1..=2 args, non-volatile; the leftmost
/// `num_chars` characters (default 1). See `func_left`.
const LEFT_SPEC: FnSpec = FnSpec {
    name: "LEFT",
    min_args: 1,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_left::eval,
};

/// `MID(text, start_num, num_chars)` — 3 args, non-volatile; `num_chars`
/// characters of `text` beginning at 1-based `start_num`. See `func_mid`.
const MID_SPEC: FnSpec = FnSpec {
    name: "MID",
    min_args: 3,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_mid::eval,
};

/// `FIND(find_text, within_text, [start_num])` — 2..=3 args, non-volatile;
/// case-sensitive, non-wildcard 1-based position of `find_text` in
/// `within_text`. See `func_find`.
const FIND_SPEC: FnSpec = FnSpec {
    name: "FIND",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_find::eval,
};

/// `SUBTOTAL(function_num, ref1, [ref2], ...)` — 2..=255 args, non-volatile;
/// an aggregate whose operation is selected by `function_num` (1-11 include /
/// 101-111 exclude manually-hidden rows), ignoring nested SUBTOTALs. See
/// `func_subtotal`.
const SUBTOTAL_SPEC: FnSpec = FnSpec {
    name: "SUBTOTAL",
    min_args: 2,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_subtotal::eval,
};

/// `SUMPRODUCT(array1, [array2], ...)` — 1..=255 args, non-volatile; the sum of
/// the element-wise products of equally-shaped arrays. See `func_sumproduct`.
const SUMPRODUCT_SPEC: FnSpec = FnSpec {
    name: "SUMPRODUCT",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_sumproduct::eval,
};

/// `LOOKUP(lookup_value, lookup_vector, [result_vector])` / `LOOKUP(value,
/// array)` — 2..=3 args, non-volatile; approximate (sorted) search returning the
/// aligned result. See `func_lookup`.
const LOOKUP_SPEC: FnSpec = FnSpec {
    name: "LOOKUP",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_lookup::eval,
};

/// `CHOOSE(index_num, value1, [value2], ...)` — 2..=255 args, non-volatile;
/// returns the `index_num`-th value, evaluating only the selected one (lazy).
/// See `func_choose`.
const CHOOSE_SPEC: FnSpec = FnSpec {
    name: "CHOOSE",
    min_args: 2,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_choose::eval,
};

/// `NPV(rate, value1, [value2], ...)` — 2..=255 args, non-volatile; net present
/// value of a series of cash flows discounted at `rate`, the first value one
/// period out. See `func_npv`.
const NPV_SPEC: FnSpec = FnSpec {
    name: "NPV",
    min_args: 2,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_npv::eval,
};

/// `VALUE(text)` — 1 arg, non-volatile; converts a text number/date/time into a
/// numeric value (en-US formats). See `func_value`.
const VALUE_SPEC: FnSpec = FnSpec {
    name: "VALUE",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_value::eval,
};

/// `MINA(value1, [value2], ...)` — 1..=255 args, non-volatile; smallest value,
/// counting logicals (TRUE=1/FALSE=0) and text (as 0), unlike `MIN`. See
/// `func_mina`.
const MINA_SPEC: FnSpec = FnSpec {
    name: "MINA",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_mina::eval,
};

/// `MAXA(value1, [value2], ...)` — 1..=255 args, non-volatile; largest value,
/// counting logicals and text-as-0, unlike `MAX`. See `func_maxa`.
const MAXA_SPEC: FnSpec = FnSpec {
    name: "MAXA",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_maxa::eval,
};

/// `DAYS360(start_date, end_date, [method])` — 2..=3 args, non-volatile; day
/// count on a 360-day (12x30) year, US (NASD) or European method. See
/// `func_days360`.
const DAYS360_SPEC: FnSpec = FnSpec {
    name: "DAYS360",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_days360::eval,
};

/// `PPMT(rate, per, nper, pv, [fv], [type])` — 4..=6 args, non-volatile; the
/// principal portion of an annuity payment for a given period. See `func_ppmt`.
const PPMT_SPEC: FnSpec = FnSpec {
    name: "PPMT",
    min_args: 4,
    max_args: Some(6),
    volatility: Volatility::NonVolatile,
    eval: func_ppmt::eval,
};

/// `IPMT(rate, per, nper, pv, [fv], [type])` — 4..=6 args, non-volatile; the
/// interest portion of an annuity payment for a given period. See `func_ipmt`.
const IPMT_SPEC: FnSpec = FnSpec {
    name: "IPMT",
    min_args: 4,
    max_args: Some(6),
    volatility: Volatility::NonVolatile,
    eval: func_ipmt::eval,
};

/// `DSUM(database, field, criteria)` — 3 args, non-volatile; sums a column of a
/// database's records that match the criteria range. See `func_dsum`.
const DSUM_SPEC: FnSpec = FnSpec {
    name: "DSUM",
    min_args: 3,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_dsum::eval,
};

const PMT_SPEC: FnSpec = FnSpec {
    name: "PMT",
    min_args: 3,
    max_args: Some(5),
    volatility: Volatility::NonVolatile,
    eval: func_pmt::eval,
};

const RIGHT_SPEC: FnSpec = FnSpec {
    name: "RIGHT",
    min_args: 1,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_right::eval,
};

const ROWS_SPEC: FnSpec = FnSpec {
    name: "ROWS",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_rows::eval,
};

const SUMSQ_SPEC: FnSpec = FnSpec {
    name: "SUMSQ",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_sumsq::eval,
};

const QUOTIENT_SPEC: FnSpec = FnSpec {
    name: "QUOTIENT",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_quotient::eval,
};

const HOUR_SPEC: FnSpec = FnSpec {
    name: "HOUR",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_hour::eval,
};

const SLN_SPEC: FnSpec = FnSpec {
    name: "SLN",
    min_args: 3,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_sln::eval,
};

const EDATE_SPEC: FnSpec = FnSpec {
    name: "EDATE",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_edate::eval,
};

const STDEV_SPEC: FnSpec = FnSpec {
    name: "STDEV",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_stdev::eval,
};

const WORKDAY_SPEC: FnSpec = FnSpec {
    name: "WORKDAY",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_workday::eval,
};

const CORREL_SPEC: FnSpec = FnSpec {
    name: "CORREL",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_correl::eval,
};

const TEXT_SPEC: FnSpec = FnSpec {
    name: "TEXT",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_text::eval,
};

// --- Coverage wave 2 (v3 capstone unimplemented-function ranking): math/text
// tail. Trig core is a single-argument, unambiguous libm pass-through (mirrors
// SIN); ACOS/ASIN carry the documented `[-1, 1]` `#NUM!` domain. ---
const COS_SPEC: FnSpec = FnSpec {
    name: "COS",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_cos::eval,
};
const ACOS_SPEC: FnSpec = FnSpec {
    name: "ACOS",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_acos::eval,
};
const ASIN_SPEC: FnSpec = FnSpec {
    name: "ASIN",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_asin::eval,
};
const ATAN_SPEC: FnSpec = FnSpec {
    name: "ATAN",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_atan::eval,
};
/// `TRUNC(number, [num_digits])` — 1..=2 args, non-volatile; truncates toward
/// zero at `num_digits` (default 0). Same directed-rounding kernel as
/// `ROUNDDOWN`. See `func_trunc`.
const TRUNC_SPEC: FnSpec = FnSpec {
    name: "TRUNC",
    min_args: 1,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_trunc::eval,
};
/// `LOG(number, [base])` — 1..=2 args, non-volatile; base-`base` log (default
/// 10). Documented `number ≤ 0` → `#NUM!`; degenerate `base` (`≤ 0` or `1`)
/// deferred to OXP-224. See `func_log`.
const LOG_SPEC: FnSpec = FnSpec {
    name: "LOG",
    min_args: 1,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_log::eval,
};
/// `FLOOR(number, significance)` — 2 args, non-volatile; rounds down to the
/// nearest multiple of `significance`. Serves the unambiguous `number ≥ 0` &
/// `significance > 0` core; negative-number / non-positive-significance edges
/// deferred to OXP-223. See `func_floor`.
const FLOOR_SPEC: FnSpec = FnSpec {
    name: "FLOOR",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_floor::eval,
};
/// `CEILING(number, significance)` — 2 args, non-volatile; rounds up to the
/// nearest multiple of `significance`. The round-up `FLOOR` sibling; full sign
/// rule pinned by OXP-229 (`significance == 0` → `0`, `number > 0 &&
/// significance < 0` → `#NUM!`, else `ceil(number/significance)·significance`).
/// See `func_ceiling`.
const CEILING_SPEC: FnSpec = FnSpec {
    name: "CEILING",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_ceiling::eval,
};
/// `SEARCH(find_text, within_text, [start_num])` — 2..=3 args, non-volatile;
/// case-insensitive substring position (the wildcard-aware `FIND` sibling).
/// Serves literal ASCII-cased search **and** `*`/`?`/`~`-escape wildcards
/// (OXP-225); an unpinned `~`-escape and non-ASCII case folding still defer
/// (LOWER/UPPER precedent). See `func_search`.
const SEARCH_SPEC: FnSpec = FnSpec {
    name: "SEARCH",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_search::eval,
};
/// `PROPER(text)` — 1 arg, non-volatile; capitalizes the first letter of each
/// word (ASCII casing; non-ASCII letters deferred, LOWER/UPPER precedent). See
/// `func_proper`.
const PROPER_SPEC: FnSpec = FnSpec {
    name: "PROPER",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_proper::eval,
};

/// The complete v0 function table (the "minimal evaluator" plus the Tier-0
/// numeric-aggregation family `SUM`, `IF`, `AVERAGE`, `COUNT`, `COUNTA`,
/// `MIN`, `MAX` and the date-component family `YEAR`, `MONTH`, `DAY`, `DATE`,
/// `EOMONTH`).
// --- Pre-wired stubs for the parallel implementation blitz. Each has a
// written spec in docs/specs/; the eval fn returns #UNSUPPORTED! until filled
// in by its implementation agent. Arity comes from the spec Signature line. ---
const IFERROR_SPEC: FnSpec = FnSpec {
    name: "IFERROR",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_iferror::eval,
};
const SUMIFS_SPEC: FnSpec = FnSpec {
    name: "SUMIFS",
    min_args: 3,
    max_args: None,
    volatility: Volatility::NonVolatile,
    eval: func_sumifs::eval,
};
const COUNTIFS_SPEC: FnSpec = FnSpec {
    name: "COUNTIFS",
    min_args: 2,
    max_args: None,
    volatility: Volatility::NonVolatile,
    eval: func_countifs::eval,
};
/// `AVERAGEIFS(average_range, criteria_range1, criteria1, ...)` — 3..=N args
/// (odd, `average_range` + N complete pairs), non-volatile; mirrors
/// `SUMIFS`'s signature/geometry exactly (note: `average_range` is FIRST,
/// unlike `AVERAGEIF`'s optional-last `[average_range]`). See
/// `func_averageifs` / `criteria`.
const AVERAGEIFS_SPEC: FnSpec = FnSpec {
    name: "AVERAGEIFS",
    min_args: 3,
    max_args: None,
    volatility: Volatility::NonVolatile,
    eval: func_averageifs::eval,
};
/// `IFNA(value, value_if_na)` — 2 args, non-volatile; mirrors `IFERROR`'s
/// structure exactly, narrowed to catch only `#N/A`. See `func_ifna`.
const IFNA_SPEC: FnSpec = FnSpec {
    name: "IFNA",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_ifna::eval,
};
const LEN_SPEC: FnSpec = FnSpec {
    name: "LEN",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_len::eval,
};
const TRIM_SPEC: FnSpec = FnSpec {
    name: "TRIM",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_trim::eval,
};
const UPPER_SPEC: FnSpec = FnSpec {
    name: "UPPER",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_upper::eval,
};
const LOWER_SPEC: FnSpec = FnSpec {
    name: "LOWER",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_lower::eval,
};
const CONCAT_SPEC: FnSpec = FnSpec {
    name: "CONCAT",
    min_args: 1,
    max_args: None,
    volatility: Volatility::NonVolatile,
    eval: func_concat::eval,
};
const COLUMNS_SPEC: FnSpec = FnSpec {
    name: "COLUMNS",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_columns::eval,
};

const AVERAGEA_SPEC: FnSpec = FnSpec {
    name: "AVERAGEA",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_averagea::eval,
};
const PV_SPEC: FnSpec = FnSpec {
    name: "PV",
    min_args: 3,
    max_args: Some(5),
    volatility: Volatility::NonVolatile,
    eval: func_pv::eval,
};
const FV_SPEC: FnSpec = FnSpec {
    name: "FV",
    min_args: 3,
    max_args: Some(5),
    volatility: Volatility::NonVolatile,
    eval: func_fv::eval,
};
const ROUNDUP_SPEC: FnSpec = FnSpec {
    name: "ROUNDUP",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_roundup::eval,
};
const ROUNDDOWN_SPEC: FnSpec = FnSpec {
    name: "ROUNDDOWN",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_rounddown::eval,
};
const LARGE_SPEC: FnSpec = FnSpec {
    name: "LARGE",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_large::eval,
};
const SMALL_SPEC: FnSpec = FnSpec {
    name: "SMALL",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_small::eval,
};
const MEDIAN_SPEC: FnSpec = FnSpec {
    name: "MEDIAN",
    min_args: 1,
    max_args: Some(255),
    volatility: Volatility::NonVolatile,
    eval: func_median::eval,
};
const EXACT_SPEC: FnSpec = FnSpec {
    name: "EXACT",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_exact::eval,
};
const PI_SPEC: FnSpec = FnSpec {
    name: "PI",
    min_args: 0,
    max_args: Some(0),
    volatility: Volatility::NonVolatile,
    eval: func_pi::eval,
};
const REPT_SPEC: FnSpec = FnSpec {
    name: "REPT",
    min_args: 2,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_rept::eval,
};
const NETWORKDAYS_SPEC: FnSpec = FnSpec {
    name: "NETWORKDAYS",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_networkdays::eval,
};
const DAVERAGE_SPEC: FnSpec = FnSpec {
    name: "DAVERAGE",
    min_args: 3,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_daverage::eval,
};
const ADDRESS_SPEC: FnSpec = FnSpec {
    name: "ADDRESS",
    min_args: 2,
    max_args: Some(5),
    volatility: Volatility::NonVolatile,
    eval: func_address::eval,
};
const YEARFRAC_SPEC: FnSpec = FnSpec {
    name: "YEARFRAC",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_yearfrac::eval,
};
const SUBSTITUTE_SPEC: FnSpec = FnSpec {
    name: "SUBSTITUTE",
    min_args: 3,
    max_args: Some(4),
    volatility: Volatility::NonVolatile,
    eval: func_substitute::eval,
};
const ROW_SPEC: FnSpec = FnSpec {
    name: "ROW",
    min_args: 0,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_row::eval,
};
const COLUMN_SPEC: FnSpec = FnSpec {
    name: "COLUMN",
    min_args: 0,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_column::eval,
};

// --- M2 Lane 3b — dynamic-array lookup/sort/filter tranche. Each implements
// only unambiguously-documented behavior; hazard modes/edges refuse loudly and
// are queued in docs/plans/2026-07-15-lane3b-probe-needed.md. ---

/// `XLOOKUP(lookup_value, lookup_array, return_array, [if_not_found],
/// [match_mode], [search_mode])` — 3..=6 args, non-volatile; exact-match search
/// (first/last direction) returning the aligned single-or-multi-cell (spilling)
/// result. Wildcard/binary/approximate-fallback modes refuse loudly. See
/// `func_xlookup`.
const XLOOKUP_SPEC: FnSpec = FnSpec {
    name: "XLOOKUP",
    min_args: 3,
    max_args: Some(6),
    volatility: Volatility::NonVolatile,
    eval: func_xlookup::eval,
};

/// `XMATCH(lookup_value, lookup_array, [match_mode], [search_mode])` — 2..=4
/// args, non-volatile; the 1-based relative position of an exact match
/// (first/last direction). Wildcard/binary/approximate-fallback modes refuse
/// loudly. See `func_xmatch`.
const XMATCH_SPEC: FnSpec = FnSpec {
    name: "XMATCH",
    min_args: 2,
    max_args: Some(4),
    volatility: Volatility::NonVolatile,
    eval: func_xmatch::eval,
};

/// `FILTER(array, include, [if_empty])` — 2..=3 args, non-volatile; keeps the
/// rows/columns whose lockstep boolean `include` is TRUE; empty → `if_empty`
/// else `#CALC!`; shape mismatch → `#VALUE!`. See `func_filter`.
const FILTER_SPEC: FnSpec = FnSpec {
    name: "FILTER",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_filter::eval,
};

/// `SORT(array, [sort_index], [sort_order], [by_col])` — 1..=4 args,
/// non-volatile; stable reorder of rows/columns by one key line via `compare`'s
/// frozen total order (mixed-type/blank placement unpinned — OXP-040). See
/// `func_sort`.
const SORT_SPEC: FnSpec = FnSpec {
    name: "SORT",
    min_args: 1,
    max_args: Some(4),
    volatility: Volatility::NonVolatile,
    eval: func_sort::eval,
};

/// `SORTBY(array, by_array1, [sort_order1], [by_array2, sort_order2], …)` —
/// 2..N args, non-volatile; stable multi-key reorder of rows/columns by separate
/// `by_array` keys; mismatched shape/size or bad order → `#VALUE!`. See
/// `func_sortby`.
const SORTBY_SPEC: FnSpec = FnSpec {
    name: "SORTBY",
    min_args: 2,
    max_args: None,
    volatility: Volatility::NonVolatile,
    eval: func_sortby::eval,
};

/// `UNIQUE(array, [by_col], [exactly_once])` — 1..=3 args, non-volatile; distinct
/// rows/columns in first-occurrence order; refuses genuinely ambiguous dedup
/// equality (text case collision, `Blank` morph). See `func_unique`.
const UNIQUE_SPEC: FnSpec = FnSpec {
    name: "UNIQUE",
    min_args: 1,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_unique::eval,
};

/// `NORMDIST(x, mean, standard_dev, cumulative)` — 4 args, non-volatile; the
/// normal CDF (`cumulative` TRUE) or PDF (FALSE). `standard_dev ≤ 0` → `#NUM!`.
/// CDF via the shared Cody `erfc` kernel. See `func_normdist`.
const NORMDIST_SPEC: FnSpec = FnSpec {
    name: "NORMDIST",
    min_args: 4,
    max_args: Some(4),
    volatility: Volatility::NonVolatile,
    eval: func_normdist::eval,
};

/// `NORM.DIST(x, mean, standard_dev, cumulative)` — the 2010+ rename of
/// `NORMDIST`; identical implementation. See `func_normdist`.
const NORM_DIST_SPEC: FnSpec = FnSpec {
    name: "NORM.DIST",
    min_args: 4,
    max_args: Some(4),
    volatility: Volatility::NonVolatile,
    eval: func_normdist::eval,
};

const NORMSINV_SPEC: FnSpec = FnSpec {
    name: "NORMSINV",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_normsinv::eval,
};
const NORMINV_SPEC: FnSpec = FnSpec {
    name: "NORMINV",
    min_args: 3,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_norminv::eval,
};
const IRR_SPEC: FnSpec = FnSpec {
    name: "IRR",
    min_args: 1,
    max_args: Some(2),
    volatility: Volatility::NonVolatile,
    eval: func_irr::eval,
};
const XIRR_SPEC: FnSpec = FnSpec {
    name: "XIRR",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_xirr::eval,
};
const XNPV_SPEC: FnSpec = FnSpec {
    name: "XNPV",
    min_args: 3,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_xnpv::eval,
};
const VDB_SPEC: FnSpec = FnSpec {
    name: "VDB",
    min_args: 5,
    max_args: Some(7),
    volatility: Volatility::NonVolatile,
    eval: func_vdb::eval,
};

const DATEVALUE_SPEC: FnSpec = FnSpec {
    name: "DATEVALUE",
    min_args: 1,
    max_args: Some(1),
    volatility: Volatility::NonVolatile,
    eval: func_datevalue::eval,
};

// --- M2 lane 5 (Tier-1 scalar tail), first tranche. Each declares only the
// documented behavior; ambiguous edges refuse loudly and are queued in
// docs/plans/2026-07-15-lane5-probe-needed.md. ---

/// `IFS(logical_test1, value_if_true1, [logical_test2, value_if_true2], …)` —
/// 2..=254 args (up to 127 test/value pairs), non-volatile; returns the value
/// paired with the first TRUE test, else `#N/A`. Lazy in the values. See
/// `func_ifs`.
const IFS_SPEC: FnSpec = FnSpec {
    name: "IFS",
    min_args: 2,
    max_args: Some(254),
    volatility: Volatility::NonVolatile,
    eval: func_ifs::eval,
};

/// `SWITCH(expression, value1, result1, [value2, result2], …, [default])` —
/// 3..=254 args (up to 126 value/result pairs + optional default),
/// non-volatile; returns the result paired with the first `valueN` equal to
/// `expression` (Excel `=`), else `default` or `#N/A`. Lazy in the results.
/// See `func_switch`.
const SWITCH_SPEC: FnSpec = FnSpec {
    name: "SWITCH",
    min_args: 3,
    max_args: Some(254),
    volatility: Volatility::NonVolatile,
    eval: func_switch::eval,
};

/// `XOR(logical1, [logical2], …)` — 1..=254 args, non-volatile; TRUE iff an odd
/// number of contributing logical values are TRUE (parity). Mirrors `AND`/`OR`
/// coercion; `#VALUE!` if no logical value is found. See `func_xor`.
const XOR_SPEC: FnSpec = FnSpec {
    name: "XOR",
    min_args: 1,
    max_args: Some(254),
    volatility: Volatility::NonVolatile,
    eval: func_xor::eval,
};

/// `TEXTJOIN(delimiter, ignore_empty, text1, …)` — 3..=254 args, non-volatile;
/// joins each item's text with `delimiter`, omitting empty cells when
/// `ignore_empty` is TRUE. See `func_textjoin`.
const TEXTJOIN_SPEC: FnSpec = FnSpec {
    name: "TEXTJOIN",
    min_args: 3,
    max_args: Some(254),
    volatility: Volatility::NonVolatile,
    eval: func_textjoin::eval,
};

/// `MAXIFS(max_range, criteria_range1, criteria1, …)` — 3..=N args (odd:
/// `max_range` + N complete pairs), non-volatile; the maximum of the numeric
/// `max_range` cells matching every criteria pair, `0` if none. Mirrors
/// `SUMIFS`'s criteria/geometry with a `MAX` fold. See `func_maxifs` /
/// `criteria`.
const MAXIFS_SPEC: FnSpec = FnSpec {
    name: "MAXIFS",
    min_args: 3,
    max_args: None,
    volatility: Volatility::NonVolatile,
    eval: func_maxifs::eval,
};

/// `MINIFS(min_range, criteria_range1, criteria1, …)` — 3..=N args (odd),
/// non-volatile; the minimum of the numeric `min_range` cells matching every
/// criteria pair, `0` if none. Mirrors `MAXIFS` with a `MIN` fold. See
/// `func_minifs` / `criteria`.
const MINIFS_SPEC: FnSpec = FnSpec {
    name: "MINIFS",
    min_args: 3,
    max_args: None,
    volatility: Volatility::NonVolatile,
    eval: func_minifs::eval,
};

// --- M2 lane 3a (dynamic-array reshape / generate tranche). Each implements
// only documented behavior; ambiguous / undocumented edges refuse loudly and
// are queued in docs/plans/2026-07-15-lane3a-probe-needed.md. Shared helpers
// (materialization, cap, spill) live in `arrayshape`. Results are top-level
// `Value::Array`s (spilled by the engine). ---

/// `SEQUENCE(rows, [columns], [start], [step])` — 1..=4 args, non-volatile;
/// generates a sequence. Only **one-dimensional** sequences are implemented; a
/// genuinely 2-D fill order is image-only on the MS page → `#UNSUPPORTED!`. See
/// `func_sequence`.
const SEQUENCE_SPEC: FnSpec = FnSpec {
    name: "SEQUENCE",
    min_args: 1,
    max_args: Some(4),
    volatility: Volatility::NonVolatile,
    eval: func_sequence::eval,
};

/// `TOCOL(array, [ignore], [scan_by_column])` — 1..=3 args, non-volatile;
/// flattens `array` into a single column. See `func_tocol`.
const TOCOL_SPEC: FnSpec = FnSpec {
    name: "TOCOL",
    min_args: 1,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_tocol::eval,
};

/// `TOROW(array, [ignore], [scan_by_column])` — 1..=3 args, non-volatile;
/// flattens `array` into a single row. See `func_torow`.
const TOROW_SPEC: FnSpec = FnSpec {
    name: "TOROW",
    min_args: 1,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_torow::eval,
};

/// `VSTACK(array1, [array2], …)` — 1..=N args, non-volatile; stacks arrays
/// vertically, padding ragged widths with `#N/A`. See `func_vstack`.
const VSTACK_SPEC: FnSpec = FnSpec {
    name: "VSTACK",
    min_args: 1,
    max_args: None,
    volatility: Volatility::NonVolatile,
    eval: func_vstack::eval,
};

/// `HSTACK(array1, [array2], …)` — 1..=N args, non-volatile; stacks arrays
/// horizontally, padding ragged heights with `#N/A`. See `func_hstack`.
const HSTACK_SPEC: FnSpec = FnSpec {
    name: "HSTACK",
    min_args: 1,
    max_args: None,
    volatility: Volatility::NonVolatile,
    eval: func_hstack::eval,
};

/// `TAKE(array, rows, [columns])` — 2..=3 args, non-volatile; keeps contiguous
/// rows/columns from the start (or, negative, the end). See `func_take`.
const TAKE_SPEC: FnSpec = FnSpec {
    name: "TAKE",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_take::eval,
};

/// `DROP(array, rows, [columns])` — 2..=3 args, non-volatile; excludes
/// contiguous rows/columns from the start (or, negative, the end). See
/// `func_drop`.
const DROP_SPEC: FnSpec = FnSpec {
    name: "DROP",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_drop::eval,
};

/// `EXPAND(array, rows, [columns], [pad_with])` — 2..=4 args, non-volatile; pads
/// an array out to larger dimensions (default pad `#N/A`). See `func_expand`.
const EXPAND_SPEC: FnSpec = FnSpec {
    name: "EXPAND",
    min_args: 2,
    max_args: Some(4),
    volatility: Volatility::NonVolatile,
    eval: func_expand::eval,
};

/// `WRAPROWS(vector, wrap_count, [pad_with])` — 2..=3 args, non-volatile; wraps a
/// 1-D vector into rows of `wrap_count` width. See `func_wraprows`.
const WRAPROWS_SPEC: FnSpec = FnSpec {
    name: "WRAPROWS",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_wraprows::eval,
};

/// `WRAPCOLS(vector, wrap_count, [pad_with])` — 2..=3 args, non-volatile; wraps a
/// 1-D vector into columns of `wrap_count` height. See `func_wrapcols`.
const WRAPCOLS_SPEC: FnSpec = FnSpec {
    name: "WRAPCOLS",
    min_args: 2,
    max_args: Some(3),
    volatility: Volatility::NonVolatile,
    eval: func_wrapcols::eval,
};

/// `CHOOSEROWS(array, row_num1, …)` — 2..=N args, non-volatile; selects rows by
/// (1-based, negative-from-end) index. See `func_chooserows`.
const CHOOSEROWS_SPEC: FnSpec = FnSpec {
    name: "CHOOSEROWS",
    min_args: 2,
    max_args: None,
    volatility: Volatility::NonVolatile,
    eval: func_chooserows::eval,
};

/// `CHOOSECOLS(array, col_num1, …)` — 2..=N args, non-volatile; selects columns
/// by (1-based, negative-from-end) index. See `func_choosecols`.
const CHOOSECOLS_SPEC: FnSpec = FnSpec {
    name: "CHOOSECOLS",
    min_args: 2,
    max_args: None,
    volatility: Volatility::NonVolatile,
    eval: func_choosecols::eval,
};

/// `RANDARRAY([rows], [columns], [min], [max], [whole_number])` — 0..=5 args.
/// **Refuses** (`#UNSUPPORTED!`) in v0: it needs the seeded RNG that `RAND` also
/// lacks, and guessing / a fake RNG is forbidden. Registered **non-volatile**
/// (it refuses deterministically); its graph volatility comes from
/// `VOLATILE_NAMES` (which lists `RANDARRAY`), so `is_volatile` stays true. Flips
/// to `Volatility::Volatile` when a real seeded RNG lands. See `func_randarray`.
const RANDARRAY_SPEC: FnSpec = FnSpec {
    name: "RANDARRAY",
    min_args: 0,
    max_args: Some(5),
    volatility: Volatility::NonVolatile,
    eval: func_randarray::eval,
};

pub static REGISTRY: &[FnSpec] = &[
    SUM_SPEC,
    IF_SPEC,
    AVERAGE_SPEC,
    COUNT_SPEC,
    COUNTA_SPEC,
    MIN_SPEC,
    MAX_SPEC,
    YEAR_SPEC,
    MONTH_SPEC,
    DAY_SPEC,
    DATE_SPEC,
    EOMONTH_SPEC,
    VLOOKUP_SPEC,
    HLOOKUP_SPEC,
    MATCH_SPEC,
    INDEX_SPEC,
    AND_SPEC,
    OR_SPEC,
    NOT_SPEC,
    ISNUMBER_SPEC,
    ISERROR_SPEC,
    ISBLANK_SPEC,
    ISTEXT_SPEC,
    ISNA_SPEC,
    WEEKDAY_SPEC,
    CONCATENATE_SPEC,
    ROUND_SPEC,
    ABS_SPEC,
    INT_SPEC,
    MOD_SPEC,
    SUMIF_SPEC,
    COUNTIF_SPEC,
    AVERAGEIF_SPEC,
    NA_SPEC,
    TRUE_SPEC,
    FALSE_SPEC,
    ERFC_SPEC,
    CELL_SPEC,
    HYPERLINK_SPEC,
    RANK_SPEC,
    RANK_EQ_SPEC,
    SIN_SPEC,
    COS_SPEC,
    ACOS_SPEC,
    ASIN_SPEC,
    ATAN_SPEC,
    TRUNC_SPEC,
    LOG_SPEC,
    FLOOR_SPEC,
    CEILING_SPEC,
    SEARCH_SPEC,
    PROPER_SPEC,
    SQRT_SPEC,
    PRODUCT_SPEC,
    LN_SPEC,
    POWER_SPEC,
    EXP_SPEC,
    ISERR_SPEC,
    LEFT_SPEC,
    MID_SPEC,
    FIND_SPEC,
    SUBTOTAL_SPEC,
    SUMPRODUCT_SPEC,
    LOOKUP_SPEC,
    CHOOSE_SPEC,
    NPV_SPEC,
    VALUE_SPEC,
    MINA_SPEC,
    MAXA_SPEC,
    DAYS360_SPEC,
    PPMT_SPEC,
    IPMT_SPEC,
    DSUM_SPEC,
    PMT_SPEC,
    RIGHT_SPEC,
    ROWS_SPEC,
    SUMSQ_SPEC,
    QUOTIENT_SPEC,
    HOUR_SPEC,
    SLN_SPEC,
    EDATE_SPEC,
    STDEV_SPEC,
    WORKDAY_SPEC,
    CORREL_SPEC,
    TEXT_SPEC,
    IFERROR_SPEC,
    IFNA_SPEC,
    SUMIFS_SPEC,
    COUNTIFS_SPEC,
    AVERAGEIFS_SPEC,
    LEN_SPEC,
    TRIM_SPEC,
    UPPER_SPEC,
    LOWER_SPEC,
    CONCAT_SPEC,
    COLUMNS_SPEC,
    DATEVALUE_SPEC,
    NORMDIST_SPEC,
    NORM_DIST_SPEC,
    NORMSINV_SPEC,
    NORMINV_SPEC,
    IRR_SPEC,
    XIRR_SPEC,
    XNPV_SPEC,
    VDB_SPEC,
    AVERAGEA_SPEC,
    PV_SPEC,
    FV_SPEC,
    ROUNDUP_SPEC,
    ROUNDDOWN_SPEC,
    LARGE_SPEC,
    SMALL_SPEC,
    MEDIAN_SPEC,
    EXACT_SPEC,
    PI_SPEC,
    REPT_SPEC,
    NETWORKDAYS_SPEC,
    DAVERAGE_SPEC,
    ADDRESS_SPEC,
    YEARFRAC_SPEC,
    SUBSTITUTE_SPEC,
    ROW_SPEC,
    COLUMN_SPEC,
    // M2 lane 5 — Tier-1 scalar tail.
    IFS_SPEC,
    SWITCH_SPEC,
    XOR_SPEC,
    TEXTJOIN_SPEC,
    MAXIFS_SPEC,
    MINIFS_SPEC,
    // M2 lane 3a — dynamic-array reshape / generate tranche.
    SEQUENCE_SPEC,
    TOCOL_SPEC,
    TOROW_SPEC,
    VSTACK_SPEC,
    HSTACK_SPEC,
    TAKE_SPEC,
    DROP_SPEC,
    EXPAND_SPEC,
    WRAPROWS_SPEC,
    WRAPCOLS_SPEC,
    CHOOSEROWS_SPEC,
    CHOOSECOLS_SPEC,
    RANDARRAY_SPEC,
    // M2 lane 3b — dynamic-array lookup/sort/filter tranche.
    XLOOKUP_SPEC,
    XMATCH_SPEC,
    FILTER_SPEC,
    SORT_SPEC,
    SORTBY_SPEC,
    UNIQUE_SPEC,
];

/// Function names that must mark their cell **volatile** but do not (yet) carry
/// [`Volatility::Volatile`] in a registered [`FnSpec`].
///
/// The graph must still mark a cell volatile when its formula calls one of
/// these (`implementation-plan.md` §2; the set matches `xl-graph`'s documented
/// volatile set). Two cases live here:
/// - **Unregistered volatiles** (`NOW`, `TODAY`, `RAND`, `RANDBETWEEN`,
///   `OFFSET`, `INDIRECT`, `INFO`): absent from [`REGISTRY`], so a call
///   evaluates to `#UNSUPPORTED!` in v0, but its cell is still volatile.
/// - **Registered-but-refusing volatiles** (`RANDARRAY`): present in
///   [`REGISTRY`] with a `NonVolatile` `FnSpec` because it refuses
///   deterministically in v0 (no seeded RNG — see `func_randarray`), yet its
///   cell must still be treated as volatile. [`is_volatile`] ORs this list with
///   the `FnSpec` flag, so `is_volatile("RANDARRAY")` stays `true`; the `FnSpec`
///   flips to [`Volatility::Volatile`] (and this note is revisited) when a real
///   seeded RNG lands and the function actually draws.
///
/// A genuinely *implemented* volatile function instead carries
/// [`Volatility::Volatile`] directly in its [`FnSpec`] and is **not** listed
/// here — e.g. `CELL` (partial `info_type` support; see `func_cell`), whose
/// `FnSpec` is `Volatile`, so `is_volatile("CELL")` is `true` via the flag.
static VOLATILE_NAMES: &[&str] = &[
    "NOW",
    "TODAY",
    "RAND",
    "RANDBETWEEN",
    "RANDARRAY",
    "OFFSET",
    "INDIRECT",
    "INFO",
];

/// Look up a function by name (ASCII case-insensitive), or `None` if it is not
/// implemented.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static FnSpec> {
    REGISTRY
        .iter()
        .find(|spec| spec.name.eq_ignore_ascii_case(name))
}

/// Whether a call to `name` must mark its cell volatile — true for an
/// implemented volatile function *or* a name in the internal volatile-name list (volatile but
/// unimplemented). ASCII case-insensitive.
#[must_use]
pub fn is_volatile(name: &str) -> bool {
    if let Some(spec) = lookup(name)
        && spec.volatility == Volatility::Volatile
    {
        return true;
    }
    VOLATILE_NAMES.iter().any(|v| v.eq_ignore_ascii_case(name))
}

/// Functions that can return a **top-level `Value::Array`** — i.e. a result
/// that *spills* into neighbouring cells (M2 lanes 3–4).
///
/// # Single source of truth (RFC-0014 R4)
/// This is the authoritative list of functions that return an array **as their
/// own top-level result** (a genuine spill). The engine's parallel-recalc
/// **spill gate** (RFC-0014 §5) consults it via [`returns_array`] to keep any
/// spill-capable workbook on the serial path.
///
/// **Scope — read before editing.** List a function here iff its `eval` can
/// produce a top-level `Value::Array`. Do **NOT** add functions that merely
/// *broadcast over* an array argument (`IF`/`NOT`/`ISBLANK`/aggregators): at a
/// cell's top level they are called in scalar context (`eval` gates broadcast
/// on `array_arg_ctx`, so a scalar-context call never materialises an array),
/// and once every actual array *source* is gated out, they have no array to
/// broadcast — adding them would needlessly collapse the gate's open rate. The
/// discriminator is "returns an array", not "touches an array".
///
/// Kept explicit three ways: (1) a test asserts every entry is a registered
/// function; (2) a structural test scans each `func_*` module for the
/// array-construction helpers (`arrayshape`/`dynarray` `spill`, `INDEX`'s
/// `Array::new` returns) and fails if a returner is missing; (3) the engine's
/// always-on staged-array backstop (RFC-0014 R3) falls back to serial if a
/// *runtime* array escapes an unlisted function — so an omission costs
/// parallelism, never correctness.
///
/// Reference *transformers* (`OFFSET`/`INDIRECT`/`ANCHORARRAY`) are handled
/// separately by the engine (`refx::is_ref_returning`); they are not xl-fn
/// registry functions and are not listed here.
pub static ARRAY_RETURNING_FNS: &[&str] = &[
    // M2 lane 3a — reshape / generate.
    "SEQUENCE",
    "RANDARRAY",
    "TOCOL",
    "TOROW",
    "VSTACK",
    "HSTACK",
    "TAKE",
    "DROP",
    "EXPAND",
    "WRAPROWS",
    "WRAPCOLS",
    "CHOOSEROWS",
    "CHOOSECOLS",
    // M2 lane 3b — lookup / sort / filter.
    "XLOOKUP",
    "FILTER",
    "SORT",
    "SORTBY",
    "UNIQUE",
    // Tier-0 legacy: INDEX(range, 0, c) / (range, r, 0) return a whole
    // column/row as a spilled array (func_index).
    "INDEX",
];

/// Whether a call to `name` can return a top-level `Value::Array` that spills
/// (ASCII case-insensitive) — the [`ARRAY_RETURNING_FNS`] membership test the
/// engine's parallel-recalc spill gate uses (RFC-0014).
#[must_use]
pub fn returns_array(name: &str) -> bool {
    ARRAY_RETURNING_FNS
        .iter()
        .any(|f| f.eq_ignore_ascii_case(name))
}
