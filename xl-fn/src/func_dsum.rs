//! `DSUM` — sum a database column over the records matching a criteria range.
//!
//! `DSUM(database, field, criteria)` is the first of Excel's **database (`D*`)
//! functions**. It shares two building blocks with the rest of the library:
//! the `database`/`criteria` rectangles are materialised through the positional
//! [`CallArgs::for_each_row`] walk (like `SUMIF`/`VLOOKUP`), and every
//! per-cell condition is compiled and tested by the shared criteria
//! mini-language ([`crate::criteria`], owned by `SUMIF`/`COUNTIF`) — so
//! comparisons (`">100"`), numeric/text equality, wildcards and the
//! oracle-deferred corners all behave identically to `SUMIF`. The
//! header-resolution and criteria-matching helpers here are deliberately kept
//! free of anything `DSUM`-specific so `DAVERAGE`/`DCOUNT`/`DGET`/… can reuse
//! them later (this task implements only `DSUM`).
//!
//! # Provenance
//! Behavior contract: `docs/specs/DSUM.md`, which cites the Microsoft Learn
//! `DSUM` page (field = a quoted column label *or* a 1-based column number;
//! `database` first row = column labels; criteria range = labels row + one or
//! more condition rows; conditions in the **same row** are AND-ed) and the
//! Microsoft "Filter by using advanced criteria" page (the identical
//! criteria-range model where conditions in **different rows** are OR-ed),
//! both verified 2026-07-11. All value coercion/parsing is deferred to
//! `xl-value` and `xl-fn`'s existing criteria engine — this module never
//! re-implements number parsing or comparison.
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
//!   record) — **OXP-148 RESOLVED** by RUN-2026-07-11-oracle01, which observed
//!   `DSUM(A1:C4,"Sales",F1:F2)` = `0` (and the field-by-index twin
//!   `DSUM(A1:C4,3,F1:F2)` = `0`) with the criteria header `"Bonus"` absent
//!   from the `Name`/`Age`/`Sales` database — i.e. the condition is neither
//!   dropped (which would match everything → 600) nor mis-assigned; it simply
//!   excludes every record.
//! - **Sum** the `field` cell of every matching record; only numeric field
//!   cells contribute (text/blank/logical skipped, exactly `SUM`'s
//!   `RangeAggregate` rule), an error field cell propagates, and an empty match
//!   set sums to `0`.
//!
//! # OXP-148 — partly RESOLVED, partly still deferred
//! - **RESOLVED (RUN-2026-07-11-oracle01)** — a non-blank criterion under a
//!   non-blank **text** label matching no `database` header: its condition row
//!   is unsatisfiable (matches no record), per the oracle observations above.
//! - **Still deferred (loud `#UNSUPPORTED!`, never a guess)** — a non-blank
//!   criterion under a **blank or non-text** label. That sub-case was not
//!   probed by this run; which `database` column such a condition constrains
//!   is still unpinned, so rather than silently drop or mis-assign it the whole
//!   call defers.
//! - The criteria engine's own deferrals (non-numeric ordering operands,
//!   blank/`Array`/`Ref` criteria *values*, error criteria propagation) reach
//!   `DSUM` unchanged via [`criteria::Matcher::short_circuit`].
//!
//! # Recalc sentinels in a criteria-tested record cell propagate
//! See [`record_matches`]'s docs for the fix: a Recalc sentinel
//! ([`xl_value::ErrorKind::is_recalc_sentinel`]) in a criteria-tested record
//! cell now propagates (kind preserved) out of the whole `DSUM` call, instead
//! of being silently treated as "condition fails" the way a genuine Excel
//! error there is. Genuine errors are unaffected.

use xl_value::{CoercionMode, ErrorKind, NumericArg, Value, coerce_number_arg};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::criteria::{self, Matcher};

/// Evaluate a `DSUM(database, field, criteria)` call. See the module docs.
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

    // Sum the field cell of every matching record (records = rows below the
    // header row). Only numeric field cells contribute; an error propagates.
    let mut acc = 0.0_f64;
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
            NumericArg::Number(n) => acc += n,
            NumericArg::Skip => {}
            NumericArg::Error(k) => return Value::Error(k),
        }
    }
    Value::number(acc)
}

/// Resolve `field` to a 0-based column index, or an early-return `Value` (an
/// error) on failure.
///
/// `Number` → 1-based column index (truncated toward zero); `Text` → a
/// case-insensitive header-label match; an error propagates; anything else is
/// `#VALUE!`. Reusable verbatim by the other `D*` functions.
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
                        // database header matches no record — the oracle saw
                        // `DSUM(A1:C4,"Sales",F1:F2)` = 0 with header "Bonus"
                        // absent from the database. Mark the row unsatisfiable
                        // and skip compiling this (uncolumned) criterion.
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
/// `Ok(false)`, [`criteria::matches`]'s "excluded" default for a genuine
/// Excel error in a criteria-tested record cell, is correct only when the
/// cell genuinely holds that error in Excel too. A Recalc sentinel
/// ([`xl_value::ErrorKind::is_recalc_sentinel`]) means Recalc never actually
/// evaluated the cell, so whether it would satisfy its condition in real
/// Excel is unknowable — reporting "excluded" would launder that gap into a
/// possibly-wrong aggregate. Per Recalc Principle 2, `sentinel_of` is
/// checked on each condition's cell *before* `criteria::matches`, in the
/// existing row-then-column scan order (rows are OR-ed top to bottom; within
/// a satisfiable row, conditions are AND-ed left to right and already
/// short-circuit on the first failing condition — a genuine mismatch earlier
/// in that scan still short-circuits before a later sentinel is ever
/// reached, so this never over-propagates). An `unsatisfiable` row's cells
/// are never checked — that row is excluded regardless of their contents,
/// same as before. The first sentinel found is returned as `Err` (kind
/// preserved); the caller ([`eval`]) propagates it exactly like a matched
/// field cell's `coerce_number_arg` error already does. `criteria::matches`'s
/// own `bool` signature is unchanged.
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
        // Sum Score where Age = 25 → Bob(200) + Dave(400) = 600.
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("Score")),
                criteria1(txt("Age"), num(25.0)),
            ],
        );
        assert_eq!(got, num(600.0));
    }

    #[test]
    fn field_by_1_based_index() {
        // field = 3 → the "Score" column; Age = 25 → 600 (same subset).
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(num(3.0)),
                criteria1(txt("Age"), num(25.0)),
            ],
        );
        assert_eq!(got, num(600.0));
    }

    #[test]
    fn field_index_truncates_toward_zero() {
        // 2.9 → column 2 ("Age"); Age = 25 → 25 + 25 = 50.
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(num(2.9)),
                criteria1(txt("Age"), num(25.0)),
            ],
        );
        assert_eq!(got, num(50.0));
    }

    #[test]
    fn comparison_criterion() {
        // Sum Score where Age > 25 → Alice(100) + Carol(50) = 150.
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("Score")),
                criteria1(txt("Age"), txt(">25")),
            ],
        );
        assert_eq!(got, num(150.0));
    }

    #[test]
    fn comparison_on_field_column() {
        // Sum Score where Score > 100 → Bob(200) + Dave(400) = 600.
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("Score")),
                criteria1(txt("Score"), txt(">100")),
            ],
        );
        assert_eq!(got, num(600.0));
    }

    #[test]
    fn no_match_sums_to_zero() {
        // No record has Age = 99 → 0.
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("Score")),
                criteria1(txt("Age"), num(99.0)),
            ],
        );
        assert_eq!(got, num(0.0));
    }

    #[test]
    fn and_within_criteria_row() {
        // Two labels in one row → AND: Age = 25 AND Score > 300 → only Dave(400).
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
        // Carol(50) + Alice(100) = 150.
        let criteria = Rect {
            rows: 3,
            cols: 1,
            data: vec![txt("Age"), num(40.0), num(30.0)],
        };
        let got = eval_direct(eval, vec![database(), Scalar(txt("Score")), criteria]);
        assert_eq!(got, num(150.0));
    }

    #[test]
    fn field_label_is_case_insensitive() {
        // "score" matches header "Score".
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
    fn text_field_cells_skip_like_sum() {
        // Sum the text "Name" column where Age = 25: both matched field cells
        // are text → contribute 0.
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("Name")),
                criteria1(txt("Age"), num(25.0)),
            ],
        );
        assert_eq!(got, num(0.0));
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
    fn unmappable_text_criteria_label_matches_nothing() {
        // OXP-148 RESOLVED (RUN-2026-07-11-oracle01): a condition under a
        // non-blank text label matching no database header excludes every
        // record → 0 (not a dropped condition → 750, not #UNSUPPORTED!).
        let got = eval_direct(
            eval,
            vec![
                database(),
                Scalar(txt("Score")),
                criteria1(txt("Weight"), num(25.0)),
            ],
        );
        assert_eq!(got, num(0.0));
    }

    #[test]
    fn oracle_unmatched_criteria_header_is_zero() {
        // Replicates OXP-148 / RUN-2026-07-11-oracle01: database headers
        // Name/Age/Sales, criteria label "Bonus" (absent) with ">0".
        //   =DSUM(A1:C4,"Sales",F1:F2) = 0  and  =DSUM(A1:C4,3,F1:F2) = 0.
        let db = || Rect {
            rows: 4,
            cols: 3,
            data: vec![
                txt("Name"),
                txt("Age"),
                txt("Sales"),
                txt("amy"),
                num(30.0),
                num(100.0),
                txt("ben"),
                num(40.0),
                num(200.0),
                txt("cid"),
                num(50.0),
                num(300.0),
            ],
        };
        let by_name = eval_direct(
            eval,
            vec![
                db(),
                Scalar(txt("Sales")),
                criteria1(txt("Bonus"), txt(">0")),
            ],
        );
        assert_eq!(by_name, num(0.0));
        let by_index = eval_direct(
            eval,
            vec![db(), Scalar(num(3.0)), criteria1(txt("Bonus"), txt(">0"))],
        );
        assert_eq!(by_index, num(0.0));
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
        // A criteria-engine short-circuit (Unsupported) surfaces through DSUM.
        // Date/currency ordering operands are now resolved and PARSE (OXP-101/162,
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
        // Bob's Age cell is a Recalc sentinel; the "Age > 20" condition tests
        // it, so it must propagate (kind preserved) rather than exclude Bob.
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
        // "row doesn't match, record excluded" behavior — Bob's #DIV/0! Age
        // simply fails the "Age > 20" condition, not propagated.
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
        assert_eq!(got, num(150.0)); // Alice(100) + Carol(50); Bob excluded
    }

    #[test]
    fn sentinel_after_a_definite_mismatch_in_same_row_does_not_over_propagate() {
        // Two AND-ed conditions in one row: Age ">5" (checked first) fails
        // for the sole record BEFORE the Score sentinel cell would ever be
        // scanned — the row's AND short-circuits exactly as before, so the
        // record is simply excluded, never propagated.
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
        assert_eq!(got, num(0.0));
    }

    #[test]
    fn sentinel_in_matched_field_cell_still_propagates_preexisting() {
        // A sentinel in the aggregated *field* cell of a MATCHED record
        // already propagates via `coerce_number_arg` (pre-existing behavior,
        // unrelated to this fix — verified unbroken here).
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
        // The field cell of a record that does NOT match is never read at
        // all (record excluded before the field cell is even looked at), so
        // a sentinel sitting there is genuinely irrelevant and must not
        // surface.
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
        assert_eq!(got, num(0.0));
    }
}
