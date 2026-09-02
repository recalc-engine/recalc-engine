//! `VLOOKUP` — search the first column of a table for a key and return a value
//! from the same row in a chosen column.
//!
//! # Provenance
//! Behavior contract: `docs/specs/VLOOKUP.md` (which cites the Microsoft
//! `support.microsoft.com` VLOOKUP page, verified 2026-07-05). Value ordering
//! (approximate mode) and non-`Blank`-involved equality (exact mode) are
//! deferred to `xl-value` ([`compare`] / `values_equal`), so VLOOKUP inherits
//! Excel's case-insensitive text rule and cross-type ordering (`Number < Text
//! < Bool`) automatically and stays correct as that contract is refined.
//! **Exact-mode** equality where a `Blank` is involved is *not* the raw
//! `values_equal` call — `Blank`'s operator-level morphs
//! (`Blank ↔ Number(0)`/`Text("")`/`Bool(false)`) were never pinned for LOOKUP
//! semantics and one is oracle-contradicted, so it goes through the scoped,
//! provenance-documented [`crate::lookup::exact_eq`] shared with HLOOKUP and
//! MATCH instead (see [`exact_search`] and that module's docs).
//!
//! # Semantics implemented (spec bullets in parentheses)
//! - `VLOOKUP(lookup_value, table_array, col_index_num, [range_lookup])`.
//!   `range_lookup` defaults to `TRUE` (approximate match) when omitted
//!   (VLOOKUP.md §Signature).
//! - **Exact match** (`range_lookup = FALSE`): a linear top-to-bottom scan of
//!   `table_array`'s first column; the **first** cell equal to `lookup_value`
//!   (equality via [`crate::lookup::exact_eq`]) wins; no match → `#N/A`
//!   (VLOOKUP.md §2, §4).
//! - **Approximate match** (`range_lookup = TRUE`/omitted): Excel assumes the
//!   first column is sorted ascending and runs a **binary search** (VLOOKUP.md
//!   §3). The exact probe order — floor midpoint, immediate return on an exact
//!   hit, otherwise settle on the last `<=` probe — is pinned to Excel by
//!   **OXP-088** (RUN-2026-07-11-oracle01); on unsorted data it reproduces
//!   Excel's algorithm-order-dependent answer rather than a re-sorted "sensible"
//!   one (see [`approx_search`]). `lookup_value` smaller than every key → `#N/A`
//!   (VLOOKUP.md §4).
//! - Returns column `col_index_num` (1-based) of the matched row.
//!   `col_index_num < 1` → `#VALUE!`; `col_index_num >` table width → `#REF!`
//!   (VLOOKUP.md §5, §6). An error value sitting in the returned cell of the
//!   matched row propagates as VLOOKUP's result (it is simply returned).
//! - Error in `lookup_value`, `col_index_num`, or `range_lookup` propagates
//!   immediately (VLOOKUP.md §Error behavior).
//!
//! # Whole-column `table_array` (`A:D`) — used-extent iteration (RFC 0001)
//! A whole-**column** `table_array` (`A:D`, `A:A`) is supported via the
//! used-extent row walk ([`for_each_row_or_used`]): the table is buffered from
//! its **populated** rows (in ascending order, blanks filled within the column
//! span), which the engine's sparse cell store enumerates in `O(populated)`
//! rather than scanning 1,048,576 rows. VLOOKUP returns a *value*, not a
//! position, so compacting away unpopulated rows is invisible to its search:
//! - **Exact match** — a blank (absent) row never equals a non-blank
//!   `lookup_value`, so omitting absent rows cannot change the exact-match
//!   answer for a non-blank key.
//! - **Approximate match** — on a sorted first column, omitting absent (blank)
//!   interior rows does not change the largest-`<=` answer (**OXP-104**; the
//!   precise Excel treatment of blank cells interspersed in a sorted whole
//!   column is not claimed as verified).
//!
//! A **`Blank` `lookup_value`** over a whole column is answerable in **exact**
//! mode (OXP-165, `RUN-2026-07-11-oracle01`): a blank key matches a `0`-valued
//! first-column cell, so `VLOOKUP(<blank>, A:B, 2, FALSE)` over `A = [0, "",
//! <blank>, 5]` returns `B1` (the row of the leading `0`-cell) — exactly what
//! [`crate::lookup::exact_eq`] returns for that pair (no override needed; see
//! that module for the full pinned/deferred pair table, including why a
//! `Blank` key does **not** match a populated `""`/`FALSE` cell or a truly
//! blank one, unlike the raw `values_equal` contract). This resolves
//! **OXP-104's clean half**; a populated match is trusted only when no
//! *absent* row precedes it (the populated rows are contiguous from the top
//! through the match), else it defers. A `Blank`-key scan that **completes
//! with no match** returns `#N/A` (L2-A, `docs/l2-refusal-decomposition.md`):
//! **OXP-104 H3** pins `VLOOKUP(<blank>, A:B, 2, FALSE)` over the whole
//! column `{1, 2, <truly blank>, 4}` to `#N/A`, and OXP-104's blank-vs-blank
//! NoMatch pin means the absent rows the walk cannot see are confirmed
//! no-matches — so the used-extent answer equals the bounded walk's. Any
//! unpinned `Blank` pair (`""`/`FALSE`/`0`-key-vs-blank-cell; OXP-171 queued)
//! encountered mid-scan still Defers first, bit-for-bit unchanged. An
//! ENTIRELY empty whole column with a `Blank` key still defers (never
//! probed). In **approximate** mode a `Blank` key
//! defers (`#UNSUPPORTED!`) uniformly — not only over a whole column, but
//! over any `table_array` shape (bounded or whole-column), and likewise for
//! a `Blank` *cell* the binary search actually probes, not only the key —
//! because [`approx_search`] refuses to let a `Blank` reach `compare`'s
//! operator-only-pinned morph for this unpinned lookup ordering (OXP-104's
//! interspersed-blank ordering remains unverified); see that function's docs.
//!
//! A whole-**row** `table_array` (unbounded columns) is still `#UNSUPPORTED!` —
//! the row-oriented used-extent walk does not serve it (see RFC 0001 §3). Named
//! and cross-sheet whole-column tables resolve through the same path. Bounded
//! ranges (`A1:D100`), array constants, and a single-cell `table_array` remain
//! fully supported with unchanged behavior.
//!
//! # Oracle-resolved (RUN-2026-07-11-oracle01)
//! - **`OXP-088`** — the *exact* answers approximate mode returns on **unsorted**
//!   data are now pinned to Excel. Excel's binary search uses a **floor**
//!   midpoint, returns immediately on an exact hit (so duplicate/unsorted keys
//!   resolve to whichever equal key the probe lands on — e.g. lookup `2` over
//!   `[5,2,8,2,1,9,3]` returns row 4), otherwise settles on the last `<=` probe
//!   (`hi`) and yields `#N/A` only when every probe stayed above `lookup_value`.
//!   An error cell **at a probe midpoint does not abort** the search — Excel
//!   moves into the upper half past it (observed H9: `VLOOKUP(3, D1:E3, 2, TRUE)`
//!   over a column whose middle cell is `#DIV/0!` returns `e-3`). See
//!   [`approx_search`].
//! - **`OXP-089`** — exact-mode wildcards are honored (`*` = any run, `?` = one
//!   char, `~` escapes `*`/`?`/`~`), matched case-insensitively against each
//!   first-column **text** cell with the **first** match winning; a non-integer
//!   `col_index_num` is **truncated toward zero** (`2.9`→col 2, `2.1`→col 2). See
//!   [`exact_search`].

use std::cmp::Ordering;
use std::ops::ControlFlow;

use xl_value::{ErrorKind, Value, compare, to_number};

use crate::args::{CallArgs, for_each_row_or_used};
use crate::context::EvalContext;
use crate::lookup::{LookupEq, approx_touches_blank, exact_eq};

/// Evaluate a `VLOOKUP(...)` call. See the module docs for the semantics and
/// their spec provenance.
pub(crate) fn eval(_ctx: &EvalContext, args: &mut dyn CallArgs) -> Value {
    // --- lookup_value (arg 0): evaluated in scalar context; error propagates.
    let lookup = args.eval_scalar(0);
    if let Value::Error(k) = lookup {
        return Value::Error(k);
    }

    // --- col_index_num (arg 2): numeric-coerced; error propagates.
    let col_index = match to_number(&args.eval_scalar(2)) {
        Ok(n) => n,
        Err(k) => return Value::Error(k),
    };
    // OXP-089 (RUN-2026-07-11-oracle01): a non-integer col_index_num is
    // truncated **toward zero** (`2.9`→2, `2.1`→2 both read column B).
    // `f64::trunc` is exactly that operation; the bound checks and the `as usize`
    // index below then all operate on the truncated value.
    let col_index = col_index.trunc();
    // col_index_num < 1 → #VALUE! (VLOOKUP.md §6). Checked independently of the
    // table width, which governs the #REF! case below.
    if col_index < 1.0 {
        return Value::Error(ErrorKind::Value);
    }

    // --- range_lookup (arg 3, optional): defaults to TRUE (approximate) when
    // the argument is absent. When present it is boolean-coerced via xl-value;
    // an error propagates.
    let approximate = if args.count() >= 4 {
        match xl_value::to_bool(&args.eval_scalar(3)) {
            Ok(b) => b,
            Err(k) => return Value::Error(k),
        }
    } else {
        true
    };

    // --- table_array (arg 1): buffer the rectangle positionally. The dense walk
    // surfaces blanks at their column positions; a whole-column range falls back
    // to the used-extent walk (populated rows only). A whole-row range or an
    // unresolvable range still errors → #UNSUPPORTED! (see the whole-column note).
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut rels: Vec<u32> = Vec::new();
    let used_extent = match for_each_row_or_used(args, 1, &mut |rel, row| {
        rels.push(rel);
        rows.push(row.to_vec());
        ControlFlow::Continue(())
    }) {
        Ok(used) => used,
        Err(k) => return Value::Error(k),
    };
    // A Blank lookup_value in APPROXIMATE mode used to defer here only for a
    // whole column (OXP-165/OXP-104), leaving a bounded `table_array` to fall
    // through to `approx_search` and silently apply `compare`'s
    // operator-only-pinned Blank morphs to unpinned lookup ordering. That gap
    // is now closed uniformly (bounded and whole-column alike, key or a
    // probed cell) by the touch-a-Blank defer inside `approx_search` itself
    // — see that function's docs — so no pre-check is needed here.

    // Table width governs the #REF! bound. A rectangle has a uniform width; take
    // the widest row defensively for any ragged array constant.
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    // col_index_num > number of columns → #REF! (VLOOKUP.md §5).
    if (col_index as usize) > width {
        return Value::Error(ErrorKind::Ref);
    }
    let col = col_index as usize; // 1-based, validated 1..=width.

    // Empty table (no rows): nothing to match → #N/A. On a whole column, a
    // Blank key still defers — kept bit-for-bit (L2-A condition 2). Unlike the
    // populated no-match case below (pinned directly by OXP-104 H3), an
    // ENTIRELY empty column was never probed: #N/A here would rest on
    // composing the blank-vs-blank NoMatch pair alone, so the conservative
    // defer stands until an OXP observes the empty-column shape.
    if rows.is_empty() {
        return if used_extent && matches!(lookup, Value::Blank) {
            Value::Error(ErrorKind::Unsupported)
        } else {
            Value::Error(ErrorKind::Na)
        };
    }

    let matched_row = if approximate {
        match approx_search(&rows, &lookup) {
            Ok(Some(i)) => i,
            Ok(None) => return Value::Error(ErrorKind::Na),
            Err(k) => return Value::Error(k),
        }
    } else {
        match exact_search(&rows, &lookup) {
            Ok(Some(i)) => {
                // Pre-existing, extra-conservative guard (unchanged, out of
                // scope for this fix): on a whole column, trust a populated
                // Blank-key match only when no absent row precedes it (the
                // populated rows are contiguous from the top through the
                // match, `rels[i] == i`); otherwise defer. Not required by
                // OXP-104 itself — `exact_eq` already proves a Blank key
                // never matches *any* blank cell, absent or populated, so an
                // earlier absent row cannot hide an extra match — but kept as
                // a belt-and-braces margin.
                if used_extent && matches!(lookup, Value::Blank) && rels[i] as usize != i {
                    return Value::Error(ErrorKind::Unsupported);
                }
                i
            }
            // No-match completion → #N/A, on the used-extent path too (L2-A).
            // For a Blank key this is pinned, not composed: OXP-104 H3
            // (RUN-2026-07-11-oracle01) observed `VLOOKUP(C1, A:B, 2, FALSE)`
            // with C1 blank over the whole column {1, 2, <truly blank>, 4} →
            // `#N/A`. Reaching this arm means every populated first-column
            // cell was a confirmed NoMatch (any unpinned pair Defers → `Err`
            // above, preserved bit-for-bit), and OXP-104 pins that a Blank key
            // matches no truly-blank (absent) cell — so the invisible absent
            // rows are confirmed no-matches, and the completed scan's #N/A
            // equals the already-pinned bounded walk's answer. (Until this
            // change the arm carried an extra-conservative Blank-key defer,
            // which was the actual refusal behind the L2-A corpus shape —
            // blank template-row keys over `WC_Underlyings!$A:$B`.)
            Ok(None) => return Value::Error(ErrorKind::Na),
            Err(k) => return Value::Error(k),
        }
    };

    // Return the value in the chosen column of the matched row. An error sitting
    // there is returned as-is, i.e. propagates (VLOOKUP.md §Error behavior).
    rows[matched_row]
        .get(col - 1)
        .cloned()
        .unwrap_or(Value::Blank)
}

/// First column of row `i`, or `Blank` for a (defensively handled) short row.
fn first_col(rows: &[Vec<Value>], i: usize) -> &Value {
    rows[i].first().unwrap_or(&Value::Blank)
}

/// Exact-match scan (`range_lookup = FALSE`): the first row whose first-column
/// cell equals `lookup` (case-insensitive text / cross-type per
/// [`crate::lookup::exact_eq`]) in top-to-bottom order. `Ok(None)` if no row
/// matches.
///
/// **Wildcards** (OXP-089, RUN-2026-07-11-oracle01): when `lookup` is text
/// containing an (unescaped) `*` or `?`, it is treated as an Excel wildcard
/// pattern — `*` matches any run of characters, `?` any single character, `~`
/// escapes the next `*`/`?`/`~` to a literal — matched **case-insensitively**
/// against the whole first-column text of each row; the first matching row wins.
/// Non-text first-column cells cannot match a wildcard pattern. Outside the
/// wildcard path, equality is [`crate::lookup::exact_eq`] — an ordinary
/// `values_equal` call except for `Blank`-involved pairs, which are scoped to
/// the pinned OXP-165/OXP-104 facts and defer (`#UNSUPPORTED!`) rather than
/// silently match on every other unpinned pairing (see that module's docs). An
/// error cell in the search column still propagates as before.
fn exact_search(rows: &[Vec<Value>], lookup: &Value) -> Result<Option<usize>, ErrorKind> {
    if let Value::Text(t) = lookup
        && t.as_str().contains(['*', '?'])
    {
        let pattern = parse_pattern(t.as_str());
        for i in 0..rows.len() {
            if let Value::Text(cell) = first_col(rows, i)
                && wildcard_match(&pattern, cell.as_str())
            {
                return Ok(Some(i));
            }
        }
        return Ok(None);
    }
    for i in 0..rows.len() {
        match exact_eq(first_col(rows, i), lookup) {
            Ok(LookupEq::Match) => return Ok(Some(i)),
            Ok(LookupEq::NoMatch) => {}
            Ok(LookupEq::Defer) => return Err(ErrorKind::Unsupported),
            Err(k) => return Err(k),
        }
    }
    Ok(None)
}

/// One token of a parsed Excel wildcard pattern.
enum PatTok {
    /// `*` — matches any run of characters (including empty).
    Star,
    /// `?` — matches exactly one character.
    Any,
    /// A literal character (already case-unfolded at compare time).
    Lit(char),
}

/// Parse an Excel wildcard pattern: `*`→[`PatTok::Star`], `?`→[`PatTok::Any`],
/// `~` escapes the following `*`/`?`/`~` to a literal (a `~` before anything
/// else, or at end of string, is a literal `~`).
fn parse_pattern(pat: &str) -> Vec<PatTok> {
    let mut toks = Vec::new();
    let mut chars = pat.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '~' => match chars.peek() {
                Some(&n @ ('*' | '?' | '~')) => {
                    toks.push(PatTok::Lit(n));
                    chars.next();
                }
                _ => toks.push(PatTok::Lit('~')),
            },
            '*' => toks.push(PatTok::Star),
            '?' => toks.push(PatTok::Any),
            other => toks.push(PatTok::Lit(other)),
        }
    }
    toks
}

/// Case-insensitive single-character equality (Excel folds case in text
/// comparison; mirrors `xl-value`'s case-insensitive text rule).
fn ci_eq(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// Match a parsed wildcard `pat` against the whole of `text` (classic
/// linear-time backtracking glob over the last-seen `*`).
fn wildcard_match(pat: &[PatTok], text: &str) -> bool {
    let text: Vec<char> = text.chars().collect();
    let (mut ti, mut pi) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < text.len() {
        match pat.get(pi) {
            Some(PatTok::Any) => {
                pi += 1;
                ti += 1;
            }
            Some(PatTok::Lit(c)) if ci_eq(*c, text[ti]) => {
                pi += 1;
                ti += 1;
            }
            Some(PatTok::Star) => {
                star = Some((pi, ti));
                pi += 1;
            }
            // Mismatch (or pattern exhausted before the text): backtrack to the
            // last `*`, extending the span it consumes by one character.
            _ => {
                if let Some((sp, st)) = star {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                } else {
                    return false;
                }
            }
        }
    }
    // Any trailing `*`s match the empty remainder.
    while let Some(PatTok::Star) = pat.get(pi) {
        pi += 1;
    }
    pi == pat.len()
}

/// Approximate-match binary search (`range_lookup = TRUE`/omitted) — Excel's
/// actual algorithm, pinned by **OXP-088** (RUN-2026-07-11-oracle01).
///
/// Maintain `[lo, hi]`; probe the **floor** midpoint `mid = lo + (hi - lo) / 2`.
/// An **exact** hit returns that probe's row *immediately* — which is how Excel
/// resolves duplicate/unsorted equal keys (whichever the probe lands on, not a
/// "bottom-most" rule). Otherwise move `lo`/`hi` as an ordinary binary search
/// (`< lookup` ⇒ `lo = mid + 1`, `> lookup` ⇒ `hi = mid - 1`). After the loop
/// Excel returns row `hi` — the last probe that was `<= lookup` on sorted data,
/// and its algorithm-order-dependent "wrong answer" on unsorted data — or `#N/A`
/// (`Ok(None)`) when `hi < 0`, i.e. every probe stayed above `lookup`.
///
/// An **error cell at a probe midpoint does not abort** the search: Excel moves
/// into the upper half past it (`lo = mid + 1`), observed in OXP-088 H9.
///
/// A **`Blank` touching a comparison — the key, or a probed cell — defers**
/// (`Err(ErrorKind::Unsupported)`) instead of calling `compare`:
/// `compare`'s `Blank` morphs (`Blank ↔ 0`/`""`/`FALSE`, `Blank == Blank`)
/// are pinned only for the `=`/`<>`/ordering **operators**, not for this
/// search's approximate lookup ordering (OXP-088's probe data had no blanks;
/// OXP-104 marks interspersed-blank ordering unverified), so silently
/// applying them here would be a guess — see [`crate::lookup::approx_touches_blank`]
/// and this module's provenance note. The check sits after the error-cell
/// skip above, so an error cell is still skipped without ever "touching" the
/// key, and a `Blank`-free search (no `Blank` key, no `Blank` probed cell) is
/// unaffected byte-for-byte — same probes, same OXP-088-pinned answers. A
/// non-error, non-`Blank` comparison that itself errors (currently only
/// OXP-031's held non-ASCII-text deferral) still propagates as `Err`.
fn approx_search(rows: &[Vec<Value>], lookup: &Value) -> Result<Option<usize>, ErrorKind> {
    let mut lo: isize = 0;
    let mut hi: isize = rows.len() as isize - 1;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let cell = first_col(rows, mid as usize);
        if matches!(cell, Value::Error(_)) {
            lo = mid + 1;
            continue;
        }
        if approx_touches_blank(cell, lookup) {
            return Err(ErrorKind::Unsupported);
        }
        match compare(cell, lookup)? {
            Ordering::Equal => return Ok(Some(mid as usize)),
            Ordering::Less => lo = mid + 1,
            Ordering::Greater => hi = mid - 1,
        }
    }
    if hi < 0 {
        Ok(None)
    } else {
        Ok(Some(hi as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::test_support::{
        TestArg::{self, *},
        eval_direct, num, txt,
    };
    use xl_value::{ErrorKind, Value};

    // OXP-088 table A1:B7: unsorted first column [5,2,8,2,1,9,3] (duplicate 2).
    fn approx_table() -> TestArg {
        Rect {
            rows: 7,
            cols: 2,
            data: vec![
                num(5.0),
                txt("v-a"),
                num(2.0),
                txt("v-b"),
                num(8.0),
                txt("v-c"),
                num(2.0),
                txt("v-d"),
                num(1.0),
                txt("v-e"),
                num(9.0),
                txt("v-f"),
                num(3.0),
                txt("v-g"),
            ],
        }
    }

    fn vlookup_approx(lv: f64) -> Value {
        eval_direct(
            eval,
            vec![
                Scalar(num(lv)),
                approx_table(),
                Scalar(num(2.0)),
                Scalar(Value::Bool(true)),
            ],
        )
    }

    /// RUN-2026-07-11-oracle01 / OXP-088 (H1–H8): exact approximate-mode answers
    /// on the unsorted first column [5,2,8,2,1,9,3].
    #[test]
    fn oxp088_unsorted_approx_answers() {
        assert_eq!(vlookup_approx(2.0), txt("v-d")); // H1
        assert_eq!(vlookup_approx(4.0), txt("v-e")); // H2
        assert_eq!(vlookup_approx(8.0), txt("v-e")); // H3
        assert_eq!(vlookup_approx(10.0), txt("v-g")); // H4
        assert_eq!(vlookup_approx(0.0), Value::Error(ErrorKind::Na)); // H5
        assert_eq!(vlookup_approx(1.0), Value::Error(ErrorKind::Na)); // H6
        assert_eq!(vlookup_approx(3.0), txt("v-e")); // H7
        assert_eq!(vlookup_approx(9.0), txt("v-f")); // H8
    }

    /// RUN-2026-07-11-oracle01 / OXP-088 (H9): an error cell at a probe midpoint
    /// does not abort the search — `VLOOKUP(3, D1:E3, 2, TRUE)` over a middle
    /// `#DIV/0!` search cell returns `e-3`.
    #[test]
    fn oxp088_error_cell_at_midpoint_does_not_abort() {
        let v = eval_direct(
            eval,
            vec![
                Scalar(num(3.0)),
                Rect {
                    rows: 3,
                    cols: 2,
                    data: vec![
                        num(1.0),
                        txt("e-1"),
                        Value::Error(ErrorKind::Div0),
                        txt("e-2"),
                        num(3.0),
                        txt("e-3"),
                    ],
                },
                Scalar(num(2.0)),
                Scalar(Value::Bool(true)),
            ],
        );
        assert_eq!(v, txt("e-3"));
    }

    // OXP-089 table `tbl` A1:C4.
    fn wildcard_table() -> TestArg {
        Rect {
            rows: 4,
            cols: 3,
            data: vec![
                txt("apple"),
                txt("b-apple"),
                txt("c-apple"),
                txt("apricot"),
                txt("b-apricot"),
                txt("c-apricot"),
                txt("banana"),
                txt("b-banana"),
                txt("c-banana"),
                txt("a*b"),
                txt("b-star"),
                txt("c-star"),
            ],
        }
    }

    fn vlookup_exact(lookup: Value, col: f64) -> Value {
        eval_direct(
            eval,
            vec![
                Scalar(lookup),
                wildcard_table(),
                Scalar(num(col)),
                Scalar(Value::Bool(false)),
            ],
        )
    }

    /// RUN-2026-07-11-oracle01 / OXP-089 (H1–H5): exact-mode wildcards and
    /// non-integer `col_index_num` truncation toward zero.
    #[test]
    fn oxp089_wildcards_and_fractional_col_index() {
        // H1: prefix `*` — "ap*" matches "apple" (first of apple/apricot).
        assert_eq!(vlookup_exact(txt("ap*"), 2.0), txt("b-apple"));
        // H2: `~*` escapes to a literal `*` — matches the "a*b" key.
        assert_eq!(vlookup_exact(txt("a~*b"), 2.0), txt("b-star"));
        // H3: "A?C" matches no key → #N/A.
        assert_eq!(vlookup_exact(txt("A?C"), 2.0), Value::Error(ErrorKind::Na));
        // H4/H5: 2.9 and 2.1 both truncate to column 2 → "b-apple".
        assert_eq!(vlookup_exact(txt("apple"), 2.9), txt("b-apple"));
        assert_eq!(vlookup_exact(txt("apple"), 2.1), txt("b-apple"));
    }

    // OXP-165 whole column A:B as the used-extent walk sees it: A1=0/B1=100
    // (rel 0), A2=""/B2 (rel 1), A4=5/B4 (rel 3). The truly-blank A3 is absent.
    fn oxp165_table() -> TestArg {
        UsedRows(vec![
            (0, vec![num(0.0), num(100.0)]),
            (1, vec![txt(""), txt("v2")]),
            (3, vec![num(5.0), num(500.0)]),
        ])
    }

    fn vlookup_wholecol_exact(lookup: Value) -> Value {
        eval_direct(
            eval,
            vec![
                Scalar(lookup),
                oxp165_table(),
                Scalar(num(2.0)),
                Scalar(Value::Bool(false)),
            ],
        )
    }

    /// RUN-2026-07-11-oracle01 / OXP-165: an exact-mode lookup over a whole
    /// column finds a 0-valued first-column cell for both a literal `0` and a
    /// **Blank** key (a blank key coerces to 0), returning that row's column-2
    /// value — `VLOOKUP(0|<blank>, A:B, 2, FALSE)` = 100 (the row of A1=0).
    #[test]
    fn oxp165_zero_and_blank_key_match_zero_cell_exact() {
        assert_eq!(vlookup_wholecol_exact(num(0.0)), num(100.0));
        assert_eq!(vlookup_wholecol_exact(Value::Blank), num(100.0));
    }

    /// OXP-104 tail: when an *absent* row precedes the first populated match, a
    /// Blank key's populated match could be beaten by an invisible earlier blank
    /// row → defer (the first populated cell here is at rel 2).
    #[test]
    fn oxp104_blank_key_with_absent_row_before_match_defers() {
        let got = eval_direct(
            eval,
            vec![
                Scalar(Value::Blank),
                UsedRows(vec![(2, vec![num(0.0), num(999.0)])]),
                Scalar(num(2.0)),
                Scalar(Value::Bool(false)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Unsupported));
    }

    /// Only exact mode is pinned (OXP-165); a Blank key in APPROXIMATE mode over
    /// a whole column stays deferred (OXP-104).
    #[test]
    fn oxp104_blank_key_approx_mode_over_whole_column_defers() {
        let got = eval_direct(
            eval,
            vec![
                Scalar(Value::Blank),
                oxp165_table(),
                Scalar(num(2.0)),
                Scalar(Value::Bool(true)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Unsupported));
    }

    /// OXP-104 (H3, RUN-2026-07-11-oracle01) — the used-extent view of OXP-104's
    /// own whole-column fixture `A = {1, 2, <truly blank>, 4}` (B = 10/20/-/40):
    /// `VLOOKUP(C1, A:B, 2, FALSE)` with C1 blank is pinned to **`#N/A`**. Every
    /// populated first-column cell is a confirmed NoMatch (non-zero numbers) and
    /// the absent row is a truly-blank cell, which OXP-104 pins as NoMatch for a
    /// Blank key — so the completed scan's `#N/A` is fully determined by pinned
    /// facts, same as the bounded walk already answers
    /// (`oxp104_blank_key_over_bounded_column_no_zero_cell_is_na`). This is the
    /// L2-A corpus shape (`docs/l2-refusal-decomposition.md`, `WC_ISIN_Lookup` =
    /// `WC_Underlyings!$A:$B`, blank template-row keys over an all-text column).
    #[test]
    fn oxp104_blank_key_no_match_over_whole_column_is_na() {
        let got = eval_direct(
            eval,
            vec![
                Scalar(Value::Blank),
                UsedRows(vec![
                    (0, vec![num(1.0), num(10.0)]),
                    (1, vec![num(2.0), num(20.0)]),
                    (3, vec![num(4.0), num(40.0)]),
                ]),
                Scalar(num(2.0)),
                Scalar(Value::Bool(false)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Na));
    }

    /// PRESERVED bit-for-bit (L2-A condition 2): an unpinned Defer *pair* met
    /// mid-scan — a populated `""` first-column cell against a Blank key
    /// (OXP-171 queued) — still aborts the whole-column scan with
    /// `#UNSUPPORTED!`; the no-match `#N/A` above never bypasses `exact_eq`'s
    /// pair table.
    #[test]
    fn blank_key_whole_column_with_empty_text_cell_still_defers() {
        let got = eval_direct(
            eval,
            vec![
                Scalar(Value::Blank),
                UsedRows(vec![
                    (0, vec![num(1.0), num(10.0)]),
                    (1, vec![txt(""), num(20.0)]),
                    (3, vec![num(4.0), num(40.0)]),
                ]),
                Scalar(num(2.0)),
                Scalar(Value::Bool(false)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Unsupported));
    }

    // --- Bounded-range (dense-walk) blank-scoped equality --------------------
    // The `oxp165`/`oxp104` used-extent tests above cover the whole-column
    // path; these cover the exact same `crate::lookup::exact_eq` decision
    // table over a plain bounded `table_array`, where a genuinely `Blank`
    // cell is walked directly (never compacted away as "absent").

    /// Pinned NO-MATCH (OXP-104): a Blank key over a bounded column with no
    /// `0`-cell — `{1, 2, <truly blank>, 4}` — never matches (including the
    /// truly-blank A3 cell itself) and the search reaches the end → `#N/A`.
    /// The non-zero `1`/`2`/`4` cells must be ordinary skips, not deferrals,
    /// or the scan would abort with `#UNSUPPORTED!` before ever reaching A3.
    #[test]
    fn oxp104_blank_key_over_bounded_column_no_zero_cell_is_na() {
        let got = eval_direct(
            eval,
            vec![
                Scalar(Value::Blank),
                Rect {
                    rows: 4,
                    cols: 2,
                    data: vec![
                        num(1.0),
                        txt("r1"),
                        num(2.0),
                        txt("r2"),
                        Value::Blank,
                        txt("r3"),
                        num(4.0),
                        txt("r4"),
                    ],
                },
                Scalar(num(2.0)),
                Scalar(Value::Bool(false)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Na));
    }

    /// DEFER: a Blank key's first candidate is a populated `""` cell (no
    /// `0`-cell precedes it) — unpinned, so the search refuses rather than
    /// silently match or silently skip it.
    #[test]
    fn blank_key_vs_empty_text_first_candidate_defers() {
        let got = eval_direct(
            eval,
            vec![
                Scalar(Value::Blank),
                Rect {
                    rows: 2,
                    cols: 2,
                    data: vec![txt(""), num(100.0), num(5.0), num(500.0)],
                },
                Scalar(num(2.0)),
                Scalar(Value::Bool(false)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Unsupported));
    }

    /// DEFER: a Blank key's first candidate is a populated `FALSE` cell —
    /// unpinned, same as the `""` case above.
    #[test]
    fn blank_key_vs_false_first_candidate_defers() {
        let got = eval_direct(
            eval,
            vec![
                Scalar(Value::Blank),
                Rect {
                    rows: 1,
                    cols: 2,
                    data: vec![Value::Bool(false), num(100.0)],
                },
                Scalar(num(2.0)),
                Scalar(Value::Bool(false)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Unsupported));
    }

    /// DEFER: a real (non-Blank) `0` key against a truly-`Blank` first-column
    /// cell — the unpinned reverse direction of OXP-165.
    #[test]
    fn zero_key_vs_blank_cell_defers() {
        let got = eval_direct(
            eval,
            vec![
                Scalar(num(0.0)),
                Rect {
                    rows: 1,
                    cols: 2,
                    data: vec![Value::Blank, num(100.0)],
                },
                Scalar(num(2.0)),
                Scalar(Value::Bool(false)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Unsupported));
    }

    /// UNCHANGED: cross-type strictness (`5 <> "5"`) is untouched — a numeric
    /// key never matches a text cell holding the same digits.
    #[test]
    fn number_key_does_not_match_text_digit_cell() {
        let got = eval_direct(
            eval,
            vec![
                Scalar(num(5.0)),
                Rect {
                    rows: 1,
                    cols: 2,
                    data: vec![txt("5"), num(999.0)],
                },
                Scalar(num(2.0)),
                Scalar(Value::Bool(false)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Na));
    }

    /// UNCHANGED: an ordinary non-blank exact hit still works.
    #[test]
    fn ordinary_non_blank_exact_hit_still_works() {
        let got = eval_direct(
            eval,
            vec![
                Scalar(num(3.0)),
                Rect {
                    rows: 2,
                    cols: 2,
                    data: vec![num(1.0), txt("one"), num(3.0), txt("three")],
                },
                Scalar(num(2.0)),
                Scalar(Value::Bool(false)),
            ],
        );
        assert_eq!(got, txt("three"));
    }

    // --- Approximate-mode touch-a-Blank defer (this fix) --------------------

    /// THE BUG THIS FIX CLOSES: a Blank key in APPROXIMATE mode over a
    /// **bounded** (non-whole-column) `table_array` used to have no guard at
    /// all — only the whole-column path deferred. It must now defer too.
    #[test]
    fn blank_key_approx_mode_over_bounded_range_defers() {
        let got = eval_direct(
            eval,
            vec![
                Scalar(Value::Blank),
                Rect {
                    rows: 3,
                    cols: 2,
                    data: vec![
                        num(1.0),
                        txt("one"),
                        num(3.0),
                        txt("three"),
                        num(5.0),
                        txt("five"),
                    ],
                },
                Scalar(num(2.0)),
                Scalar(Value::Bool(true)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Unsupported));
    }

    /// THE BUG THIS FIX CLOSES: `VLOOKUP(999, A1:B20, 2, TRUE)` over a first
    /// column populated only in rows 1-10 (ascending 1..10) with a Blank tail
    /// in rows 11-20. Before this fix the binary search would probe into the
    /// Blank tail, have `compare` morph a Blank cell to `0`, and silently
    /// settle on a Blank row instead of correctly recognizing the ordering as
    /// unpinned. It must now defer the moment the search touches a Blank cell.
    #[test]
    fn blank_cell_touched_by_approx_search_over_oversized_range_defers() {
        let mut data: Vec<Value> = Vec::new();
        for i in 1..=10 {
            data.push(num(f64::from(i)));
            data.push(txt(&format!("v{i}")));
        }
        for _ in 0..10 {
            data.push(Value::Blank);
            data.push(Value::Blank);
        }
        let got = eval_direct(
            eval,
            vec![
                Scalar(num(999.0)),
                Rect {
                    rows: 20,
                    cols: 2,
                    data,
                },
                Scalar(num(2.0)),
                Scalar(Value::Bool(true)),
            ],
        );
        assert_eq!(got, Value::Error(ErrorKind::Unsupported));
    }

    /// Blank-free approximate searches are byte-identical to before this fix:
    /// the OXP-088-pinned unsorted-data answers (asserted above in
    /// `oxp088_unsorted_approx_answers`) and the OXP-088 H9 error-cell-skip
    /// answer (`oxp088_error_cell_at_midpoint_does_not_abort`) are unaffected
    /// by the touch-a-Blank defer, since neither fixture contains a Blank.
    /// This test re-affirms one such pinned case explicitly.
    #[test]
    fn blank_free_approx_search_still_byte_identical() {
        assert_eq!(vlookup_approx(8.0), txt("v-e")); // OXP-088 H3, unchanged.
    }
}
