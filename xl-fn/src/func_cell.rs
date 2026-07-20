//! `CELL` — information about the formatting, location, or contents of a cell.
//! **Volatile.** Partial `info_type` support: the geometry/content types that
//! are unambiguous from the current `CallArgs` reference channels are served;
//! every other (valid-but-unimplemented, or invalid) `info_type` refuses
//! **loudly** with `#UNSUPPORTED!` (Recalc Principle 2 — never silently
//! wrong; the task brief permits partial support with a loud decline).
//!
//! # Provenance
//! Behavior contract: `docs/specs/CELL.md`, which cites the Microsoft Learn
//! CELL function page
//! (`https://support.microsoft.com/en-us/office/cell-function-51bd39a5-f338-4dbe-a33f-955d67c2b2cf`).
//!
//! # Volatility
//! `CELL` is in Excel's volatile set (`implementation-plan.md` §2): it can
//! report position/format that changes without its precedents changing, so it
//! recomputes every recalc. Registered with [`Volatility::Volatile`]; it is
//! therefore **removed** from `registry::VOLATILE_NAMES` (the unregistered-
//! volatile fallback list) — the `FnSpec` flag now carries its volatility
//! directly. See `registry::is_volatile`.
//!
//! # info_types implemented (all read the reference's **top-left** cell)
//! - **`"row"`** — the 1-based row of the reference's first cell
//!   ([`RefRect::row`] + 1) (CELL.md §row).
//! - **`"col"`** — the 1-based column ([`RefRect::col`] + 1) (CELL.md §col).
//! - **`"type"`** — `"b"` if the top-left cell is blank, `"l"` if it holds
//!   text (a label), `"v"` otherwise (any other value) (CELL.md §type). This is
//!   fully documented and covers the blank case.
//! - **`"contents"`** — the value of the top-left cell, **for a non-blank
//!   cell** (CELL.md §contents). A **blank** cell's contents (Excel's general
//!   empty-ref-yields-`0` rule vs an empty result) is unpinned → deferred to
//!   OXP-218.
//!
//! # Deferred loudly (`#UNSUPPORTED!`)
//! - **Every other `info_type`** — `"address"`, `"color"`, `"filename"`,
//!   `"format"`, `"parentheses"`, `"prefix"`, `"protect"`, `"width"` — needs
//!   formatting/workbook/UI state (or, for `"address"`, the sheet-qualified
//!   name that `arg_ref_extent` does not carry), and an **unrecognized**
//!   `info_type` string's error is itself unpinned. All refuse loudly.
//! - **An omitted `reference`** — Excel's "the cell that was last changed" is
//!   undefined in a headless recalc, so the 1-argument form refuses (never
//!   guesses the calling cell).
//! - **A non-single-area `reference`** (an array constant, a computed scalar, a
//!   union) — no resolvable geometry ([`CallArgs::arg_ref_extent`] → `None`).
//!
//! # Oracle experiments needed (OXP-218)
//! Queued as **OXP-218**: (1) the exact string `"address"` produces (absolute
//! style, sheet/book qualification); (2) blank-cell `"contents"` (`0` vs
//! empty); (3) what an omitted `reference` resolves to; (4) an invalid
//! `info_type`'s error kind. Until pinned, all four refuse loudly.
//!
//! The OXP-218 run (RUN-2026-07-16-oracle01) authored only the omitted-reference
//! case — `=CELL("row")` in `A1` → `1` — and it stays **inconclusive** for a
//! headless engine: a single data point in `A1` cannot distinguish "the formula's
//! own cell" from Excel's documented "the last cell that was changed" (a
//! session/UI-state notion that is irreproducible in a batch recalc); both read
//! `1` here. So the omitted `reference` form **remains deferred** (no code
//! change), and the richer probes (`"address"`, blank `"contents"`, blank
//! `"type"`, `"width"`, `"format"`, an invalid `info_type`) still need a
//! scaffolded re-author before anything can be implemented.

use xl_value::{ErrorKind, Value, to_text};

use crate::args::{ArgShape, CallArgs};
use crate::context::EvalContext;

/// Classify a cell value for `CELL("type", …)`: `"b"` blank, `"l"` text/label,
/// `"v"` any other value (number, logical, error).
fn cell_type(v: &Value) -> &'static str {
    match v {
        Value::Blank => "b",
        Value::Text(_) => "l",
        _ => "v",
    }
}

/// Evaluate a `CELL(info_type, [reference])` call. See the module docs for the
/// supported `info_type`s and the loud deferrals.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let info = match to_text(&args.eval_scalar(0)) {
        Ok(t) => t.as_str().to_ascii_lowercase(),
        Err(k) => return Value::Error(k),
    };

    // The reference is required. The omitted form ("the cell that was last
    // changed") is undefined in a headless recalc → refuse loudly (OXP-218),
    // never guess the calling cell.
    let has_ref = args.count() >= 2 && args.shape(1) != ArgShape::Omitted;
    if !has_ref {
        return Value::Error(ErrorKind::Unsupported);
    }
    // Geometry of the reference's rectangle (position + extent). `None` for a
    // non-single-area reference (array constant, computed scalar, union).
    let Some(rect) = args.arg_ref_extent(1) else {
        return Value::Error(ErrorKind::Unsupported);
    };

    match info.as_str() {
        // Position of the reference's top-left cell (1-based).
        "row" => Value::number(f64::from(rect.row) + 1.0),
        "col" => Value::number(f64::from(rect.col) + 1.0),
        // Contents / type of the top-left cell (read at its absolute position).
        "contents" => match args.cell_at(1, rect.row, rect.col) {
            // Blank contents (0 vs empty) is unpinned → defer (OXP-218).
            Some(v) if v.is_blank() => Value::Error(ErrorKind::Unsupported),
            Some(v) => v,
            None => Value::Error(ErrorKind::Unsupported),
        },
        "type" => match args.cell_at(1, rect.row, rect.col) {
            Some(v) => Value::text(cell_type(&v)),
            None => Value::Error(ErrorKind::Unsupported),
        },
        // Every other info_type (valid-but-unimplemented or invalid) refuses
        // loudly — see module docs / OXP-218.
        _ => Value::Error(ErrorKind::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::RefRect;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};

    fn rect_at(row: u32, col: u32) -> RefRect {
        RefRect {
            row,
            col,
            height: 1,
            width: 1,
        }
    }

    #[test]
    fn row_returns_one_based_row() {
        // Reference at 0-based (4, 2) → row 5.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("row")), Reference(rect_at(4, 2))]),
            num(5.0)
        );
    }

    #[test]
    fn col_returns_one_based_col() {
        // 0-based col 2 → column 3.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("col")), Reference(rect_at(4, 2))]),
            num(3.0)
        );
    }

    #[test]
    fn info_type_is_case_insensitive() {
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("ROW")), Reference(rect_at(0, 0))]),
            num(1.0)
        );
    }

    #[test]
    fn contents_returns_top_left_value() {
        let rect = rect_at(2, 1);
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("contents")),
                    RefCells {
                        rect,
                        cells: vec![(2, 1, num(42.0))],
                    },
                ],
            ),
            num(42.0)
        );
    }

    #[test]
    fn contents_of_text_cell() {
        let rect = rect_at(0, 0);
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("contents")),
                    RefCells {
                        rect,
                        cells: vec![(0, 0, txt("hi"))],
                    },
                ],
            ),
            txt("hi")
        );
    }

    #[test]
    fn contents_of_blank_is_deferred() {
        // OXP-218: blank contents (0 vs empty) is unpinned.
        let rect = rect_at(0, 0);
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("contents")),
                    RefCells {
                        rect,
                        cells: vec![], // (0,0) inside rect but unlisted → Blank
                    },
                ],
            ),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn type_of_number_is_v() {
        let rect = rect_at(0, 0);
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("type")),
                    RefCells {
                        rect,
                        cells: vec![(0, 0, num(3.0))],
                    },
                ],
            ),
            txt("v")
        );
    }

    #[test]
    fn type_of_text_is_l() {
        let rect = rect_at(0, 0);
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("type")),
                    RefCells {
                        rect,
                        cells: vec![(0, 0, txt("label"))],
                    },
                ],
            ),
            txt("l")
        );
    }

    #[test]
    fn type_of_blank_is_b() {
        let rect = rect_at(0, 0);
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(txt("type")),
                    RefCells {
                        rect,
                        cells: vec![],
                    },
                ],
            ),
            txt("b")
        );
    }

    #[test]
    fn unsupported_info_type_defers_loudly() {
        for it in ["address", "format", "width", "color", "protect", "bogus"] {
            assert_eq!(
                eval_direct(eval, vec![Scalar(txt(it)), Reference(rect_at(0, 0))]),
                Value::Error(ErrorKind::Unsupported),
                "info_type {it:?} should defer loudly"
            );
        }
    }

    #[test]
    fn omitted_reference_defers_loudly() {
        // The 1-arg "last changed cell" form is undefined in a headless recalc.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("row"))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn non_single_area_reference_defers_loudly() {
        // A computed scalar (not a reference) has no arg_ref_extent → defer.
        assert_eq!(
            eval_direct(eval, vec![Scalar(txt("row")), Scalar(num(5.0))]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn error_info_type_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![
                    Scalar(Value::Error(ErrorKind::Div0)),
                    Reference(rect_at(0, 0))
                ],
            ),
            Value::Error(ErrorKind::Div0)
        );
    }
}
