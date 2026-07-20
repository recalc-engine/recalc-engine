//! `DAVERAGE` — average a database column over the records matching a criteria
//! range. The **database-function twin of `DSUM`** (`func_dsum.rs`): identical
//! `(database, field, criteria)` argument structure, identical field selection
//! (a quoted column label *or* a 1-based column number), and the identical
//! criteria-matching machinery (`database`/`criteria` rectangles materialised
//! via [`CallArgs::for_each_row`], per-cell conditions compiled and tested by
//! the shared [`crate::criteria`] mini-language owned by `SUMIF`/`COUNTIF`).
//! Where `DSUM` **sums** the matched `field` cells, `DAVERAGE` takes their
//! **arithmetic mean**.
//!
//! # Provenance
//! Behavior contract: `docs/specs/DAVERAGE.md`, which cites the Microsoft Learn
//! `DAVERAGE` page ("Averages the values in a field (column) of records in a
//! list or database that match conditions you specify"; field = a quoted column
//! label *or* a 1-based column number; `database` first row = column labels;
//! criteria range = labels row + one or more condition rows) verified
//! 2026-07-11, and reuses the `DSUM` behavior contract wholesale for the
//! database/field/criteria model (`docs/specs/DSUM.md`, incl. the Microsoft
//! "Filter by using advanced criteria" AND-within-a-row / OR-across-rows model).
//! All value coercion/parsing is deferred to `xl-value` and `xl-fn`'s existing
//! criteria engine — this module never re-implements number parsing or
//! comparison.
//!
//! # Relationship to `DSUM` (shared machinery, minimally duplicated)
//! `DSUM`'s database/criteria/field helpers (`buffer_rows`, `resolve_field`,
//! `find_header`, `compile_conditions`, `record_matches`, the `ConditionRow`
//! type) are **module-private** to `func_dsum.rs` (`fn`, not `pub(crate)`), so
//! they cannot be imported here without promoting their visibility — which would
//! require editing `func_dsum.rs`, out of scope for this task. They are
//! therefore duplicated **verbatim** below (same logic, same OXP-148 handling),
//! flagged here so a future refactor can hoist them into a shared `db` module
//! reused by `DAVERAGE`/`DCOUNT`/`DGET`/… The only genuinely `DAVERAGE`-specific
//! code is the final aggregation (mean instead of sum, plus the empty-set rule).
//!
//! # Semantics implemented (never guessed)
//! - **`database` (arg 0)** — a rectangular range whose **first row** is the
//!   column labels (headers); the remaining rows are records. Materialised via
//!   [`CallArgs::for_each_row`]; an unbounded whole-column/row range (the dense
//!   walk refuses it) returns `#UNSUPPORTED!` (never a guessed extent).
//! - **`field` (arg 1)** — evaluated in scalar context:
//!   - a **`Number`** is a 1-based column index (truncated toward zero, per
//!     Excel's column-position reading); out of `1..=ncols`, non-finite → `#VALUE!`;
//!   - **`Text`** is a column *label*, matched case-insensitively against a
//!     `Text` header; no match → `#VALUE!`;
//!   - an error `field` propagates; any other type → `#VALUE!`.
//! - **`criteria` (arg 2)** — a rectangular range: its **first row** is labels
//!   (each mapped case-insensitively to a `database` header), the rows below
//!   are condition rows. A record matches when **any** condition row matches
//!   (OR across rows); a condition row matches when **all** its non-blank
//!   cells match (AND across columns). A **blank** criterion cell is "no
//!   condition"; an all-blank condition row therefore matches every record
//!   (the documented empty-criteria reading). Each non-blank criterion is
//!   compiled once via [`criteria::parse`] and tested via [`criteria::matches`].
//!   A non-blank criterion under a non-blank **text** label that matches **no**
//!   `database` header makes its condition row **unsatisfiable** (matches no
//!   record) — **OXP-148 RESOLVED** by RUN-2026-07-11-oracle01 (see `DSUM`).
//! - **Average** the `field` cell of every matching record: only numeric field
//!   cells enter the mean (text/blank/logical skipped, exactly `SUM`/`AVERAGE`'s
//!   `RangeAggregate` rule), an error field cell propagates, and an **empty set
//!   of numeric matched values** → `#DIV/0!` (the average of nothing — the same
//!   empty-set rule as `AVERAGE`, `docs/specs/AVERAGE.md` §4; *not* `0` like
//!   `DSUM`).
//!
//! # OXP-148 — partly RESOLVED, partly still deferred (identical to `DSUM`)
//! - **RESOLVED (RUN-2026-07-11-oracle01)** — a non-blank criterion under a
//!   non-blank **text** label matching no `database` header: its condition row
//!   is unsatisfiable (matches no record). `DAVERAGE` mirrors `DSUM`'s resolved
//!   path exactly; the only difference is that an unsatisfiable-everywhere
//!   criteria set yields `#DIV/0!` here (no numeric values averaged) rather than
//!   `DSUM`'s `0`.
//! - **Still deferred (loud `#UNSUPPORTED!`, never a guess)** — a non-blank
//!   criterion under a **blank or non-text** label (unprobed; column unpinned).
//! - The criteria engine's own deferrals (non-numeric ordering operands,
//!   blank/`Array`/`Ref` criteria *values*, error criteria propagation) reach
//!   `DAVERAGE` unchanged via [`criteria::Matcher::short_circuit`].
//!
//! # Recalc sentinels in a criteria-tested record cell propagate
//! See [`record_matches`]'s docs for the fix (mirrors `DSUM`'s identical fix
//! verbatim): a Recalc sentinel ([`xl_value::ErrorKind::is_recalc_sentinel`])
//! in a criteria-tested record cell now propagates (kind preserved) out of
//! the whole `DAVERAGE` call, instead of being silently treated as
//! "condition fails" the way a genuine Excel error there is. Genuine errors
//! are unaffected.

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::criteria::{self, Matcher};

/// Evaluate a `DAVERAGE(database, field, criteria)` call. See the module docs.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // database (arg 0): materialise the rectangle. The dense walk refuses an
    // unbounded whole-column/row range → #UNSUPPORTED! (never a guessed extent).
    let database = match buffer_rows(args, 0) {
        Ok(rows) => rows,
        Err(k) => return Value::Error(k),
    };
    let headers: &[Value] = database.first().map(Vec::as_slice).unwrap_or(&[]);
    let ncols = database.iter().map(Vec::len).max().unwrap_or(0);

    // field (arg 1): a 1-based column index (Number) or a header label (Text).
    let field_col = match resolve_field(&args.eval_scalar(1), headers, ncols) {
        Ok(c) => c,
        Err(v) => return v,
    };

    // criteria (arg 2): first row = labels, rows below = condition rows.
    let criteria_rows = match buffer_rows(args, 2) {
        Ok(rows) => rows,
        Err(k) => return Value::Error(k),
    };
    let conditions = match compile_conditions(&criteria_rows, headers) {
        Ok(c) => c,
        Err(v) => return v,
    };

    // Average the field cell of every matching record (records = rows below the
    // header row). Only numeric field cells enter the mean; an error propagates.
    let mut acc = 0.0_f64;
    let mut count: usize = 0;
    for record in database.iter().skip(1) {
        match record_matches(record, &conditions) {
            Ok(true) => {}
            Ok(false) => continue,
            // A Recalc sentinel in a criteria-tested record cell propagates
            // (kind preserved) — see `record_matches`'s docs.
            Err(k) => return Value::Error(k),
        }
        let cell = record.get(field_col).unwrap_or(&Value::Blank);
        match coerce_number_arg(cell, CoercionMode::RangeAggregate) {
            NumericArg::Number(n) => {
                acc += n;
                count += 1;
            }
            NumericArg::Skip => {}
            NumericArg::Error(k) => return Value::Error(k),
        }
    }
    // Average of nothing → #DIV/0! (same empty-set rule as AVERAGE, never 0).
    if count == 0 {
        return Value::Error(ErrorKind::Div0);
    }
    Value::number(acc / count as f64)
}

// ---------------------------------------------------------------------------
// Shared database/criteria/field machinery — duplicated verbatim from `DSUM`
// (`func_dsum.rs`), whose copies are module-private `fn`s and thus not
// importable. See the module docs for the flag; a future refactor should hoist
// these into a shared crate-internal module. Kept byte-for-byte identical to
// `DSUM` so the two functions can never diverge on criteria semantics.
// ---------------------------------------------------------------------------

/// Resolve `field` to a 0-based column index, or an early-return `Value` (an
/// error) on failure.
///
/// `Number` → 1-based column index (truncated toward zero); `Text` → a
/// case-insensitive header-label match; an error propagates; anything else is
/// `#VALUE!`.
fn resolve_field(field: &Value, headers: &[Value], ncols: usize) -> Result<usize, Value> {
    match field {
        Value::Error(k) => Err(Value::Error(*k)),
        Value::Number(n) => {
            // 1-based column position; Excel reads the integer part.
            if !n.is_finite() {
                return Err(Value::Error(ErrorKind::Value));
            }
            let idx = n.trunc();
            if idx < 1.0 || idx > ncols as f64 {
                return Err(Value::Error(ErrorKind::Value));
            }
            Ok(idx as usize - 1)
        }
        Value::Text(t) => find_header(headers, t.as_str()).ok_or(Value::Error(ErrorKind::Value)),
        // BC-6 (RFC-0012): a lambda is not a valid field selector — `#VALUE!`,
        // the same invalid-selector error as Bool/Blank/Array/Ref.
        Value::Lambda(_) => Err(Value::Error(ErrorKind::Value)),
        // Bool / Blank / Array / Ref are not a valid field selector.
        _ => Err(Value::Error(ErrorKind::Value)),
    }
}

/// Case-insensitive position of a `Text` header equal to `label`.
///
/// Only `Text` headers participate (a numeric header is never matched by a
/// text label). Case folding uses Unicode lowercasing, matching the criteria
/// engine's comparison convention.
fn find_header(headers: &[Value], label: &str) -> Option<usize> {
    headers
        .iter()
        .position(|h| matches!(h, Value::Text(t) if ci_eq(t.as_str(), label)))
}

/// Case-insensitive string equality (Unicode-lowercase fold).
fn ci_eq(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// One compiled condition row: the non-blank criteria of a single criteria-range
/// row, each as `(database column, compiled matcher)`, plus an `unsatisfiable`
/// flag. An empty `conds` with `unsatisfiable == false` is an all-blank row (no
/// conditions → matches every record). `unsatisfiable` is set when a criterion
/// in the row sits under a non-blank text label that matches no `database`
/// header (OXP-148 RESOLVED): such a row matches no record.
struct ConditionRow {
    conds: Vec<(usize, Matcher)>,
    unsatisfiable: bool,
}

/// Compile the criteria range into OR-ed condition rows, resolving header
/// mapping and short-circuits **once** up front (mirroring `SUMIF`'s
/// compile-criteria-once contract). Returns an early `Value` on a deferral:
/// - a non-blank criterion under an unmappable label → `#UNSUPPORTED!` (OXP-148);
/// - a criteria engine short-circuit (`#UNSUPPORTED!` / propagated error).
///
/// The first criteria row is the labels row; rows below it are conditions.
fn compile_conditions(
    criteria_rows: &[Vec<Value>],
    headers: &[Value],
) -> Result<Vec<ConditionRow>, Value> {
    let labels: &[Value] = criteria_rows.first().map(Vec::as_slice).unwrap_or(&[]);

    let mut compiled: Vec<ConditionRow> = Vec::new();
    for row in criteria_rows.iter().skip(1) {
        let mut conds: Vec<(usize, Matcher)> = Vec::new();
        let mut unsatisfiable = false;
        for (c, cell) in row.iter().enumerate() {
            // A blank criterion cell imposes no condition on its column.
            if cell.is_blank() {
                continue;
            }
            // Map this criteria column to a database column via its label.
            let label = labels.get(c).unwrap_or(&Value::Blank);
            let col = match label {
                Value::Text(t) if !t.as_str().is_empty() => {
                    match find_header(headers, t.as_str()) {
                        Some(col) => col,
                        // OXP-148 RESOLVED (RUN-2026-07-11-oracle01): a real
                        // condition under a non-blank text label matching no
                        // database header matches no record. Mark the row
                        // unsatisfiable and skip compiling this criterion.
                        None => {
                            unsatisfiable = true;
                            continue;
                        }
                    }
                }
                // BC-6 (RFC-0012): a lambda label is refused explicitly
                // (`#UNSUPPORTED!`), matching the non-text-label disposition.
                Value::Lambda(_) => return Err(Value::Error(ErrorKind::Unsupported)),
                // OXP-148 (still deferred): a condition under a blank / non-text
                // label cannot be mapped to a database column, and this sub-case
                // was not probed — defer rather than guess.
                _ => return Err(Value::Error(ErrorKind::Unsupported)),
            };
            let matcher = criteria::parse(cell);
            if let Some(short) = matcher.short_circuit() {
                return Err(short);
            }
            conds.push((col, matcher));
        }
        compiled.push(ConditionRow {
            conds,
            unsatisfiable,
        });
    }
    Ok(compiled)
}

/// Whether a record matches the compiled criteria: **any** condition row (OR),
/// where a row matches iff it is satisfiable **and all** its conditions hold
/// (AND). With no condition rows nothing matches; an all-blank condition row
/// (empty `conds`, satisfiable) matches vacuously; an `unsatisfiable` row (a
/// criterion under an unmatched text label, OXP-148) matches no record.
///
/// # Recalc sentinels (`#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`) propagate
/// Identical fix and rationale to `DSUM`'s own `record_matches` (see
/// `func_dsum.rs`, which this module duplicates verbatim): a Recalc sentinel
/// ([`xl_value::ErrorKind::is_recalc_sentinel`]) in a criteria-tested record
/// cell is checked via `sentinel_of` *before* `criteria::matches`, in the
/// existing row-then-column scan order, and returned as `Err` (kind
/// preserved) instead of being silently treated as "condition fails" the way
/// a genuine Excel error there is. A genuine mismatch earlier in the AND scan
/// still short-circuits before a later sentinel is ever reached (no
/// over-propagation); an `unsatisfiable` row's cells are never checked.
fn record_matches(record: &[Value], conditions: &[ConditionRow]) -> Result<bool, ErrorKind> {
    for row in conditions {
        if row.unsatisfiable {
            continue;
        }
        let mut row_matches = true;
        for (col, matcher) in &row.conds {
            let cell = record.get(*col).unwrap_or(&Value::Blank);
            if let Some(k) = criteria::refuse_cell(matcher, cell) {
                return Err(k);
            }
            if !criteria::matches(matcher, cell) {
                row_matches = false;
                break;
            }
        }
        if row_matches {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Buffer an argument's rectangle row-by-row into an owned grid using the dense
/// [`CallArgs::for_each_row`] walk, so an unbounded whole-column/row range
/// surfaces as `Err(Unsupported)` (the caller then returns `#UNSUPPORTED!`).
fn buffer_rows(args: &mut dyn CallArgs, index: usize) -> Result<Vec<Vec<Value>>, ErrorKind> {
    use std::ops::ControlFlow;
    let mut rows: Vec<Vec<Value>> = Vec::new();
    args.for_each_row(index, &mut |row| {
        rows.push(row.to_vec());
        ControlFlow::Continue(())
    })?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    // A 3-column database: headers ["Name","Age","Score"] then 4 records.
    //   Alice 30 100
    //   Bob   25 200
    //   Carol 40  50
    //   Dave  25 400
    fn database() -> crate::test_support::TestArg {
        Rect {
            rows: 5,
            cols: 3,
            data: vec![
                txt("Name"),
                txt("Age"),
                txt("Score"),
                txt("Alice"),
                num(30.0),
                num(100.0),
                txt("Bob"),
                num(25.0),
                num(200.0),
                txt("Carol"),
                num(40.0),
                num(50.0),
                txt("Dave"),
                num(25.0),
                num(400.0),
            ],
        }
    }

    // A 1-column criteria block: label + one condition row.
    fn criteria1(label: Value, cond: Value) -> crate::test_support::TestArg {
        Rect {
            rows: 2,
            cols: 1,
            data: vec![label, cond],
        }
    }

    #[test]
    fn numeric_criterion_label_field() {
        // Average Score where Age = 25 → mean(Bob 200, Dave 400) = 300.
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("Score")),
                criteria1(txt("Age"), num(25.0)),
            ],
        );
        assert_eq!(got, num(300.0));
    }

    #[test]
    fn field_by_1_based_index() {
        // field = 3 → the "Score" column; Age = 25 → mean(200, 400) = 300.
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(num(3.0)),
                criteria1(txt("Age"), num(25.0)),
            ],
        );
        assert_eq!(got, num(300.0));
    }

    #[test]
    fn field_index_truncates_toward_zero() {
        // 2.9 → column 2 ("Age"); Age = 25 → mean(25, 25) = 25.
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(num(2.9)),
                criteria1(txt("Age"), num(25.0)),
            ],
        );
        assert_eq!(got, num(25.0));
    }

    #[test]
    fn comparison_criterion() {
        // Average Score where Age > 25 → mean(Alice 100, Carol 50) = 75.
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("Score")),
                criteria1(txt("Age"), txt(">25")),
            ],
        );
        assert_eq!(got, num(75.0));
    }

    #[test]
    fn comparison_on_field_column() {
        // Average Score where Score > 100 → mean(Bob 200, Dave 400) = 300.
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("Score")),
                criteria1(txt("Score"), txt(">100")),
            ],
        );
        assert_eq!(got, num(300.0));
    }

    #[test]
    fn no_match_is_div0() {
        // No record has Age = 99 → no numeric values averaged → #DIV/0!.
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("Score")),
                criteria1(txt("Age"), num(99.0)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Div0));
    }

    #[test]
    fn and_within_criteria_row() {
        // Two labels in one row → AND: Age = 25 AND Score > 300 → only Dave(400).
        // A single matched numeric value → mean = that value.
        let criteria = Rect {
            rows: 2,
            cols: 2,
            data: vec![txt("Age"), txt("Score"), num(25.0), txt(">300")],
        };
        let got = eval_direct(eval, vec![database(), Scalar(txt("Score")), criteria]);
        assert_eq!(got, num(400.0));
    }

    #[test]
    fn or_across_criteria_rows() {
        // Two condition rows on Age → OR: Age = 40 OR Age = 30.
        // mean(Carol 50, Alice 100) = 75.
        let criteria = Rect {
            rows: 3,
            cols: 1,
            data: vec![txt("Age"), num(40.0), num(30.0)],
        };
        let got = eval_direct(eval, vec![database(), Scalar(txt("Score")), criteria]);
        assert_eq!(got, num(75.0));
    }

    #[test]
    fn field_label_is_case_insensitive() {
        // "score" matches header "Score"; Age = 40 → only Carol(50).
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("score")),
                criteria1(txt("Age"), num(40.0)),
            ],
        );
        assert_eq!(got, num(50.0));
    }

    #[test]
    fn field_label_not_found_is_value_error() {
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("Height")),
                criteria1(txt("Age"), num(25.0)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Value));
    }

    #[test]
    fn field_index_out_of_range_is_value_error() {
        // Only 3 columns; index 4 → #VALUE!.
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(num(4.0)),
                criteria1(txt("Age"), num(25.0)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Value));
        // index 0 (below 1-based range) → #VALUE!.
        let got0 = eval_direct(
            eval,
            vec![
                database(),
                Scalar(num(0.0)),
                criteria1(txt("Age"), num(25.0)),
            ],
        );
        assert_eq!(got0, Value::Error(ErrorKind::Value));
    }

    #[test]
    fn text_field_cells_skipped_yield_div0() {
        // Average the text "Name" column where Age = 25: both matched field
        // cells are text → skipped → no numeric values → #DIV/0! (not 0).
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("Name")),
                criteria1(txt("Age"), num(25.0)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Div0));
    }

    #[test]
    fn text_field_cells_skipped_in_mixed_column() {
        // A field column mixing numbers and a text cell among matched records:
        // only the numeric cells enter the mean (text skipped, not counted as 0).
        //   headers: ["Grp","Val"]; rows: (x, 10), (x, "n/a"), (x, 20).
        // Criterion Grp = "x" matches all three; mean(10, 20) = 15 (text skipped).
        let db = Rect {
            rows: 4,
            cols: 2,
            data: vec![
                txt("Grp"),
                txt("Val"),
                txt("x"),
                num(10.0),
                txt("x"),
                txt("n/a"),
                txt("x"),
                num(20.0),
            ],
        };
        let got = eval_direct(
            eval,
            vec![db, Scalar(txt("Val")), criteria1(txt("Grp"), txt("x"))],
        );
        assert_eq!(got, num(15.0));
    }

    #[test]
    fn unbounded_database_is_unsupported() {
        // A whole-column database (dense walk refuses) → #UNSUPPORTED!.
        let got = eval_direct(
            eval,
            vec![
                Unbounded(vec![txt("Age"), num(25.0)]),
                Scalar(num(1.0)),
                criteria1(txt("Age"), num(25.0)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Unsupported));
    }

    #[test]
    fn unmappable_text_criteria_label_is_div0() {
        // OXP-148 RESOLVED (RUN-2026-07-11-oracle01): a condition under a
        // non-blank text label matching no database header excludes every
        // record → no numeric values averaged → #DIV/0! (DAVERAGE's empty-set
        // rule; the DSUM twin returns 0). The condition is neither dropped nor
        // mis-assigned.
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("Score")),
                criteria1(txt("Weight"), num(25.0)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Div0));
    }

    #[test]
    fn blank_criteria_label_with_condition_still_defers() {
        // OXP-148 remainder (NOT probed by this run): a non-blank criterion
        // under a blank label cannot be mapped to a column → #UNSUPPORTED!.
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("Score")),
                criteria1(Value::Blank, num(25.0)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Unsupported));
    }

    #[test]
    fn criteria_engine_short_circuit_propagates() {
        // A criteria-engine short-circuit (Unsupported) surfaces through DAVERAGE,
        // mirroring DSUM. Date/currency ordering operands now PARSE (OXP-101/162,
        // RUN-2026-07-11-oracle01), so this uses the still-deferred **non-ASCII**
        // text ordering operand `">ä"` (OXP-031 HELD locale-collation defer).
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("Score")),
                criteria1(txt("Name"), txt(">ä")),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Unsupported));
    }

    // ---- Recalc sentinels propagate (Principle 2 fix) ---------------------

    #[test]
    fn sentinel_in_criteria_tested_record_cell_propagates_kind_preserved() {
        for k in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            let db = Rect {
                rows: 4,
                cols: 3,
                data: vec![
                    txt("Name"),
                    txt("Age"),
                    txt("Score"),
                    txt("Alice"),
                    num(30.0),
                    num(100.0),
                    txt("Bob"),
                    Value::Error(k),
                    num(200.0),
                    txt("Carol"),
                    num(40.0),
                    num(50.0),
                ],
            };
            let got = eval_direct(
                eval,
                vec![db, Scalar(txt("Score")), criteria1(txt("Age"), txt(">20"))],
            );
            assert_eq!(got, Value::Error(k), "{k:?} should propagate");
        }
    }

    #[test]
    fn genuine_error_in_criteria_tested_record_cell_still_excludes_record_unchanged() {
        // Control: a genuine error in the tested cell keeps the exact prior
        // "row doesn't match, record excluded" behavior.
        let db = Rect {
            rows: 4,
            cols: 3,
            data: vec![
                txt("Name"),
                txt("Age"),
                txt("Score"),
                txt("Alice"),
                num(30.0),
                num(100.0),
                txt("Bob"),
                Value::Error(ErrorKind::Div0),
                num(200.0),
                txt("Carol"),
                num(40.0),
                num(50.0),
            ],
        };
        let got = eval_direct(
            eval,
            vec![db, Scalar(txt("Score")), criteria1(txt("Age"), txt(">20"))],
        );
        assert_eq!(got, num(75.0)); // mean(Alice 100, Carol 50); Bob excluded
    }

    #[test]
    fn sentinel_after_a_definite_mismatch_in_same_row_does_not_over_propagate() {
        let db = Rect {
            rows: 2,
            cols: 3,
            data: vec![
                txt("Name"),
                txt("Age"),
                txt("Score"),
                txt("Solo"),
                num(1.0),                             // fails ">5" first
                Value::Error(ErrorKind::Unsupported), // never reached
            ],
        };
        let criteria = Rect {
            rows: 2,
            cols: 2,
            data: vec![txt("Age"), txt("Score"), txt(">5"), txt(">0")],
        };
        let got = eval_direct(eval, vec![db, Scalar(txt("Score")), criteria]);
        assert_eq!(got, Value::Error(ErrorKind::Div0)); // empty match set
    }

    #[test]
    fn sentinel_in_matched_field_cell_still_propagates_preexisting() {
        let db = Rect {
            rows: 2,
            cols: 2,
            data: vec![
                txt("Name"),
                txt("Score"),
                txt("Alice"),
                Value::Error(ErrorKind::Blocked),
            ],
        };
        let criteria = Rect {
            rows: 2,
            cols: 1,
            data: vec![txt("Name"), txt("Alice")],
        };
        let got = eval_direct(eval, vec![db, Scalar(txt("Score")), criteria]);
        assert_eq!(got, Value::Error(ErrorKind::Blocked));
    }

    #[test]
    fn sentinel_in_field_cell_of_unmatched_record_stays_ignored() {
        let db = Rect {
            rows: 2,
            cols: 2,
            data: vec![
                txt("Name"),
                txt("Score"),
                txt("Alice"),
                Value::Error(ErrorKind::Unsupported),
            ],
        };
        let criteria = Rect {
            rows: 2,
            cols: 1,
            data: vec![txt("Name"), txt("Nobody")], // matches no record
        };
        let got = eval_direct(eval, vec![db, Scalar(txt("Score")), criteria]);
        assert_eq!(got, Value::Error(ErrorKind::Div0)); // empty match set
    }
}
