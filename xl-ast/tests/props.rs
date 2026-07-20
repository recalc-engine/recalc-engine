//! Property tests for the formula front-end.
//!
//! Three properties:
//! 1. **Robustness** — the parser never panics on arbitrary input; it always
//!    returns `Ok` or a `ParseError` (the front-door must survive fuzzing).
//! 2. **Display round-trip** — for a generated set of well-formed formulas,
//!    `parse(display(parse(s)))` yields a structurally identical tree, i.e.
//!    canonical [`Display`](std::fmt::Display) is a fixed point of parsing.
//! 3. **Bounded recursion** — nesting depths straddling
//!    [`MAX_NESTING_DEPTH`] either parse or fail with `TooDeeplyNested`;
//!    they never abort the process (regression guard for the recursion
//!    limit).

use proptest::prelude::*;
use xl_ast::{MAX_NESTING_DEPTH, ParseErrorKind, ParseMode, parse, parse_with_mode};

proptest! {
    /// The parser never panics, in either reference mode, on any input.
    #[test]
    fn never_panics(s in ".{0,64}") {
        let _ = parse(&s);
        let _ = parse_with_mode(&s, ParseMode::R1C1);
    }

    /// Restricted to plausible formula characters, still never panics.
    #[test]
    fn never_panics_formulaish(
        s in prop::collection::vec(
            prop::sample::select(FORMULA_CHARS.chars().collect::<Vec<_>>()),
            0..40,
        ).prop_map(|v| v.into_iter().collect::<String>())
    ) {
        let _ = parse(&s);
        let _ = parse_with_mode(&s, ParseMode::R1C1);
    }

    /// Canonical Display is a fixed point of parsing for well-formed formulas.
    #[test]
    fn display_is_a_fixed_point(formula in arb_formula(4)) {
        let e1 = parse(&formula).expect("generated formula should parse");
        let displayed = e1.to_string();
        let e2 = parse(&displayed).expect("Display output should re-parse");
        prop_assert_eq!(e1.to_sexpr(), e2.to_sexpr());
        // Display must be idempotent on its own output.
        prop_assert_eq!(displayed, e2.to_string());
    }

    /// Nesting depths straddling the recursion limit (well past it, up to
    /// `MAX_NESTING_DEPTH + 60`) never crash: shallow nesting parses, deep
    /// nesting fails with exactly `TooDeeplyNested`. A small unasserted band
    /// around the limit absorbs the ±few-frame bookkeeping differences
    /// between constructs.
    #[test]
    fn nesting_depth_is_bounded(
        n in 1usize..=MAX_NESTING_DEPTH + 60,
        construct in 0u8..4,
    ) {
        let formula = match construct {
            0 => format!("{}1{}", "(".repeat(n), ")".repeat(n)),
            1 => format!("{}1", "-".repeat(n)),
            2 => format!("{}1{}", "SUM(".repeat(n), ")".repeat(n)),
            _ => format!("{}A1", "@".repeat(n)),
        };
        match parse(&formula) {
            Ok(_) => prop_assert!(
                n <= MAX_NESTING_DEPTH,
                "depth {n} parsed Ok above the limit"
            ),
            Err(e) => {
                prop_assert_eq!(&e.kind, &ParseErrorKind::TooDeeplyNested);
                prop_assert!(
                    n + 5 >= MAX_NESTING_DEPTH,
                    "depth {} rejected far below the limit", n
                );
            }
        }
    }
}

const FORMULA_CHARS: &str = "AZ019+-*/^&=<>:,;()[]{}@#$%!.\"' ";

/// A strategy producing syntactically valid A1 formula strings. Subexpressions
/// are parenthesized where needed so every generated string parses.
fn arb_formula(depth: u32) -> BoxedStrategy<String> {
    let leaf = prop_oneof![
        (0u32..1000).prop_map(|n| n.to_string()),
        (1u32..50, 1u32..50).prop_map(|(c, r)| format!("{}{}", col(c), r)),
        "[a-z]{1,5}".prop_map(|s| s), // defined name
        Just("\"txt\"".to_string()),
        Just("TRUE".to_string()),
        Just("#N/A".to_string()),
        // Sheet-qualified shapes (FINDING-001 regression coverage): the
        // supported left-qualified forms must stay Display fixed points.
        (1u32..50, 1u32..50).prop_map(|(c, r)| format!("Sheet1!{}{}", col(c), r)),
        (1u32..50, 1u32..50).prop_map(|(c, r)| format!("'My Sheet'!{}{}", col(c), r)),
        // Left-qualified range (qualifier on the left endpoint only).
        (1u32..50, 1u32..50, 1u32..50, 1u32..50).prop_map(|(c1, r1, c2, r2)| format!(
            "Sheet1!{}{}:{}{}",
            col(c1),
            r1,
            col(c2),
            r2
        )),
        // 3-D sheet-qualified cell (sheet names deliberately not cell-like:
        // a cell-shaped first part is rejected per OXP-053).
        (1u32..50, 1u32..50).prop_map(|(c, r)| format!("Alpha:Omega!{}{}", col(c), r)),
    ]
    .boxed();

    if depth == 0 {
        return leaf;
    }

    let inner = arb_formula(depth - 1);
    prop_oneof![
        leaf.clone(),
        // Binary arithmetic / comparison, fully parenthesized.
        (inner.clone(), bin_op(), inner.clone()).prop_map(|(a, op, b)| format!("({a}{op}{b})")),
        // Unary.
        inner.clone().prop_map(|a| format!("(-{a})")),
        // Percent postfix.
        inner.clone().prop_map(|a| format!("({a})%")),
        // Function call with up to three args.
        prop::collection::vec(inner.clone(), 1..4)
            .prop_map(|args| format!("SUM({})", args.join(","))),
        // Range of two cells.
        (1u32..50, 1u32..50, 1u32..50, 1u32..50).prop_map(|(c1, r1, c2, r2)| format!(
            "{}{}:{}{}",
            col(c1),
            r1,
            col(c2),
            r2
        )),
    ]
    .boxed()
}

fn bin_op() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "+", "-", "*", "/", "^", "&", "=", "<", ">", "<=", ">=", "<>",
    ])
}

/// 1-based column index → letters.
fn col(mut n: u32) -> String {
    let mut buf = Vec::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        buf.push(b'A' + rem as u8);
        n = (n - 1) / 26;
    }
    buf.reverse();
    String::from_utf8(buf).unwrap()
}
