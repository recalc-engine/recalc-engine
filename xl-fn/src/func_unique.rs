//! `UNIQUE` — return the distinct rows (or columns) of `array`.
//!
//! # Provenance
//! Microsoft support page "UNIQUE function"
//! (<https://support.microsoft.com/en-us/office/unique-function-c5ab87fd-30a3-4ce9-9d1a-40204fb85e1e>),
//! fetched 2026-07-15. Clean-room from the page's unambiguous prose. The page
//! does **not** pin the equality used for "unique" (text case-sensitivity, the
//! `Blank`↔`0`/`""`/`FALSE` morph), so this module **computes only where every
//! plausible equality agrees and refuses loudly where they diverge** (the Recalc design rules
//! Principle 2 — the same discipline `func_switch` applies to its match). Refused
//! edges are queued in `docs/plans/2026-07-15-lane3b-probe-needed.md`.
//!
//! # Signature (page verbatim)
//! `UNIQUE(array, [by_col], [exactly_once])` — 1..=3 args. `by_col` (default
//! FALSE): "compare rows against each other and return the unique rows" vs TRUE
//! "compare columns … return the unique columns". `exactly_once` (default
//! FALSE): "return all distinct rows or columns" vs TRUE "that occur exactly
//! once".
//!
//! # Semantics implemented
//! - Distinct **rows** (`by_col = FALSE`) or **columns** (`by_col = TRUE`),
//!   emitted in first-occurrence order; `exactly_once = TRUE` keeps only lines
//!   whose key occurs exactly once.
//! - Equality is per-cell and type-exact for the unambiguous cases: two numbers
//!   equal iff bit-equal (signed zero normalised), two logicals iff equal, and
//!   `Number`/`Text`/`Bool`/`Blank` never cross-match (matching `compare`'s
//!   `Number < Text < Bool` ranks). ASCII text is grouped case-insensitively.
//! - A **data error** anywhere propagates leftmost-first (row-major) — an
//!   error's participation in dedup equality is unpinned (Principle 2).
//!
//! # Refused (see the probe doc)
//! - **Text case collision**: if **any** two byte-distinct ASCII texts in the
//!   array share a case-folded form (e.g. `"USA"` and `"usa"`), case-sensitive vs
//!   case-insensitive dedup can diverge → `#UNSUPPORTED!`. The check is **global
//!   and conservative** — it may refuse an input whose unique set happens to be
//!   identical under both readings, but it never computes a divergent one. Text
//!   with no such collision computes.
//! - **`Blank`↔`0`/`""`/`FALSE` morph**: when a compared position holds both a
//!   `Blank` and a `0`/`""`/`FALSE`, strict-vs-morph dedup diverge →
//!   `#UNSUPPORTED!`. Otherwise `Blank` is its own distinct value.
//! - **Non-ASCII text** (collation unpinned — OXP-031) and unresolved
//!   **array/ref** cells → `#UNSUPPORTED!`.
//! - **Empty result** (only reachable with `exactly_once = TRUE`) → `#CALC!`
//!   (assumed, mirroring FILTER's documented empty behavior).
//! - **Whole-column/row inputs** (`A:A`): the dense walk refuses → `#UNSUPPORTED!`.

use std::collections::HashMap;

use xl_value::{ErrorKind, Value, to_bool};

use crate::args::CallArgs;
use crate::context::EvalContext;
use crate::dynarray::{first_error, materialize, spill};

/// Evaluate a `UNIQUE(...)` call. See the module docs for semantics/provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    let data = match materialize(args, 0) {
        Ok(g) => g,
        Err(k) => return Value::Error(k),
    };
    if data.height == 0 || data.width == 0 {
        return Value::Error(ErrorKind::Calc);
    }

    // A data error's dedup-equality is unpinned → propagate leftmost-first.
    if let Some(k) = first_error(&data) {
        return Value::Error(k);
    }

    // by_col (arg 1) + exactly_once (arg 2): FALSE defaults; errors propagate.
    let by_col = match bool_arg(args, 1) {
        Ok(b) => b,
        Err(k) => return Value::Error(k),
    };
    let exactly_once = match bool_arg(args, 2) {
        Ok(b) => b,
        Err(k) => return Value::Error(k),
    };

    // Lines to dedup: the rows, or the columns (by_col).
    let lines: Vec<Vec<Value>> = if by_col {
        (0..data.width).map(|c| data.column(c)).collect()
    } else {
        data.rows.clone()
    };

    let kept = match dedup_lines(&lines, exactly_once) {
        Ok(k) => k,
        Err(k) => return Value::Error(k),
    };
    if kept.is_empty() {
        return Value::Error(ErrorKind::Calc);
    }

    // Reassemble.
    if by_col {
        let height = data.height;
        let width = kept.len();
        let mut flat: Vec<Value> = Vec::with_capacity(height * width);
        for r in 0..height {
            for &c in &kept {
                flat.push(data.rows[r][c].clone());
            }
        }
        spill(height, width, flat)
    } else {
        let height = kept.len();
        let width = data.width;
        let mut flat: Vec<Value> = Vec::with_capacity(height * width);
        for &r in &kept {
            flat.extend(lines[r].iter().cloned());
        }
        spill(height, width, flat)
    }
}

/// Coerce an optional boolean flag argument (default FALSE when absent).
fn bool_arg(args: &mut dyn CallArgs, index: usize) -> Result<bool, ErrorKind> {
    if args.count() > index {
        to_bool(&args.eval_scalar(index))
    } else {
        Ok(false)
    }
}

/// A per-cell grouping key. Distinct variants never cross-match (mirroring
/// `compare`'s `Number < Text < Bool` ranks); `Blank` is its own value unless a
/// morph divergence is detected upstream.
#[derive(Clone, PartialEq, Eq, Hash)]
enum CellKey {
    Num(u64),
    Txt(String),
    Boolean(bool),
    Blank,
}

/// Normalise a finite `f64` to an equality-stable bit pattern (`-0.0` folds to
/// `+0.0`, which `compare` treats as equal). Values are finite by the `Value`
/// invariant, so there is no NaN to canonicalise.
fn norm_bits(n: f64) -> u64 {
    if n == 0.0 {
        0.0f64.to_bits()
    } else {
        n.to_bits()
    }
}

/// Dedup `lines` (each of equal length), returning the kept line indices in
/// first-occurrence order, or an error to surface. Refuses the genuinely
/// ambiguous equality zones (see module docs).
fn dedup_lines(lines: &[Vec<Value>], exactly_once: bool) -> Result<Vec<usize>, ErrorKind> {
    let l = lines.first().map(Vec::len).unwrap_or(0);

    // Blank-morph divergence: a position holding both a `Blank` and a
    // `0`/`""`/`FALSE` would dedup differently under strict vs morph equality.
    for p in 0..l {
        let mut has_blank = false;
        let mut has_morph = false;
        for line in lines {
            match &line[p] {
                Value::Blank => has_blank = true,
                Value::Number(n) if *n == 0.0 => has_morph = true,
                Value::Text(t) if t.as_str().is_empty() => has_morph = true,
                Value::Bool(false) => has_morph = true,
                Value::Number(_)
                | Value::Text(_)
                | Value::Bool(true)
                | Value::Error(_)
                | Value::Array(_)
                | Value::Ref(_)
                | Value::Lambda(_) => {}
            }
        }
        if has_blank && has_morph {
            return Err(ErrorKind::Unsupported);
        }
    }

    // Per-cell keys, with case-collision detection over ASCII text.
    let mut lower_seen: HashMap<String, String> = HashMap::new();
    let mut keys: Vec<Vec<CellKey>> = Vec::with_capacity(lines.len());
    for line in lines {
        let mut key: Vec<CellKey> = Vec::with_capacity(l);
        for v in line {
            match v {
                Value::Number(n) => key.push(CellKey::Num(norm_bits(*n))),
                Value::Bool(b) => key.push(CellKey::Boolean(*b)),
                Value::Blank => key.push(CellKey::Blank),
                Value::Text(t) => {
                    let s = t.as_str();
                    if !s.is_ascii() {
                        return Err(ErrorKind::Unsupported);
                    }
                    let lower = s.to_ascii_lowercase();
                    match lower_seen.get(&lower) {
                        Some(prev) if prev != s => return Err(ErrorKind::Unsupported),
                        Some(_) => {}
                        None => {
                            lower_seen.insert(lower.clone(), s.to_string());
                        }
                    }
                    key.push(CellKey::Txt(lower));
                }
                // Errors were already propagated by the caller's `first_error`
                // scan; kept enumerated so a new `Value` variant forces a
                // decision here (no bare `_`).
                Value::Error(k) => return Err(*k),
                Value::Array(_) | Value::Ref(_) | Value::Lambda(_) => {
                    return Err(ErrorKind::Unsupported);
                }
            }
        }
        keys.push(key);
    }

    // First-occurrence grouping with counts.
    let mut index_of: HashMap<Vec<CellKey>, usize> = HashMap::new();
    let mut order: Vec<(usize, usize)> = Vec::new(); // (first line idx, count)
    for (idx, key) in keys.into_iter().enumerate() {
        match index_of.get(&key) {
            Some(&pos) => order[pos].1 += 1,
            None => {
                index_of.insert(key, order.len());
                order.push((idx, 1));
            }
        }
    }

    Ok(order
        .into_iter()
        .filter(|&(_, count)| !exactly_once || count == 1)
        .map(|(idx, _)| idx)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::dynarray::spill;
    use crate::test_support::{TestArg::*, eval_direct, num, txt};
    use xl_value::{ErrorKind, Value};

    #[test]
    fn unique_rows_basic() {
        let got = eval_direct(
            eval,
            vec![Range(vec![num(1.0), num(2.0), num(2.0), num(3.0)])],
        );
        assert_eq!(got, spill(3, 1, vec![num(1.0), num(2.0), num(3.0)]));
    }

    #[test]
    fn exactly_once_keeps_singletons() {
        let got = eval_direct(
            eval,
            vec![
                Range(vec![num(1.0), num(2.0), num(2.0), num(3.0)]),
                Scalar(Value::Bool(false)),
                Scalar(Value::Bool(true)),
            ],
        );
        assert_eq!(got, spill(2, 1, vec![num(1.0), num(3.0)]));
    }

    #[test]
    fn exactly_once_all_duplicated_is_calc() {
        let got = eval_direct(
            eval,
            vec![
                Range(vec![num(1.0), num(1.0)]),
                Scalar(Value::Bool(false)),
                Scalar(Value::Bool(true)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Calc));
    }

    #[test]
    fn unique_columns_by_col() {
        // 1×4 row {1,2,2,3}, by_col TRUE → unique columns {1,2,3}.
        let got = eval_direct(
            eval,
            vec![
                Array(vec![num(1.0), num(2.0), num(2.0), num(3.0)]),
                Scalar(Value::Bool(true)),
            ],
        );
        assert_eq!(got, spill(1, 3, vec![num(1.0), num(2.0), num(3.0)]));
    }

    #[test]
    fn text_case_insensitive_when_byte_identical() {
        let got = eval_direct(eval, vec![Range(vec![txt("a"), txt("a"), txt("b")])]);
        assert_eq!(got, spill(2, 1, vec![txt("a"), txt("b")]));
    }

    #[test]
    fn text_case_collision_refused() {
        // "a" vs "A": case-sensitive and case-insensitive dedup diverge → refuse.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![txt("a"), txt("A")])]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn blank_morph_refused() {
        // A column with a 0 and a Blank: strict vs morph dedup diverge → refuse.
        assert_eq!(
            eval_direct(eval, vec![Range(vec![num(0.0), Value::Blank])]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn blank_distinct_when_no_morph_partner() {
        // Blanks with no 0/""/FALSE present: dedup together, distinct from 5.
        let got = eval_direct(
            eval,
            vec![Range(vec![Value::Blank, Value::Blank, num(5.0)])],
        );
        assert_eq!(got, spill(2, 1, vec![Value::Blank, num(5.0)]));
    }

    #[test]
    fn distinct_case_text_computes() {
        // "abc" and "abd" differ by more than case → no collision → compute.
        let got = eval_direct(eval, vec![Range(vec![txt("abc"), txt("abd"), txt("abc")])]);
        assert_eq!(got, spill(2, 1, vec![txt("abc"), txt("abd")]));
    }

    #[test]
    fn cross_type_never_matches() {
        // Number 5 and Text "5" are distinct rows.
        let got = eval_direct(eval, vec![Range(vec![num(5.0), txt("5")])]);
        assert_eq!(got, spill(2, 1, vec![num(5.0), txt("5")]));
    }

    #[test]
    fn multi_column_row_dedup() {
        // 3×2 rows: (1,2),(1,2),(3,4) → unique rows (1,2),(3,4).
        let got = eval_direct(
            eval,
            vec![Rect {
                rows: 3,
                cols: 2,
                data: vec![num(1.0), num(2.0), num(1.0), num(2.0), num(3.0), num(4.0)],
            }],
        );
        assert_eq!(
            got,
            spill(2, 2, vec![num(1.0), num(2.0), num(3.0), num(4.0)])
        );
    }

    #[test]
    fn error_propagates() {
        assert_eq!(
            eval_direct(
                eval,
                vec![Range(vec![num(1.0), Value::Error(ErrorKind::Div0)])]
            ),
            Value::Error(ErrorKind::Div0)
        );
    }

    #[test]
    fn non_ascii_text_refused() {
        assert_eq!(
            eval_direct(eval, vec![Range(vec![txt("ä"), txt("z")])]),
            Value::Error(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn whole_column_refused() {
        assert_eq!(
            eval_direct(eval, vec![Unbounded(vec![num(1.0), num(2.0)])]),
            Value::Error(ErrorKind::Unsupported)
        );
    }
}
