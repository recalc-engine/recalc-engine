//! Property tests for the `xl-value` value model and coercion tables.
//!
//! These assert the *structural* guarantees of the frozen contract that hold
//! for all inputs (never-panic, round-trip, total-order axioms,
//! case-insensitive invariants, interner identity). They complement the
//! table-driven unit tests in `lib.rs`, which pin the specific documented
//! coercion values. Both are provisional relative to the Excel oracle grids
//! (`implementation-plan.md` §3), which become the value authority later.

use core::cmp::Ordering;

use proptest::prelude::*;
use xl_value::{
    Array, RectRange, Ref, SheetId, Text, Value, compare, text_eq_ci, to_bool, to_number, to_text,
    total_order,
};

/// A short pool of strings covering numeric-looking, boolean-looking, and
/// arbitrary text, plus the edge cases from the plan's synthetic-grid spec
/// (`""`, `" "`, `"abc"`, signs, percent, grouping).
fn text_pool() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        Just(" ".to_string()),
        Just("abc".to_string()),
        Just("TRUE".to_string()),
        Just("false".to_string()),
        Just("1".to_string()),
        Just("-3".to_string()),
        Just("1.5".to_string()),
        Just("1e2".to_string()),
        Just("50%".to_string()),
        Just("1,000".to_string()),
        Just("Ω".to_string()),
        "[a-zA-Z0-9 .,%-]{0,8}",
    ]
}

/// Finite numbers only (the NaN/Inf invariant guarantees cells never hold
/// non-finite values). Spans the full finite range, including the extremes
/// near `f64::MAX` where 15-significant-digit rounding can overflow, and the
/// smallest positive normal.
fn finite_number() -> impl Strategy<Value = f64> {
    prop_oneof![
        Just(0.0),
        Just(-0.0),
        Just(f64::MAX),
        Just(-f64::MAX),
        Just(f64::MIN_POSITIVE),
        -1e12..1e12,
        (-1e300..1e300),
        1e300..f64::MAX,
        (1e300..f64::MAX).prop_map(|x| -x),
        (-1000i64..1000i64).prop_map(|i| i as f64),
    ]
    .prop_filter("finite", |x| x.is_finite())
}

/// The scalar, directly-comparable subset (`Number`/`Text`/`Bool`) over which
/// [`compare`] is a genuine total order (no `Blank` morphing, no errors).
///
/// OXP-031 (RUN-2026-07-11-oracle01, HELD) removed non-ASCII text from this
/// subset: `compare` now *defers* (`#UNSUPPORTED!`) on any non-ASCII operand
/// rather than fold code points, so a non-ASCII `Text` is no longer directly
/// comparable. Restrict the text arm to ASCII to keep this an error-free total
/// order — the non-ASCII defer itself is exercised by the `oxp_031_*` unit test.
fn comparable_scalar() -> impl Strategy<Value = Value> {
    prop_oneof![
        finite_number().prop_map(Value::number),
        text_pool()
            .prop_filter("ASCII text only (OXP-031 defers non-ASCII compare)", |s| s
                .is_ascii())
            .prop_map(|s| Value::text(&s)),
        any::<bool>().prop_map(Value::Bool),
    ]
}

/// Any value, including `Blank`, `Error`, `Array`, and `Ref` — for
/// never-panic and total-order-over-everything checks.
///
/// **`Value::Lambda` is deliberately excluded** (RFC-0012 BC-6): a lambda is
/// born-refusing everywhere — coercion returns `#UNSUPPORTED!`, and two
/// *distinct* lambdas reaching `total_order` is a consumer bug that now panics
/// via `unreachable!` (SORT/UNIQUE-class consumers refuse lambdas upstream). So
/// a lambda has no meaningful "never-panic / total-order-over-everything"
/// contract to fuzz here; its refusal edges are covered by targeted unit tests
/// (`lambda_*` in `lib.rs`, `Value::Lambda` arms across `coerce.rs`), not this
/// strategy. Adding it to the pool would (correctly) trip those guards.
fn any_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        finite_number().prop_map(Value::number),
        text_pool().prop_map(|s| Value::text(&s)),
        any::<bool>().prop_map(Value::Bool),
        Just(Value::Blank),
        error_kind().prop_map(Value::Error),
        ref_value(),
    ];
    // One level of array nesting over scalar leaves keeps arrays rectangular
    // and avoids unbounded recursion.
    prop_oneof![
        leaf.clone(),
        (1usize..3, 1usize..3).prop_flat_map(move |(r, c)| {
            proptest::collection::vec(scalar_leaf(), r * c)
                .prop_map(move |data| Value::Array(Array::new(r, c, data).unwrap()))
        }),
    ]
}

fn scalar_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        finite_number().prop_map(Value::number),
        text_pool().prop_map(|s| Value::text(&s)),
        any::<bool>().prop_map(Value::Bool),
        Just(Value::Blank),
        error_kind().prop_map(Value::Error),
    ]
}

fn error_kind() -> impl Strategy<Value = xl_value::ErrorKind> {
    use xl_value::ErrorKind::*;
    prop_oneof![
        Just(Null),
        Just(Div0),
        Just(Value),
        Just(Ref),
        Just(Name),
        Just(Num),
        Just(Na),
        Just(GettingData),
        Just(Spill),
        Just(Calc),
        Just(Unsupported),
        Just(Blocked),
        Just(Resource),
    ]
}

fn ref_value() -> impl Strategy<Value = Value> {
    (0u32..4, 0u32..8, 0u32..8, 0u32..8, 0u32..8).prop_map(|(s, r0, r1, c0, c1)| {
        Value::Ref(Ref::new(SheetId(s), RectRange::new(r0, r1, c0, c1)))
    })
}

proptest! {
    // --- Coercion never panics on any value, in any direction. ------------
    #[test]
    fn coercion_never_panics(v in any_value()) {
        let _ = to_number(&v);
        let _ = to_bool(&v);
        let _ = to_text(&v);
        prop_assert!(v.upholds_number_invariant());
    }

    #[test]
    fn compare_never_panics(a in any_value(), b in any_value()) {
        let _ = compare(&a, &b);
        let _ = total_order(&a, &b);
    }

    // --- number -> text -> number round-trips within tolerance. -----------
    // to_text uses a v0 "General" format capped at 15 significant digits, so
    // the round-trip is exact only to a relative 1e-13 (comfortably looser
    // than the ~1e-15 rounding floor, tighter than any real coercion need).
    #[test]
    fn number_text_number_roundtrip(x in finite_number()) {
        // OXP-002 (RUN-2026-07-11-oracle01): a *computed* value whose magnitude
        // exceeds Excel's text-entry limit (9.99999999999999E+307) still formats
        // fine as text but no longer re-parses — text->number rejects an
        // above-entry-limit magnitude as #VALUE!. That is CORRECT: the entry
        // limit is a text-*input* rule, not a value rule, so a super-limit
        // computed value legitimately does not round-trip through text. The
        // property therefore holds only for in-range magnitudes; in-range
        // round-tripping is NOT weakened. (`to_text` is still exercised on the
        // out-of-range extremes — it must never panic or emit inf/NaN.)
        const EXCEL_MAX_MAGNITUDE: f64 = 9.999_999_999_999_99e307;
        let text = to_text(&Value::number(x)).expect("number formats");
        if x.abs() <= EXCEL_MAX_MAGNITUDE {
            let back = to_number(&Value::Text(text)).expect("re-parses");
            if x == 0.0 {
                prop_assert_eq!(back, 0.0);
            } else {
                let rel = (back - x).abs() / x.abs();
                prop_assert!(rel <= 1e-13, "x={} back={} rel={}", x, back, rel);
            }
        }
    }

    // --- to_text never leaks non-finite spellings for finite input. -------
    // Guards the General-format overflow path: 15-significant-digit rounding
    // near f64::MAX can overflow to infinity internally; the formatter must
    // fall back to the unrounded value, never emit "inf"/"nan".
    #[test]
    fn to_text_never_emits_inf_or_nan(x in finite_number()) {
        let text = to_text(&Value::number(x)).expect("finite number formats");
        let lower = text.as_str().to_ascii_lowercase();
        prop_assert!(
            !lower.contains("inf") && !lower.contains("nan"),
            "finite {} formatted as {:?}",
            x,
            text.as_str()
        );
    }

    // --- Case-insensitive equality invariants. ----------------------------
    #[test]
    fn ci_equality_upper_invariant(a in text_pool(), b in text_pool()) {
        // eq(a,b) == eq(upper(a), b): folding case on one side cannot change
        // the equality verdict.
        prop_assert_eq!(text_eq_ci(&a, &b), text_eq_ci(&a.to_uppercase(), &b));
        // Reflexive and symmetric.
        prop_assert!(text_eq_ci(&a, &a));
        prop_assert_eq!(text_eq_ci(&a, &b), text_eq_ci(&b, &a));
    }

    #[test]
    fn compare_text_is_case_insensitive(a in text_pool(), b in text_pool()) {
        let base = compare(&Value::text(&a), &Value::text(&b));
        let upped = compare(&Value::text(&a.to_uppercase()), &Value::text(&b.to_uppercase()));
        prop_assert_eq!(base, upped);
    }

    // --- compare() is a consistent total order on the comparable subset. --
    #[test]
    fn compare_antisymmetric(a in comparable_scalar(), b in comparable_scalar()) {
        let ab = compare(&a, &b).expect("comparable");
        let ba = compare(&b, &a).expect("comparable");
        prop_assert_eq!(ab, ba.reverse());
    }

    #[test]
    fn compare_transitive(
        a in comparable_scalar(),
        b in comparable_scalar(),
        c in comparable_scalar(),
    ) {
        let ab = compare(&a, &b).expect("comparable");
        let bc = compare(&b, &c).expect("comparable");
        let ac = compare(&a, &c).expect("comparable");
        if ab == Ordering::Less && bc == Ordering::Less {
            prop_assert_eq!(ac, Ordering::Less);
        }
        if ab == Ordering::Equal && bc == Ordering::Equal {
            prop_assert_eq!(ac, Ordering::Equal);
        }
        if ab == Ordering::Greater && bc == Ordering::Greater {
            prop_assert_eq!(ac, Ordering::Greater);
        }
    }

    // --- total_order() is a genuine total order over ALL values. ----------
    #[test]
    fn total_order_reflexive(a in any_value()) {
        prop_assert_eq!(total_order(&a, &a), Ordering::Equal);
    }

    #[test]
    fn total_order_antisymmetric(a in any_value(), b in any_value()) {
        prop_assert_eq!(total_order(&a, &b), total_order(&b, &a).reverse());
    }

    #[test]
    fn total_order_transitive(a in any_value(), b in any_value(), c in any_value()) {
        let ab = total_order(&a, &b);
        let bc = total_order(&b, &c);
        let ac = total_order(&a, &c);
        if ab == Ordering::Less && bc == Ordering::Less {
            prop_assert_eq!(ac, Ordering::Less);
        }
        if ab == Ordering::Greater && bc == Ordering::Greater {
            prop_assert_eq!(ac, Ordering::Greater);
        }
    }

    // --- Interner returns identity-equal handles for equal strings. -------
    #[test]
    fn interner_identity(s in text_pool()) {
        let a = Text::new(&s);
        let b = Text::new(&s);
        prop_assert!(a.ptr_eq(&b));
        prop_assert_eq!(&a, &b);
    }
}
