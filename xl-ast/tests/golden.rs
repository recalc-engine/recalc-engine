//! Golden AST tests: a data-driven table mapping formula strings to their
//! span-free structural dump ([`xl_ast::Expr::to_sexpr`]), plus canonical
//! [`Display`] round-trip checks and negative (error) cases.
//!
//! These pin down every grammar production and the documented Excel precedence
//! quirks. They are the crate's specification snapshot until the Excel oracle
//! grids exist (`implementation-plan.md` §3).

use xl_ast::{ExprKind, MAX_NESTING_DEPTH, ParseErrorKind, ParseMode, parse, parse_with_mode};

/// Parse in A1 mode and return the structural s-expression.
fn sx(formula: &str) -> String {
    parse(formula)
        .unwrap_or_else(|e| panic!("parse of {formula:?} failed: {e}"))
        .to_sexpr()
}

/// Parse in R1C1 mode and return the structural s-expression.
fn sx_r1c1(formula: &str) -> String {
    parse_with_mode(formula, ParseMode::R1C1)
        .unwrap_or_else(|e| panic!("R1C1 parse of {formula:?} failed: {e}"))
        .to_sexpr()
}

// ===========================================================================
// Literals
// ===========================================================================
#[test]
fn literals() {
    let cases = [
        ("2", "2"),
        ("1.5", "1.5"),
        ("1e3", "1000"),
        ("1.5e-3", "0.0015"),
        (".5", "0.5"),
        ("\"hello\"", "\"hello\""),
        // "" doubling → a single embedded quote.
        ("\"he said \"\"hi\"\"\"", "\"he said \"hi\"\""),
        ("TRUE", "TRUE"),
        ("true", "TRUE"),
        ("False", "FALSE"),
    ];
    for (f, want) in cases {
        assert_eq!(sx(f), want, "formula {f:?}");
    }
}

#[test]
fn error_literals() {
    for lit in [
        "#NULL!",
        "#DIV/0!",
        "#VALUE!",
        "#REF!",
        "#NAME?",
        "#NUM!",
        "#N/A",
        "#GETTING_DATA",
        "#SPILL!",
        "#CALC!",
    ] {
        assert_eq!(sx(lit), lit, "error literal {lit:?}");
    }
}

// ===========================================================================
// References (A1)
// ===========================================================================
#[test]
fn a1_references() {
    let cases = [
        ("A1", "(ref A1)"),
        ("$A$1", "(ref $A$1)"),
        ("$A1", "(ref $A1)"),
        ("A$1", "(ref A$1)"),
        ("a1", "(ref A1)"),                 // column letters uppercased
        ("XFD1048576", "(ref XFD1048576)"), // grid maximum
        ("AA10", "(ref AA10)"),
    ];
    for (f, want) in cases {
        assert_eq!(sx(f), want, "formula {f:?}");
    }
}

#[test]
fn ranges_and_whole_row_col() {
    let cases = [
        ("A1:B2", "(: (ref A1) (ref B2))"),
        ("A:A", "(: (ref A) (ref A))"),
        ("1:1", "(: (ref 1) (ref 1))"),
        ("$A:$A", "(: (ref $A) (ref $A))"),
        ("A:C", "(: (ref A) (ref C))"),
        // Intersection binds looser than range.
        (
            "A1:B2 C2:D4",
            "(isect (: (ref A1) (ref B2)) (: (ref C2) (ref D4)))",
        ),
    ];
    for (f, want) in cases {
        assert_eq!(sx(f), want, "formula {f:?}");
    }
}

// ===========================================================================
// OXP-050 (RUN-2026-07-11-oracle01): internal precedence of the three
// reference operators, which Microsoft's "Calculation operators and
// precedence" table groups on one row without ranking. The oracle confirms
// Recalc's assumed order — `:` (range) binds tighter than ` ` (intersection)
// binds tighter than `,` (union):
//   =SUM(A1:A3 A2:A4)        -> 5      (A1:A3 ∩ A2:A4 = A2:A3)
//   =SUM((A1,A2):(B1,B2))    -> 33     (range spanning both grouped unions)
//   =SUM(A1:A3,B1:B3 C1:C3)  -> #NULL! (disjoint intersection inside an arg)
// The observed *values* need the evaluator; these pin the parse trees that
// make those values reachable.
// ===========================================================================
#[test]
fn reference_operator_precedence_oxp050() {
    // `:` binds tighter than ` `: each range groups before intersecting.
    assert_eq!(
        sx("SUM(A1:A3 A2:A4)"),
        "(call SUM (isect (: (ref A1) (ref A3)) (: (ref A2) (ref A4))))"
    );
    // Range spanning two parenthesized unions: `,` is union inside the parens
    // and `:` operates on the grouped operands (tighter than intersection,
    // looser than the grouping the parens force).
    assert_eq!(
        sx("SUM((A1,A2):(B1,B2))"),
        "(call SUM (: (paren (union (ref A1) (ref A2))) (paren (union (ref B1) (ref B2)))))"
    );
    // Inside an argument list the top-level `,` is the argument separator (not
    // union); ` ` intersection binds tighter than that separator, so the
    // second argument is the intersection of the two column ranges.
    assert_eq!(
        sx("SUM(A1:A3,B1:B3 C1:C3)"),
        "(call SUM (: (ref A1) (ref A3)) (isect (: (ref B1) (ref B3)) (: (ref C1) (ref C3))))"
    );
}

#[test]
fn sheet_qualified_and_3d() {
    let cases = [
        ("Sheet1!A1", "(ref Sheet1!A1)"),
        ("'Sheet Name'!A1", "(ref 'Sheet Name'!A1)"),
        ("Sheet1:Sheet3!A1:B2", "(: (ref Sheet1:Sheet3!A1) (ref B2))"),
        ("Sheet1!A:A", "(: (ref Sheet1!A) (ref A))"),
        // Quote-doubling inside a quoted sheet name.
        ("'O''Brien'!A1", "(ref 'O''Brien'!A1)"),
        // Sheet-qualified defined name.
        ("Sheet1!MyName", "(name Sheet1!MyName)"),
    ];
    for (f, want) in cases {
        assert_eq!(sx(f), want, "formula {f:?}");
    }
}

// ===========================================================================
// Operators & precedence quirks
// ===========================================================================
#[test]
fn precedence_quirks() {
    // Unary minus binds tighter than ^: (-2)^2 = 4.
    assert_eq!(sx("-2^2"), "(^ (u- 2) 2)");
    // ^ is left-associative: (2^3)^2 = 64.
    assert_eq!(sx("2^3^2"), "(^ (^ 2 3) 2)");
    // Explicit grouping overrides.
    assert_eq!(sx("-(2^2)"), "(u- (paren (^ 2 2)))");
    // Arithmetic precedence.
    assert_eq!(sx("2+3*4"), "(+ 2 (* 3 4))");
    assert_eq!(sx("2*3+4"), "(+ (* 2 3) 4)");
    assert_eq!(sx("2*3/4"), "(/ (* 2 3) 4)");
    // Concatenation is left-associative and looser than arithmetic.
    assert_eq!(sx("1&2&3"), "(& (& 1 2) 3)");
    assert_eq!(sx("1+2&3"), "(& (+ 1 2) 3)");
    // Comparison is loosest.
    assert_eq!(sx("1+2=3"), "(= (+ 1 2) 3)");
    assert_eq!(sx("1<2"), "(< 1 2)");
    assert_eq!(sx("1<=2"), "(<= 1 2)");
    assert_eq!(sx("1<>2"), "(<> 1 2)");
    assert_eq!(sx("1>=2"), "(>= 1 2)");
}

#[test]
fn unary_and_postfix() {
    assert_eq!(sx("-A1"), "(u- (ref A1))");
    assert_eq!(sx("+5"), "(u+ 5)");
    assert_eq!(sx("--1"), "(u- (u- 1))");
    assert_eq!(sx("50%"), "(% 50)");
    assert_eq!(sx("A1%"), "(% (ref A1))");
    // Unary minus binds tighter than percent: (-2)%.
    assert_eq!(sx("-2%"), "(% (u- 2))");
    // Spill-range postfix.
    assert_eq!(sx("A1#"), "(spill (ref A1))");
    assert_eq!(sx("Sheet1!A1#"), "(spill (ref Sheet1!A1))");
}

#[test]
fn implicit_intersection_at() {
    assert_eq!(sx("@A1"), "(@ (ref A1))");
    assert_eq!(sx("@A1:A10"), "(@ (: (ref A1) (ref A10)))");
    assert_eq!(sx("@MyName"), "(@ (name MyName))");
}

// ===========================================================================
// Function calls, names, arrays
// ===========================================================================
#[test]
fn function_calls() {
    let cases = [
        ("SUM(A1:A10)", "(call SUM (: (ref A1) (ref A10)))"),
        ("SUM()", "(call SUM)"),
        ("sum(1,2)", "(call SUM 1 2)"), // name uppercased
        // Empty / missing arguments.
        ("IF(A1,,2)", "(call IF (ref A1) <empty> 2)"),
        ("IF(,,)", "(call IF <empty> <empty> <empty>)"),
        ("SUM(1,)", "(call SUM 1 <empty>)"),
        // LET / LAMBDA are ordinary calls at the grammar level.
        ("LET(x,1,x+1)", "(call LET (name x) 1 (+ (name x) 1))"),
        (
            "LAMBDA(a,b,a+b)",
            "(call LAMBDA (name a) (name b) (+ (name a) (name b)))",
        ),
        // Union only inside a grouping paren; here the outer commas separate
        // args while the inner comma is a union.
        (
            "SUM((A1,B1))",
            "(call SUM (paren (union (ref A1) (ref B1))))",
        ),
        // _xlfn. prefix stripped and flagged.
        (
            "_xlfn.XLOOKUP(A1,B:B,C:C)",
            "(call XLOOKUP :xlfn (ref A1) (: (ref B) (ref B)) (: (ref C) (ref C)))",
        ),
    ];
    for (f, want) in cases {
        assert_eq!(sx(f), want, "formula {f:?}");
    }
}

#[test]
fn defined_names() {
    assert_eq!(sx("MyName"), "(name MyName)");
    assert_eq!(sx("foo.bar"), "(name foo.bar)");
    assert_eq!(sx("_hidden"), "(name _hidden)");
    assert_eq!(sx("A1&MyName"), "(& (ref A1) (name MyName))");
}

#[test]
fn array_literals() {
    let cases = [
        ("{1,2;3,4}", "(array (row 1 2) (row 3 4))"),
        ("{1,2,3}", "(array (row 1 2 3))"),
        ("{1;2;3}", "(array (row 1) (row 2) (row 3))"),
        ("{-1,2}", "(array (row -1 2))"),
        ("{1,\"a\",TRUE,#N/A}", "(array (row 1 \"a\" TRUE #N/A))"),
    ];
    for (f, want) in cases {
        assert_eq!(sx(f), want, "formula {f:?}");
    }
}

// ===========================================================================
// Structured references → Unsupported (v0)
// ===========================================================================
#[test]
fn structured_refs_are_unsupported() {
    for f in [
        "Table1[Col]",
        "Table1[[#Data],[Amount]]",
        "[Col]",
        "Sheet1!Table1[Col]",
    ] {
        let e = parse(f).unwrap();
        match &e.kind {
            ExprKind::Unsupported { raw, .. } => assert_eq!(raw, f, "raw preserved for {f:?}"),
            other => panic!("expected Unsupported for {f:?}, got {other:?}"),
        }
    }
}

// ===========================================================================
// R1C1 mode
// ===========================================================================
#[test]
fn r1c1_references() {
    let cases = [
        ("R1C1", "(ref R1C1)"),
        ("R2C5", "(ref R2C5)"),
        ("R[2]C[-1]", "(ref R[2]C[-1])"),
        ("RC", "(ref RC)"),
        ("R[-1]C", "(ref R[-1]C)"),
        ("RC[1]", "(ref RC[1])"),
        ("R1C1:R3C3", "(: (ref R1C1) (ref R3C3))"),
    ];
    for (f, want) in cases {
        assert_eq!(sx_r1c1(f), want, "R1C1 formula {f:?}");
    }
    // A function whose name starts with R/C must not be read as a reference.
    assert_eq!(sx_r1c1("RAND()"), "(call RAND)");
    assert_eq!(
        sx_r1c1("SUM(R1C1,R2C2)"),
        "(call SUM (ref R1C1) (ref R2C2))"
    );
    // Grid extremes are accepted exactly up to the bounds.
    assert_eq!(sx_r1c1("R1048576C16384"), "(ref R1048576C16384)");
    assert_eq!(sx_r1c1("R[-1048575]C[16383]"), "(ref R[-1048575]C[16383])");
}

#[test]
fn r1c1_out_of_range_is_an_error_never_zero() {
    // Regression: overflowed or out-of-grid R1C1 axes were once clamped to 0
    // via `unwrap_or(0)`, silently mis-parsing e.g. R[999999999999]C as RC.
    // Every case here must be a hard error.
    let cases = [
        "R[999999999999]C", // relative offset overflows i32/i64
        "R0C0",             // absolute row/col 0 — grid is 1-based
        "RC0",              // absolute col 0
        "R1C16385",         // col > XFD (16384)
        "R1048577C1",       // row > 1048576
        "R99999999999C1",   // huge absolute axis (u32 overflow)
        "R[1048576]C",      // relative row offset beyond ±1048575
        "RC[16384]",        // relative col offset beyond ±16383
        "R[-1048576]C",     // negative relative row offset out of range
    ];
    for f in cases {
        match parse_with_mode(f, ParseMode::R1C1) {
            Ok(e) => panic!("expected RefOutOfRange for {f:?}, parsed {}", e.to_sexpr()),
            Err(err) => assert_eq!(
                err.kind,
                ParseErrorKind::RefOutOfRange,
                "R1C1 formula {f:?}"
            ),
        }
    }
    // A malformed bracket group is a precise error, not a repaired axis.
    assert_eq!(
        parse_with_mode("R[2C1", ParseMode::R1C1).unwrap_err().kind,
        ParseErrorKind::UnterminatedBracket
    );
}

// ===========================================================================
// Canonical Display round-trip
// ===========================================================================
#[test]
fn display_round_trip() {
    let formulas = [
        "2+3*4",
        "-2^2",
        "2^3^2",
        "(1+2)*3",
        "SUM(A1:A10)",
        "IF(A1,,2)",
        "A1:B2 C2:D4",
        "SUM((A1,B1))",
        "{1,2;3,4}",
        "@A1:A10",
        "Sheet1!A1",
        "'Sheet Name'!A1",
        "Sheet1:Sheet3!A1:B2",
        // RFC-0004 slice-1: normalizes to `Sheet1!A1:B2`, which is a Display
        // fixed point.
        "Sheet1!A1:Sheet1!B2",
        "A:A",
        "1:1",
        "A1&\"x\"&B1",
        "50%",
        "A1#",
        "_xlfn.XLOOKUP(A1,B:B,C:C)",
        "#DIV/0!",
    ];
    for f in formulas {
        let first = parse(f).unwrap();
        let displayed = first.to_string();
        let second = parse(&displayed).unwrap_or_else(|e| {
            panic!("re-parse of Display {displayed:?} (from {f:?}) failed: {e}")
        });
        assert_eq!(
            first.to_sexpr(),
            second.to_sexpr(),
            "round-trip mismatch for {f:?} (displayed as {displayed:?})"
        );
    }
}

// ===========================================================================
// Negative tests — precise ParseErrors, never a silent mis-parse
// ===========================================================================
#[test]
fn negative_cases() {
    let cases: &[(&str, ParseErrorKind)] = &[
        ("\"unterminated", ParseErrorKind::UnterminatedString),
        ("'unterminated!A1", ParseErrorKind::UnterminatedSheetName),
        ("Table1[Col", ParseErrorKind::UnterminatedBracket),
        // $-anchored references outside the grid are errors (not names).
        ("$XFE$1", ParseErrorKind::RefOutOfRange),
        ("$A$1048577", ParseErrorKind::RefOutOfRange),
        // Malformed array literals.
        ("{1,2;3}", ParseErrorKind::RaggedArray),
        ("{A1}", ParseErrorKind::NonConstantInArray),
        ("{1+2}", ParseErrorKind::NonConstantInArray),
        // Structural.
        ("", ParseErrorKind::ExpectedExpr),
        (")", ParseErrorKind::ExpectedExpr),
        ("SUM(A1", ParseErrorKind::ExpectedClose(')')),
        ("A1,B1", ParseErrorKind::UnexpectedToken),
        // `NAME (` — call vs intersection is ambiguous; rejected pending
        // oracle probe OXP-052, never silently read as either.
        ("SUM (1)", ParseErrorKind::AmbiguousSpaceBeforeParen),
        ("SUM  (A1:A2)", ParseErrorKind::AmbiguousSpaceBeforeParen),
        ("myname (1)", ParseErrorKind::AmbiguousSpaceBeforeParen),
        ("Sheet1!nm (1)", ParseErrorKind::AmbiguousSpaceBeforeParen),
        // Per-endpoint sheet qualifiers, DEFERRED slice-2 (RFC 0004,
        // OXP-053 / RUN-2026-07-11-oracle01): a bare left endpoint
        // (`A1:Sheet1!B2`) or a *different*-sheet right endpoint
        // (`Sheet1!A1:Sheet2!B2`) still rejects rather than guess its
        // evaluator error mapping (#VALUE! / #NAME?, under-determined). The
        // redundant *same*-sheet form is now accepted — see
        // `same_sheet_double_qualified_range_normalizes_rfc0004_slice1`.
        //   FLAG (RFC-0004 slice-1): removed the `Sheet1!A1:Sheet1!B2`
        //   rejection entry (now accepted); added the cross-sheet entry.
        ("A1:Sheet1!B2", ParseErrorKind::SheetQualifiedRangeEndpoints),
        (
            "Sheet1!A1:Sheet2!B2",
            ParseErrorKind::SheetQualifiedRangeEndpoints,
        ),
    ];
    for (f, want) in cases {
        match parse(f) {
            Ok(e) => panic!("expected error {want:?} for {f:?}, parsed {}", e.to_sexpr()),
            Err(err) => assert_eq!(&err.kind, want, "formula {f:?}"),
        }
    }
}

// ===========================================================================
// The OXP-052 rejection is scoped to names: intersection with a parenthesized
// operand stays legal when the left side cannot be a function name.
// ===========================================================================
#[test]
fn spaced_paren_after_non_name_is_still_intersection() {
    assert_eq!(sx("(A1) (B1)"), "(isect (paren (ref A1)) (paren (ref B1)))");
    assert_eq!(sx("A1 (B1)"), "(isect (ref A1) (paren (ref B1)))");
    assert_eq!(sx("SUM(1) (2)"), "(isect (call SUM 1) (paren 2))");
}

// ===========================================================================
// Non-ASCII LETTERS in the non-leading position of a defined-name identifier
// are ACCEPTED — pinned by OXP-211 (RUN 2026-07-16, Excel 16.0). The FUSE
// corpus carries a Spanish energy-audit template family whose masters reference
// `NºdeMejoras_Actual` / `AhorroEnergíaAnual_kWh` (docs/shared-residual-
// classification.md); ECMA-376 §18.17 leaves the admitted class unpinned and
// `º`/`ª` are category Lo (the same class as CJK), so a "Unicode letter" rule
// could not be assumed. OXP-211 probed one non-leading char per candidate and
// Excel accepted+resolved a name for EVERY letter across scripts — Latin
// `í`/`º`/`ª`/`ñ`/`ü` (Ll/Lo), Greek `Ω` (Lu), CJK `中` (Lo) — decisively "any
// Unicode letter". The lexer now admits non-ASCII letters in the continue
// position (`is_name_continue`), unblocking ~97 shared-formula follow-on cells.
// Held narrow: a LEADING non-ASCII char and NON-letter non-ASCII (symbols `°`)
// were deferred to a COM probe and still error loudly — never a silent guess.
// See `xl-ast/src/lexer.rs::read_ident_run` (OXP-211).
// ===========================================================================
#[test]
fn non_ascii_letters_in_name_accepted_oxp_211() {
    // The two corpus master names now parse as ordinary defined names.
    assert_eq!(sx("NºdeMejoras_Actual"), "(name NºdeMejoras_Actual)");
    assert_eq!(
        sx("AhorroEnergíaAnual_kWh"),
        "(name AhorroEnergíaAnual_kWh)"
    );

    // Every OXP-211 candidate letter (Latin Ll/Lo, Greek Lu, CJK Lo), in the
    // non-leading position Excel accepted, lexes as one name token.
    for (f, want) in [
        ("NíX", "(name NíX)"),
        ("NºX", "(name NºX)"),
        ("NªX", "(name NªX)"),
        ("NñX", "(name NñX)"),
        ("NüX", "(name NüX)"),
        ("NΩX", "(name NΩX)"),
        ("N中X", "(name N中X)"),
    ] {
        assert_eq!(sx(f), want, "candidate {f:?}");
    }

    // The actual shared-group master idiom (a SUMIF over the non-ASCII name)
    // now parses — this is the expansion the widening unblocks.
    assert_eq!(
        sx("SUMIF(NºdeMejoras_Actual,$A52,ConsumoEnergiaAnual_tep_Actual)"),
        "(call SUMIF (name NºdeMejoras_Actual) (ref $A52) (name ConsumoEnergiaAnual_tep_Actual))",
    );

    // A *leading* non-ASCII LETTER now parses — pinned ACCEPT by OXP-211's
    // follow-up COM Name-Manager probe (RUN 2026-07-16): leading `í`/`Ω`/`中`
    // each define+resolve a name. The word-start gate widened via `is_name_start`.
    assert_eq!(sx("ºdeMejoras"), "(name ºdeMejoras)");
    assert_eq!(sx("Ωtotal"), "(name Ωtotal)");
    assert_eq!(sx("中值"), "(name 中值)");

    // STILL a loud non-answer (never-guess), even though the same COM probe found
    // Excel *accepts* them in a name: NON-letter non-ASCII — a symbol (`°`
    // U+00B0 So, `§` U+00A7 So) or combining mark — is not admitted, because
    // precisely matching Excel's So/Mn classes needs Unicode general-category
    // data this zero-dep crate cannot pull, and a crude "any non-ASCII" widening
    // would over-admit unprobed categories. So `is_name_continue` stops the run
    // and the dispatch rejects.
    assert_eq!(
        parse("N°X").unwrap_err().kind,
        ParseErrorKind::UnexpectedChar('°'),
        "non-letter non-ASCII symbol still rejected (needs a Unicode-category dep)",
    );
    // A *leading* combining mark (U+0301) was pinned REJECT by Excel too, and is
    // rejected here (a non-letter, so not an `is_name_start`).
    assert_eq!(
        parse("\u{0301}NX").unwrap_err().kind,
        ParseErrorKind::UnexpectedChar('\u{0301}'),
        "leading combining mark rejected (matches Excel)",
    );
}

// ===========================================================================
// FINDING-001 (fuzz): a range whose right operand is sheet-qualified used to
// depend on whitespace — canonical Display dropped the space and the output
// re-lexed as a 3-D sheet prefix, a different AST. RFC-0004 slice-1 (OXP-053,
// RUN-2026-07-11-oracle01) removed that asymmetry at its root: the lexer now
// backs off a cell-shaped 3-D-span first part so the contiguous and spaced
// spellings share one token stream, and whitespace around `:` is inert. The
// redundant *same*-sheet form is accepted and normalized (covered by
// `same_sheet_double_qualified_range_normalizes_rfc0004_slice1`); the shapes
// below are the DEFERRED slice-2 family (bare-left or cross-sheet endpoints),
// still a hard error in every spelling so a Display round-trip can never
// silently change meaning.
// ===========================================================================
#[test]
fn range_with_sheet_qualified_right_operand_is_rejected() {
    // The exact 6-byte fuzz repros (bare left endpoint — still slice-2).
    for (f, mode) in [
        ("m: m!M", ParseMode::A1),    // bytes 6d 3a 20 6d 21 4d
        ("R:\tZ!Z", ParseMode::R1C1), // bytes 52 3a 09 5a 21 5a
    ] {
        assert_eq!(
            parse_with_mode(f, mode).unwrap_err().kind,
            ParseErrorKind::SheetQualifiedRangeEndpoints,
            "repro {f:?}"
        );
    }
    // The wider slice-2 family, in the spaced spelling. FLAG (RFC-0004
    // slice-1): the `Sheet1!A1: Sheet1!B2` symmetric same-sheet entry was
    // removed from this rejection list — it is now accepted (see the
    // normalization test).
    for f in [
        "A1: Sheet1!B2",
        "foo: Sheet1!A1",
        "A1:(Sheet1!B2)",         // parenthesized smuggling
        "A1:@Sheet1!B2",          // @-wrapped smuggling
        "S\t:F![]",               // second fuzz artifact: sheet-qualified
        "A1: Sheet1!Table1[Col]", // structured ref as the right endpoint
    ] {
        assert_eq!(
            parse(f).unwrap_err().kind,
            ParseErrorKind::SheetQualifiedRangeEndpoints,
            "formula {f:?}"
        );
    }
}

// ===========================================================================
// RFC-0004 slice-1 (OXP-053, RUN-2026-07-11-oracle01): a redundant same-sheet
// double-qualified range `Sheet1!A1:Sheet1!B2` is exactly the left-qualified
// range `Sheet1!A1:B2` in Excel (`SUM(Sheet1!A1:Sheet1!B2)` -> 10.0). The
// parser normalizes it by dropping the right endpoint's redundant qualifier.
// Whitespace around `:` is inert (the H3/H5, H4/H6 probe pairs agree), so the
// contiguous and spaced spellings all yield the identical AST and round-trip.
// Cross-sheet / bare-left shapes stay the deferred slice-2 rejection.
// NEW TEST (RFC-0004 slice-1).
// ===========================================================================
#[test]
fn same_sheet_double_qualified_range_normalizes_rfc0004_slice1() {
    // The canonical (left-qualified) target every spelling must normalize to.
    let target = "(: (ref Sheet1!A1) (ref B2))";
    assert_eq!(sx("Sheet1!A1:B2"), target); // baseline: already supported
    for f in [
        "Sheet1!A1:Sheet1!B2",   // contiguous, both qualified
        "Sheet1!A1 : Sheet1!B2", // spaced both sides of `:`
        "Sheet1!A1: Sheet1!B2",  // spaced after `:` (the H-pair variant)
        "Sheet1!A1 :Sheet1!B2",  // spaced before `:`
        "Sheet1!A1:sheet1!B2",   // right sheet name folds case → still Sheet1
    ] {
        assert_eq!(sx(f), target, "same-sheet double-qual {f:?}");
    }
    // Case folds for the *comparison*, but the surviving left endpoint keeps
    // its original casing (Display preserves sheet-name case); only the
    // redundant right qualifier is dropped.
    assert_eq!(sx("SHEET1!A1:Sheet1!B2"), "(: (ref SHEET1!A1) (ref B2))");
    // Round-trip: normalized Display is the left-qualified form and re-parses
    // to the same tree (FINDING-001's round-trip worry disproved by the probe).
    for f in ["Sheet1!A1:Sheet1!B2", "Sheet1!A1 : Sheet1!B2"] {
        let e = parse(f).unwrap();
        assert_eq!(e.to_string(), "Sheet1!A1:B2", "Display of {f:?}");
        assert_eq!(parse(&e.to_string()).unwrap().to_sexpr(), e.to_sexpr());
    }
    // Inside a call, the same normalization applies (the 10.0 oracle case):
    // `SUM(Sheet1!A1:Sheet1!B2)` parses identically to `SUM(Sheet1!A1:B2)`.
    assert_eq!(sx("SUM(Sheet1!A1:Sheet1!B2)"), sx("SUM(Sheet1!A1:B2)"));
    // A quoted same-sheet name is likewise redundant on the right endpoint.
    assert_eq!(
        sx("'My Sheet'!A1:'My Sheet'!B2"),
        "(: (ref 'My Sheet'!A1) (ref B2))"
    );
    // Slice-2 guardrail: a *different* sheet on the right is NOT normalized —
    // it stays the deferred rejection, never silently collapsed to one sheet.
    assert_eq!(
        parse("Sheet1!A1:Sheet2!B2").unwrap_err().kind,
        ParseErrorKind::SheetQualifiedRangeEndpoints
    );
}

#[test]
fn bare_structured_ref_range_endpoint_round_trips() {
    // A *bare* (unqualified) structured ref as a range endpoint stays
    // accepted and must round-trip consistently — only the sheet-qualified
    // form is the FINDING-001 hazard.
    let e = parse("A1:Table1[Col]").unwrap();
    let redisplayed = parse(&e.to_string()).unwrap();
    assert_eq!(e.to_sexpr(), redisplayed.to_sexpr());
}

#[test]
fn contiguous_3d_spelling_round_trips_consistently() {
    // The contiguous spellings of the repros keep the ECMA-376 3-D-prefix
    // reading (a 3-D-qualified name) — and that reading must be a Display
    // fixed point, so no round-trip can flip between the two readings.
    let a1 = parse("M:m!M").unwrap();
    assert_eq!(a1.to_sexpr(), "(name M:m!M)");
    let redisplayed = parse(&a1.to_string()).unwrap();
    assert_eq!(a1.to_sexpr(), redisplayed.to_sexpr());

    let r1c1 = parse_with_mode("R:Z!Z", ParseMode::R1C1).unwrap();
    assert_eq!(r1c1.to_sexpr(), "(name R:Z!Z)");
    let redisplayed = parse_with_mode(&r1c1.to_string(), ParseMode::R1C1).unwrap();
    assert_eq!(r1c1.to_sexpr(), redisplayed.to_sexpr());
}

#[test]
fn intersection_whitespace_is_preserved_by_display() {
    // The intersection operator is *spelled* with whitespace; Display must
    // keep it (dropping it would merge the operands into one token and
    // change meaning — the generic FINDING-001 mechanism).
    let e = parse("A1 B1").unwrap();
    assert_eq!(e.to_string(), "A1 B1");
    assert_eq!(
        parse("A1 B1").unwrap().to_sexpr(),
        "(isect (ref A1) (ref B1))"
    );
    let e2 = parse("A1:B2 C2:D4").unwrap();
    assert_eq!(e2.to_string(), "A1:B2 C2:D4");
    assert_eq!(parse(&e2.to_string()).unwrap().to_sexpr(), e2.to_sexpr());
}

// ===========================================================================
// Recursion depth is bounded: pathological nesting is a precise error, never
// a stack-overflow abort. Formulas can legally be 32,767 chars, so untrusted
// workbook content must not be able to kill the process.
// ===========================================================================
#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    let deep_parens = format!("{}1{}", "(".repeat(5000), ")".repeat(5000));
    let deep_unary = format!("{}1", "-".repeat(5000));
    let deep_calls = format!("{}1{}", "SUM(".repeat(3000), ")".repeat(3000));
    let deep_at = format!("{}A1", "@".repeat(5000));
    for f in [&deep_parens, &deep_unary, &deep_calls, &deep_at] {
        match parse(f) {
            Ok(_) => panic!("expected TooDeeplyNested for {} chars", f.len()),
            Err(e) => assert_eq!(e.kind, ParseErrorKind::TooDeeplyNested),
        }
    }
}

#[test]
fn nesting_below_the_limit_parses() {
    // Excel's own cap is 64 nested function levels; anything a real workbook
    // can hold must stay well inside MAX_NESTING_DEPTH.
    let n = 64;
    assert!(n * 3 < MAX_NESTING_DEPTH, "limit must clear Excel's cap");
    let f = format!("{}1{}", "SUM(".repeat(n), ")".repeat(n));
    parse(&f).expect("64 nested calls (Excel's own limit) must parse");
    let g = format!("{}1{}", "(".repeat(100), ")".repeat(100));
    parse(&g).expect("100 nested parens must parse");
}

// ===========================================================================
// Out-of-grid bare words are names, not errors.
// ===========================================================================
#[test]
fn out_of_grid_bare_word_is_a_name() {
    // XFE column and an over-large row have no `$`, so they are defined names,
    // not references and not errors.
    assert_eq!(sx("XFE1"), "(name XFE1)");
    assert_eq!(sx("A1048577"), "(name A1048577)");
    assert_eq!(sx("ZZ99"), "(ref ZZ99)"); // ...but ZZ99 is in-grid.
}
