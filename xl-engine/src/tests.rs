//! End-to-end orchestration tests.
//!
//! `xl-io` has no writer, but its model is fully constructible from public
//! fields, so these build tiny [`Workbook`]s directly (the `build` helper) —
//! simpler and more legible than committing binary `.xlsx` fixtures, and
//! sufficient to exercise the whole io→ast→graph→eval path. Behavior assertions
//! trace to `docs/specs/SUM.md` / `docs/specs/IF.md`; they are a specification
//! snapshot expected to be cross-checked against the Excel oracle grids later
//! (`implementation-plan.md` §3).

use std::collections::BTreeMap;

use xl_io::{
    CalcSettings, Cell, DateSystem, DefinedName, FormulaKind, NumFmtId, RawFormula, Sheet,
    Workbook, WorkbookFlags,
};
use xl_value::{ErrorKind, SheetId, Value};

use super::*;

// ---- workbook builder helpers -------------------------------------------

fn formula_cell(text: &str) -> Cell {
    Cell {
        value: Value::Blank,
        formula: Some(RawFormula {
            text: Some(text.to_string()),
            kind: FormulaKind::Normal,
            shared_index: None,
            range: None,
        }),
        num_fmt: NumFmtId::general(),
    }
}

/// Like [`formula_cell`] but **array-entered** — a legacy `Ctrl+Shift+Enter`
/// CSE array formula (`<f t="array">`), so the engine evaluates it in array
/// context and does **not** apply legacy implicit intersection (OXP-163). `ref`
/// carries the master cell's own single-cell range (`self_ref`), mirroring how
/// Excel writes a one-cell CSE formula; the engine ignores it (it only reads the
/// text + `t="array"` kind), but it keeps the fixture faithful to a real part.
fn array_formula_cell(text: &str, self_ref: &str) -> Cell {
    Cell {
        value: Value::Blank,
        formula: Some(RawFormula {
            text: Some(text.to_string()),
            kind: FormulaKind::Array,
            shared_index: None,
            range: Some(self_ref.to_string()),
        }),
        num_fmt: NumFmtId::general(),
    }
}

fn literal_cell(value: Value) -> Cell {
    Cell {
        value,
        formula: None,
        num_fmt: NumFmtId::general(),
    }
}

/// A shared-formula **master** cell (ECMA-376 §18.17.2): `t="shared"` with a
/// body `text`, the group index `si`, and the group's `ref` range. Recalc reads
/// the body + `si` (the master's own cell position is the group origin) and
/// ignores `ref`, but the fixture stays faithful to a real `<f>` part.
fn shared_master_cell(text: &str, si: u32, ref_range: &str) -> Cell {
    Cell {
        value: Value::Blank,
        formula: Some(RawFormula {
            text: Some(text.to_string()),
            kind: FormulaKind::Shared,
            shared_index: Some(si),
            range: Some(ref_range.to_string()),
        }),
        num_fmt: NumFmtId::general(),
    }
}

/// A shared-formula **follow-on** cell: `t="shared"`, group index `si`, and no
/// body (`text = None`) — its formula is the group master's, translated by this
/// cell's relative offset from the master.
fn shared_follow_cell(si: u32) -> Cell {
    Cell {
        value: Value::Blank,
        formula: Some(RawFormula {
            text: None,
            kind: FormulaKind::Shared,
            shared_index: Some(si),
            range: None,
        }),
        num_fmt: NumFmtId::general(),
    }
}

/// Build a single-sheet workbook named "Sheet1" from `(row, col, cell)` entries.
fn build(cells: Vec<(u32, u32, Cell)>) -> Workbook {
    build_named("Sheet1", cells, Vec::new())
}

fn build_named(
    name: &str,
    cells: Vec<(u32, u32, Cell)>,
    defined_names: Vec<DefinedName>,
) -> Workbook {
    build_named_with_hidden(
        name,
        cells,
        defined_names,
        std::collections::BTreeSet::new(),
    )
}

/// Like [`build_named`] but marks the given **0-based** rows hidden
/// (`<row hidden="1">`), to exercise `SUBTOTAL`'s `101`–`111` hidden-row
/// exclusion end-to-end (OXP-121).
fn build_named_with_hidden(
    name: &str,
    cells: Vec<(u32, u32, Cell)>,
    defined_names: Vec<DefinedName>,
    hidden_rows: std::collections::BTreeSet<u32>,
) -> Workbook {
    let mut map: BTreeMap<(u32, u32), Cell> = BTreeMap::new();
    for (r, c, cell) in cells {
        map.insert((r, c), cell);
    }
    Workbook {
        sheets: vec![Sheet {
            name: name.to_string(),
            sheet_id: 1,
            sheets_index: 0,
            cells: map,
            hidden_rows,
        }],
        date_system: DateSystem::default(),
        calc_settings: CalcSettings::default(),
        defined_names,
        flags: WorkbookFlags::default(),
    }
}

/// Like [`build`] but pins the workbook's 1900/1904 date system, to exercise
/// that `Engine::load` threads it into the date functions' `EvalContext`.
fn build_with_date_system(cells: Vec<(u32, u32, Cell)>, date_system: DateSystem) -> Workbook {
    let mut wb = build(cells);
    wb.date_system = date_system;
    wb
}

fn s0() -> SheetId {
    SheetId(0)
}

fn num(x: f64) -> Value {
    Value::number(x)
}

// ---- tests --------------------------------------------------------------

#[test]
fn sum_of_literals() {
    // SUM.md §1: adds numeric arguments.
    let mut e = Engine::load(build(vec![(0, 0, formula_cell("SUM(1,2,3)"))]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&num(6.0)));
    assert!(e.diagnostics().is_empty());
}

#[test]
fn formula_result_of_empty_reference_is_zero_not_blank() {
    // Excel caches 0 (not blank) for a formula whose final value is an empty
    // reference. Corpus-oracle-confirmed: 88% of all Enron value mismatches
    // were this pattern. B1 = `=A1` with A1 empty (absent) must recalc to 0.
    let mut e = Engine::load(build(vec![(0, 1, formula_cell("A1"))]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 1), Some(&num(0.0)));

    // And it propagates as 0 downstream: C1 = `=B1+1` -> 1.
    let mut e2 = Engine::load(build(vec![
        (0, 1, formula_cell("A1")),
        (0, 2, formula_cell("B1+1")),
    ]));
    e2.recalc();
    assert_eq!(e2.value(s0(), 0, 1), Some(&num(0.0)));
    assert_eq!(e2.value(s0(), 0, 2), Some(&num(1.0)));
}

#[test]
fn if_branches_end_to_end() {
    // IF.md §1: selects the branch matching the test.
    let e_true = {
        let mut e = Engine::load(build(vec![(0, 0, formula_cell("IF(1>0,10,20)"))]));
        e.recalc();
        e.value(s0(), 0, 0).cloned()
    };
    assert_eq!(e_true, Some(num(10.0)));

    let e_false = {
        let mut e = Engine::load(build(vec![(0, 0, formula_cell("IF(1>2,10,20)"))]));
        e.recalc();
        e.value(s0(), 0, 0).cloned()
    };
    assert_eq!(e_false, Some(num(20.0)));
}

#[test]
fn if_laziness_unselected_branch_not_computed() {
    // IF.md §2: the unselected branch (a division by zero, then an unknown
    // function) must not error the result.
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell("IF(TRUE,1,1/0)")),
        (1, 0, formula_cell("IF(FALSE,NOTAFUNC(),2)")),
    ]));
    let r = e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&num(1.0)));
    assert_eq!(e.value(s0(), 1, 0), Some(&num(2.0)));
    // Laziness means the unknown function was never evaluated → no diagnostic.
    assert_eq!(r.diagnostics, 0, "unselected branch must not diagnose");
}

#[test]
fn formula_chain_with_range() {
    // A1=1 ; A2=A1+1 ; A3=SUM(A1:A2) → 1, 2, 3.
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell("1")),
        (1, 0, formula_cell("A1+1")),
        (2, 0, formula_cell("SUM(A1:A2)")),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&num(1.0)));
    assert_eq!(e.value(s0(), 1, 0), Some(&num(2.0)));
    assert_eq!(e.value(s0(), 2, 0), Some(&num(3.0)));
}

#[test]
fn sum_range_propagates_div0() {
    // SUM.md §Error: an error inside a referenced range propagates.
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell("1/0")),
        (1, 0, literal_cell(num(5.0))),
        (2, 0, formula_cell("SUM(A1:A2)")),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&Value::Error(ErrorKind::Div0)));
    assert_eq!(e.value(s0(), 2, 0), Some(&Value::Error(ErrorKind::Div0)));
}

#[test]
fn date_functions_end_to_end_incl_1900_leap_bug() {
    // The whole io→ast→graph→eval path for the date family, pinning the 1900
    // leap-year bug at serial 60 (docs/specs/DATE.md §5).
    //   A1 = DATE(1900,2,29) → 60 (the fake leap day)
    //   A2 = YEAR(A1)  → 1900
    //   A3 = MONTH(A1) → 2
    //   A4 = DAY(A1)   → 29
    //   A5 = EOMONTH(DATE(1900,1,1),1) → 60 (Feb 1900 = 29 days)
    //   A6 = DATE(2020,13,1) → 2021-01-01 = 44197 (month overflow)
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell("DATE(1900,2,29)")),
        (1, 0, formula_cell("YEAR(A1)")),
        (2, 0, formula_cell("MONTH(A1)")),
        (3, 0, formula_cell("DAY(A1)")),
        (4, 0, formula_cell("EOMONTH(DATE(1900,1,1),1)")),
        (5, 0, formula_cell("DATE(2020,13,1)")),
    ]));
    let r = e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&num(60.0)), "DATE(1900,2,29)");
    assert_eq!(e.value(s0(), 1, 0), Some(&num(1900.0)), "YEAR");
    assert_eq!(e.value(s0(), 2, 0), Some(&num(2.0)), "MONTH");
    assert_eq!(e.value(s0(), 3, 0), Some(&num(29.0)), "DAY");
    assert_eq!(e.value(s0(), 4, 0), Some(&num(60.0)), "EOMONTH Feb 1900");
    assert_eq!(e.value(s0(), 5, 0), Some(&num(44197.0)), "month overflow");
    assert_eq!(
        r.diagnostics, 0,
        "no diagnostics for supported date formulas"
    );
}

#[test]
fn date_functions_thread_the_1904_system() {
    // Same serial-60 formula, but a 1904-flagged workbook: serial 60 is the
    // *real* 1904-03-01 (no phantom leap day). Proves Engine::load threads the
    // workbook's date system into the EvalContext.
    let mut e = Engine::load(build_with_date_system(
        vec![
            (0, 0, formula_cell("YEAR(60)")),
            (1, 0, formula_cell("MONTH(60)")),
            (2, 0, formula_cell("DAY(60)")),
        ],
        DateSystem::Excel1904,
    ));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&num(1904.0)));
    assert_eq!(e.value(s0(), 1, 0), Some(&num(3.0)));
    assert_eq!(e.value(s0(), 2, 0), Some(&num(1.0)));
}

#[test]
fn incremental_edit_recomputes_only_dependents() {
    // A1=1 (literal) ; A2=A1+1 ; A3=A2+1 ; C1=SUM(10,1) (independent).
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(1.0))),
        (1, 0, formula_cell("A1+1")),
        (2, 0, formula_cell("A2+1")),
        (0, 2, formula_cell("SUM(10,1)")),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 1, 0), Some(&num(2.0)));
    assert_eq!(e.value(s0(), 2, 0), Some(&num(3.0)));
    assert_eq!(e.value(s0(), 0, 2), Some(&num(11.0)));

    let before = e.eval_count();
    e.edit(CellId::new(s0(), 0, 0), CellInput::Literal(num(10.0)));

    // Only A2 and A3 recompute — not the independent C1.
    let a2 = CellId::new(s0(), 1, 0);
    let a3 = CellId::new(s0(), 2, 0);
    assert_eq!(e.last_recalc_cells(), &[a2, a3]);
    assert_eq!(e.eval_count() - before, 2);
    assert_eq!(e.value(s0(), 1, 0), Some(&num(11.0)));
    assert_eq!(e.value(s0(), 2, 0), Some(&num(12.0)));
    assert_eq!(
        e.value(s0(), 0, 2),
        Some(&num(11.0)),
        "independent cell untouched"
    );
}

#[test]
fn edit_formula_rebuilds_dependencies() {
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(2.0))),
        (1, 0, literal_cell(num(3.0))),
        (2, 0, formula_cell("A1+1")),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 2, 0), Some(&num(3.0)));

    // Re-point A3 at A2 instead of A1.
    e.edit(
        CellId::new(s0(), 2, 0),
        CellInput::Formula("A2*10".to_string()),
    );
    assert_eq!(e.value(s0(), 2, 0), Some(&num(30.0)));

    // Now editing A2 must recompute A3; editing A1 must not.
    let before = e.eval_count();
    e.edit(CellId::new(s0(), 1, 0), CellInput::Literal(num(4.0)));
    assert_eq!(e.last_recalc_cells(), &[CellId::new(s0(), 2, 0)]);
    assert_eq!(e.value(s0(), 2, 0), Some(&num(40.0)));

    e.edit(CellId::new(s0(), 0, 0), CellInput::Literal(num(99.0)));
    assert_eq!(e.eval_count() - before, 1, "A1 has no dependents now");
}

#[test]
fn cycle_is_unsupported_with_diagnostic() {
    // A1=B1+1 ; B1=A1+1 → a 2-cycle.
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell("B1+1")),
        (0, 1, formula_cell("A1+1")),
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
    assert_eq!(
        e.value(s0(), 0, 1),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
    let diags = e.diagnostics();
    assert_eq!(diags.len(), 2);
    assert!(diags.iter().all(|d| d.message.contains("OXP-070")));
    assert!(
        diags
            .iter()
            .all(|d| d.kind == DiagnosticKind::CircularReference)
    );
}

#[test]
fn unknown_function_is_unsupported_with_diagnostic() {
    let mut e = Engine::load(build(vec![(0, 0, formula_cell("BOGUS(1,2)"))]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
    let diags = e.diagnostics();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].kind, DiagnosticKind::UnknownFunction);
    assert!(diags[0].message.contains("BOGUS"));
}

#[test]
fn external_data_functions_are_blocked_with_distinct_sentinel() {
    for function in ["WEBSERVICE", "RTD", "STOCKHISTORY"] {
        let mut e = Engine::load(build(vec![(
            0,
            0,
            formula_cell(&format!("{function}(\"example\")")),
        )]));
        e.recalc();
        assert_eq!(
            e.value(s0(), 0, 0),
            Some(&Value::Error(ErrorKind::Blocked)),
            "{function} must use the sandbox blocked sentinel"
        );
        let diags = e.diagnostics_for(s0(), 0, 0);
        assert_eq!(diags.len(), 1, "{function} emits one refusal");
        assert_eq!(diags[0].kind, DiagnosticKind::UnsupportedConstruct);
        assert!(diags[0].message.contains("blocked external-data function"));
    }
}

#[test]
fn parse_error_is_unsupported_with_diagnostic_no_panic() {
    // An unterminated string is a parse error; the cell must become
    // #UNSUPPORTED! with a diagnostic, never a panic.
    let mut e = Engine::load(build(vec![(0, 0, formula_cell("SUM(\"oops)"))]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
    let diags = e.diagnostics();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].kind, DiagnosticKind::ParseError);
}

#[test]
fn load_time_refusals_surface_before_recalc() {
    // A parse error is a *known* refusal the moment the workbook loads:
    // `diagnostics()` must report it BEFORE any `recalc`, so a consumer that
    // reads values pre-recalc cannot mistake a poisoned workbook for "clean
    // cached values, zero refusals" (the pre-recalc silent window — never-guess,
    // the Recalc design rules §0).
    let mut e = Engine::load(build(vec![(0, 0, formula_cell("SUM(\"oops)"))]));
    let diags = e.diagnostics();
    assert_eq!(
        diags.len(),
        1,
        "load-time refusal must be visible before recalc"
    );
    assert_eq!(diags[0].kind, DiagnosticKind::ParseError);
    // And `recalc` must not double-count it: `run_cell` removes then re-inserts
    // a cell's diagnostics, so the seed is consistent with the post-recalc set.
    let r = e.recalc();
    assert_eq!(r.diagnostics, 1);
    assert_eq!(e.diagnostics().len(), 1);
}

#[test]
fn arity_error_diagnostic_kind() {
    // IF requires 2..=3 args; IF(1) is an arity error → #UNSUPPORTED! with an
    // ArityError diagnostic (behavior unchanged; the kind is machine-readable).
    let mut e = Engine::load(build(vec![(0, 0, formula_cell("IF(1)"))]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
    let diags = e.diagnostics_for(s0(), 0, 0);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].kind, DiagnosticKind::ArityError);
    assert!(diags[0].message.contains("IF"));
}

#[test]
fn multiple_diagnostics_per_cell_are_all_kept() {
    // Two unknown functions in one formula: BOTH must be recorded (run_cell
    // must not truncate to the first), in left-to-right emission order.
    // (Names chosen so they cannot lex as A1 cell references.)
    let mut e = Engine::load(build(vec![(0, 0, formula_cell("FOO(1)+QUX(2)"))]));
    let r = e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
    let diags = e.diagnostics_for(s0(), 0, 0);
    assert_eq!(diags.len(), 2, "both refusals recorded, not just the first");
    assert!(diags[0].message.contains("FOO"));
    assert!(diags[1].message.contains("QUX"));
    assert!(
        diags
            .iter()
            .all(|d| d.kind == DiagnosticKind::UnknownFunction)
    );
    // The workbook-level count and the flat listing agree with the per-cell view.
    assert_eq!(r.diagnostics, 2);
    assert_eq!(e.diagnostics().len(), 2);
}

#[test]
fn diagnostics_for_clean_cell_is_empty_and_clears_on_fix() {
    let mut e = Engine::load(build(vec![(0, 0, formula_cell("BOGUS(1)"))]));
    e.recalc();
    assert_eq!(e.diagnostics_for(s0(), 0, 0).len(), 1);
    assert!(e.diagnostics_for(s0(), 5, 5).is_empty(), "no such cell");

    // Fixing the formula clears the cell's diagnostics.
    e.edit(
        CellId::new(s0(), 0, 0),
        CellInput::Formula("SUM(1,2)".to_string()),
    );
    assert_eq!(e.value(s0(), 0, 0), Some(&num(3.0)));
    assert!(e.diagnostics_for(s0(), 0, 0).is_empty());
}

#[test]
fn volatile_call_reschedules_every_recalc() {
    // A1=NOW() is volatile (unimplemented → #UNSUPPORTED!) ; B1=2 (literal).
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell("NOW()")),
        (0, 1, literal_cell(num(2.0))),
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported))
    );

    // Editing the unrelated B1 must still reschedule the volatile A1.
    e.edit(CellId::new(s0(), 0, 1), CellInput::Literal(num(3.0)));
    assert!(
        e.last_recalc_cells().contains(&CellId::new(s0(), 0, 0)),
        "volatile cell must recompute on every recalc"
    );
}

#[test]
fn defined_name_simple_ref_resolves() {
    // A workbook-scoped name pointing at a single cell.
    let names = vec![DefinedName {
        name: "Rate".to_string(),
        formula: "Sheet1!$A$1".to_string(),
        sheet_scope: None,
    }];
    let mut e = Engine::load(build_named(
        "Sheet1",
        vec![
            (0, 0, literal_cell(num(7.0))),
            (1, 0, formula_cell("Rate*2")),
        ],
        names,
    ));
    e.recalc();
    assert_eq!(e.value(s0(), 1, 0), Some(&num(14.0)));
}

// ---- Lane L2-D: sheet-scoped defined names (ECMA-376 §18.2.6) ------------
// A `definedName` with `localSheetId="N"` is scoped to the sheet at 0-based
// position N of the `<sheets>` collection; within that sheet it shadows a
// workbook-global name of the same string, and other sheets see the global.
// Corpus shape: a sheet-local `rate`/`X` name shadowing a global one.

/// Two-sheet workbook with explicit `sheets_index` values, to model files
/// where skipped `<sheets>` entries (chartsheets, veryHidden VBA sheets)
/// shift the `localSheetId` index space away from the loaded tab order.
fn build_two_sheets_indexed(
    s1: (&str, u32, Vec<(u32, u32, Cell)>),
    s2: (&str, u32, Vec<(u32, u32, Cell)>),
    defined_names: Vec<DefinedName>,
) -> Workbook {
    let mk = |cells: Vec<(u32, u32, Cell)>| {
        let mut map: BTreeMap<(u32, u32), Cell> = BTreeMap::new();
        for (r, c, cell) in cells {
            map.insert((r, c), cell);
        }
        map
    };
    Workbook {
        sheets: vec![
            Sheet {
                name: s1.0.to_string(),
                sheet_id: 1,
                sheets_index: s1.1,
                cells: mk(s1.2),
                hidden_rows: std::collections::BTreeSet::new(),
            },
            Sheet {
                name: s2.0.to_string(),
                sheet_id: 2,
                sheets_index: s2.1,
                cells: mk(s2.2),
                hidden_rows: std::collections::BTreeSet::new(),
            },
        ],
        date_system: DateSystem::default(),
        calc_settings: CalcSettings::default(),
        defined_names,
        flags: WorkbookFlags::default(),
    }
}

#[test]
fn sheet_local_name_shadows_global_on_its_sheet_only() {
    // `Rate` is defined BOTH workbook-globally (→ Sheet1!$A$1 = 10) and
    // locally on Sheet2 (`localSheetId="1"` → Sheet2!$A$1 = 100). The name
    // match is ASCII-case-insensitive (the local is declared `rate`).
    let names = vec![
        DefinedName {
            name: "Rate".to_string(),
            formula: "Sheet1!$A$1".to_string(),
            sheet_scope: None,
        },
        DefinedName {
            name: "rate".to_string(),
            formula: "Sheet2!$A$1".to_string(),
            sheet_scope: Some(1),
        },
    ];
    let mut e = Engine::load(build_two_sheets_indexed(
        (
            "Sheet1",
            0,
            vec![
                (0, 0, literal_cell(num(10.0))),
                (0, 1, formula_cell("Rate*2")), // B1 on Sheet1 → global
            ],
        ),
        (
            "Sheet2",
            1,
            vec![
                (0, 0, literal_cell(num(100.0))),
                (0, 1, formula_cell("Rate*2")), // B1 on Sheet2 → local shadows
            ],
        ),
        names,
    ));
    e.recalc();
    assert_eq!(
        e.value(SheetId(0), 0, 1),
        Some(&num(20.0)),
        "off-scope sheet sees the workbook-global name"
    );
    assert_eq!(
        e.value(SheetId(1), 0, 1),
        Some(&num(200.0)),
        "in-scope sheet: the sheet-local name shadows the global"
    );
    assert!(e.diagnostics().is_empty());
}

#[test]
fn sheet_local_name_resolves_in_scope_and_refuses_out_of_scope() {
    // `X` exists ONLY as a Sheet1-local name. On Sheet1 it resolves; on
    // Sheet2 there is no visible name of that string — refuse loudly
    // (#UNSUPPORTED! + diagnostic), never fall back to the local.
    let names = vec![DefinedName {
        name: "X".to_string(),
        formula: "Sheet1!$A$1".to_string(),
        sheet_scope: Some(0),
    }];
    let mut e = Engine::load(build_two_sheets_indexed(
        (
            "Sheet1",
            0,
            vec![(0, 0, literal_cell(num(5.0))), (0, 1, formula_cell("X+1"))],
        ),
        ("Sheet2", 1, vec![(0, 1, formula_cell("X+1"))]),
        names,
    ));
    e.recalc();
    assert_eq!(e.value(SheetId(0), 0, 1), Some(&num(6.0)));
    assert_eq!(
        e.value(SheetId(1), 0, 1),
        Some(&Value::Error(ErrorKind::Unsupported)),
        "a name local to another sheet is not visible here"
    );
    let diags = e.diagnostics_for(SheetId(1), 0, 1);
    assert!(
        diags
            .iter()
            .any(|d| d.message.starts_with("unsupported defined name: ")),
        "out-of-scope name must refuse with the defined-name diagnostic, got {diags:?}"
    );
}

#[test]
fn sheet_qualified_name_reference_is_refused_loudly() {
    // `Sheet1!Rate`: which scope a sheet-QUALIFIED name reference selects is
    // not determined by ECMA-376 §18.2.6 (it pins storage scoping only) —
    // unpinned semantics, so the engine refuses loudly rather than guessing.
    // An oracle confirmation probe is pending. Note: before this change the
    // engine silently resolved the global with
    // the qualifier IGNORED — a silent-wrong risk the refusal replaces.
    let names = vec![DefinedName {
        name: "Rate".to_string(),
        formula: "Sheet1!$A$1".to_string(),
        sheet_scope: None,
    }];
    let mut e = Engine::load(build_named(
        "Sheet1",
        vec![
            (0, 0, literal_cell(num(7.0))),
            (1, 0, formula_cell("Sheet1!Rate*2")),
        ],
        names,
    ));
    e.recalc();
    assert_eq!(
        e.value(s0(), 1, 0),
        Some(&Value::Error(ErrorKind::Unsupported)),
        "sheet-qualified name reference is unpinned → loud refusal"
    );
    let diags = e.diagnostics_for(s0(), 1, 0);
    assert!(
        diags
            .iter()
            .any(|d| d.message.starts_with("unsupported defined name: ")),
        "qualified-name refusal must carry the defined-name diagnostic, got {diags:?}"
    );
}

#[test]
fn indirect_of_sheet_qualified_name_refuses_rather_than_guessing_ref() {
    // `INDIRECT("Sheet1!Rate")`: the INDIRECT name route maps a genuinely
    // undefined name to #REF!, but a sheet-QUALIFIED name reaching `None` is
    // unpinned-refusal territory (lane L2-D) — it must refuse loudly, not
    // emit the plausible #REF!.
    let names = vec![DefinedName {
        name: "Rate".to_string(),
        formula: "Sheet1!$A$1".to_string(),
        sheet_scope: None,
    }];
    let mut e = Engine::load(build_named(
        "Sheet1",
        vec![
            (0, 0, literal_cell(num(7.0))),
            (1, 0, formula_cell("INDIRECT(\"Sheet1!Rate\")*2")),
        ],
        names,
    ));
    e.recalc();
    assert_eq!(
        e.value(s0(), 1, 0),
        Some(&Value::Error(ErrorKind::Unsupported)),
        "INDIRECT of a sheet-qualified name is unpinned → loud refusal, not #REF!"
    );
    let diags = e.diagnostics_for(s0(), 1, 0);
    assert!(
        diags
            .iter()
            .any(|d| d.message.starts_with("unsupported defined name: ")),
        "must carry the defined-name diagnostic, got {diags:?}"
    );
}

#[test]
fn local_scope_maps_through_the_skipped_sheets_index_space() {
    // The workbook's `<sheets>` collection was [chartsheet, Alpha, Beta]:
    // the chartsheet (collection index 0) is skipped at load, so the loaded
    // tabs are Alpha (SheetId 0, sheets_index 1) and Beta (SheetId 1,
    // sheets_index 2). `localSheetId` indexes the COLLECTION (§18.2.6):
    //   • `T` global → Alpha!$A$1, and `T` local to collection index 2
    //     (= Beta) → Beta!$A$1: Beta shadows, Alpha sees the global.
    //   • `U` local to collection index 0 (= the SKIPPED chartsheet): scoped
    //     to a sheet that hosts no formulas — it must resolve NOWHERE (and
    //     must not be misread as Alpha-local via the tab index).
    let names = vec![
        DefinedName {
            name: "T".to_string(),
            formula: "Alpha!$A$1".to_string(),
            sheet_scope: None,
        },
        DefinedName {
            name: "T".to_string(),
            formula: "Beta!$A$1".to_string(),
            sheet_scope: Some(2),
        },
        DefinedName {
            name: "U".to_string(),
            formula: "Alpha!$A$1".to_string(),
            sheet_scope: Some(0),
        },
    ];
    let mut e = Engine::load(build_two_sheets_indexed(
        (
            "Alpha",
            1,
            vec![
                (0, 0, literal_cell(num(1.0))),
                (0, 1, formula_cell("T*10")),
                (0, 2, formula_cell("U+1")),
            ],
        ),
        (
            "Beta",
            2,
            vec![(0, 0, literal_cell(num(2.0))), (0, 1, formula_cell("T*10"))],
        ),
        names,
    ));
    e.recalc();
    assert_eq!(
        e.value(SheetId(0), 0, 1),
        Some(&num(10.0)),
        "Alpha is NOT collection index 2: it sees the global T"
    );
    assert_eq!(
        e.value(SheetId(1), 0, 1),
        Some(&num(20.0)),
        "Beta (collection index 2) sees its local T"
    );
    assert_eq!(
        e.value(SheetId(0), 0, 2),
        Some(&Value::Error(ErrorKind::Unsupported)),
        "a name scoped to the skipped chartsheet resolves nowhere"
    );
}

#[test]
fn edit_through_sheet_local_name_reschedules_dependents() {
    // Precedent extraction must resolve the LOCAL target on the scoped
    // sheet, so an edit to that target reschedules the dependent.
    let names = vec![
        DefinedName {
            name: "V".to_string(),
            formula: "Sheet1!$A$1".to_string(),
            sheet_scope: None,
        },
        DefinedName {
            name: "V".to_string(),
            formula: "Sheet2!$A$1".to_string(),
            sheet_scope: Some(1),
        },
    ];
    let mut e = Engine::load(build_two_sheets_indexed(
        ("Sheet1", 0, vec![(0, 0, literal_cell(num(1.0)))]),
        (
            "Sheet2",
            1,
            vec![(0, 0, literal_cell(num(2.0))), (0, 1, formula_cell("V+1"))],
        ),
        names,
    ));
    e.recalc();
    assert_eq!(e.value(SheetId(1), 0, 1), Some(&num(3.0)));
    // Editing the LOCAL target (Sheet2!A1) must recompute Sheet2!B1 …
    e.edit(CellId::new(SheetId(1), 0, 0), CellInput::Literal(num(40.0)));
    assert_eq!(e.value(SheetId(1), 0, 1), Some(&num(41.0)));
    // … and editing the GLOBAL's target (Sheet1!A1) must NOT change it.
    e.edit(
        CellId::new(SheetId(0), 0, 0),
        CellInput::Literal(num(500.0)),
    );
    assert_eq!(e.value(SheetId(1), 0, 1), Some(&num(41.0)));
}

#[test]
fn recalc_is_deterministic_and_idempotent() {
    let make = || {
        build(vec![
            (0, 0, formula_cell("1")),
            (1, 0, formula_cell("A1+1")),
            (2, 0, formula_cell("SUM(A1:A2)*IF(A2>0,2,3)")),
        ])
    };

    // Idempotent: recalc twice yields identical values.
    let mut e = Engine::load(make());
    e.recalc();
    let first: Vec<_> = (0..3).map(|r| e.value(s0(), r, 0).cloned()).collect();
    e.recalc();
    let second: Vec<_> = (0..3).map(|r| e.value(s0(), r, 0).cloned()).collect();
    assert_eq!(first, second);

    // Two engines from identical workbooks agree exactly.
    let mut e2 = Engine::load(make());
    e2.recalc();
    let other: Vec<_> = (0..3).map(|r| e2.value(s0(), r, 0).cloned()).collect();
    assert_eq!(first, other);
    assert_eq!(first[2], Some(num(6.0))); // (1+2) * 2
}

// ---- RFC 0001: used-extent whole-column iteration (end-to-end) -----------
// Plus defined-name / cross-sheet range-resolution regression coverage, which
// the used-extent whole-column path also relies on.

/// Build a two-sheet workbook (sheets get `SheetId(0)` and `SheetId(1)` by
/// position) with workbook-scoped defined names.
fn build_two_sheets(
    s1: (&str, Vec<(u32, u32, Cell)>),
    s2: (&str, Vec<(u32, u32, Cell)>),
    defined_names: Vec<DefinedName>,
) -> Workbook {
    let mk = |cells: Vec<(u32, u32, Cell)>| {
        let mut map: BTreeMap<(u32, u32), Cell> = BTreeMap::new();
        for (r, c, cell) in cells {
            map.insert((r, c), cell);
        }
        map
    };
    Workbook {
        sheets: vec![
            Sheet {
                name: s1.0.to_string(),
                sheet_id: 1,
                sheets_index: 0,
                cells: mk(s1.1),
                hidden_rows: std::collections::BTreeSet::new(),
            },
            Sheet {
                name: s2.0.to_string(),
                sheet_id: 2,
                sheets_index: 1,
                cells: mk(s2.1),
                hidden_rows: std::collections::BTreeSet::new(),
            },
        ],
        date_system: DateSystem::default(),
        calc_settings: CalcSettings::default(),
        defined_names,
        flags: WorkbookFlags::default(),
    }
}

fn txt(s: &str) -> Value {
    Value::text(s)
}

/// A 3-row lookup table [key, value] laid out at `(base_row.. , col..col+1)`.
fn conv_table(cells: &[(u32, u32, Value)]) -> Vec<(u32, u32, Cell)> {
    cells
        .iter()
        .map(|(r, c, v)| (*r, *c, literal_cell(v.clone())))
        .collect()
}

#[test]
fn vlookup_whole_column_inline_same_sheet() {
    // A1:B3 lookup table via a whole-COLUMN table_array A:B.
    let cells = vec![
        (0, 0, literal_cell(num(1.0))),
        (0, 1, literal_cell(num(10.0))),
        (1, 0, literal_cell(num(2.0))),
        (1, 1, literal_cell(num(20.0))),
        (2, 0, literal_cell(num(3.0))),
        (2, 1, literal_cell(num(30.0))),
        (0, 4, formula_cell("VLOOKUP(2,A:B,2,FALSE)")), // E1
        (1, 4, formula_cell("VLOOKUP(\"Euro\",A:B,2,FALSE)")), // E2: absent key
    ];
    let mut e = Engine::load(build(cells));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 4),
        Some(&num(20.0)),
        "whole-column exact match"
    );
    assert_eq!(
        e.value(s0(), 1, 4),
        Some(&Value::Error(ErrorKind::Na)),
        "absent key over whole column → #N/A (not a spurious match)"
    );
}

#[test]
fn sumif_countif_whole_column_inline() {
    let cells = vec![
        (0, 0, literal_cell(num(6.0))),
        (1, 0, literal_cell(num(3.0))),
        (2, 0, literal_cell(num(8.0))),
        (5, 0, literal_cell(num(7.0))), // a gap between row 2 and row 5
        (0, 2, formula_cell("SUMIF(A:A,\">5\")")), // C1
        (1, 2, formula_cell("COUNTIF(A:A,\">5\")")), // C2
    ];
    let mut e = Engine::load(build(cells));
    e.recalc();
    // 6, 8, 7 match ">5" → sum 21, count 3.
    assert_eq!(e.value(s0(), 0, 2), Some(&num(21.0)));
    assert_eq!(e.value(s0(), 1, 2), Some(&num(3.0)));
}

#[test]
fn vlookup_whole_column_via_defined_name() {
    let names = vec![DefinedName {
        name: "CONV".to_string(),
        formula: "Data!A:B".to_string(),
        sheet_scope: None,
    }];
    let mut e = Engine::load(build_two_sheets(
        (
            "Sheet1",
            vec![
                (0, 0, formula_cell("VLOOKUP(2,CONV,2,FALSE)")),
                (1, 0, formula_cell("VLOOKUP(\"Euro\",CONV,2,FALSE)")),
            ],
        ),
        (
            "Data",
            conv_table(&[
                (0, 0, num(1.0)),
                (0, 1, num(10.0)),
                (1, 0, num(2.0)),
                (1, 1, num(20.0)),
                (2, 0, num(3.0)),
                (2, 1, num(30.0)),
            ]),
        ),
        names,
    ));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&num(20.0)),
        "named whole-column match"
    );
    assert_eq!(
        e.value(s0(), 1, 0),
        Some(&Value::Error(ErrorKind::Na)),
        "named whole-column absent key → #N/A"
    );
}

/// Corpus shape (used-extent clamp, example 7):
/// a defined name whose body is a **`$`-anchored, sheet-qualified whole-column
/// range** (`WC_ISIN_Lookup` = `WC_Underlyings!$A:$B`) used as an exact-match
/// VLOOKUP `table_array`, with the corpus's trailing empty `range_lookup`
/// argument (`VLOOKUP(key, name, 2, )` — omitted 4th arg coerces `Blank` →
/// `FALSE` = exact). Must take the RFC-0001 used-extent walk exactly like the
/// direct-reference whole-column path, not refuse.
#[test]
fn vlookup_exact_whole_column_dollar_anchored_defined_name() {
    let names = vec![DefinedName {
        name: "WC_ISIN_Lookup".to_string(),
        formula: "WC_Underlyings!$A:$B".to_string(),
        sheet_scope: None,
    }];
    let mut e = Engine::load(build_two_sheets(
        (
            "Sheet1",
            vec![
                (0, 0, literal_cell(txt("DE000AB1234"))), // A1: the key
                // B1: the corpus formula shape, trailing empty 4th arg.
                (
                    0,
                    1,
                    formula_cell(
                        "IF(ISERROR(VLOOKUP(A1,WC_ISIN_Lookup,2,)),\"\",VLOOKUP(A1,WC_ISIN_Lookup,2,))",
                    ),
                ),
                // B2: the bare exact-match call, absent key → #N/A (no guess).
                (1, 1, formula_cell("VLOOKUP(\"absent\",WC_ISIN_Lookup,2,)")),
            ],
        ),
        (
            "WC_Underlyings",
            conv_table(&[
                (0, 0, txt("DE000AB1234")),
                (0, 1, txt("Underlying-1")),
                (2, 0, txt("DE000CD5678")), // gap row 1 absent: sparse walk
                (2, 1, txt("Underlying-2")),
            ]),
        ),
        names,
    ));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 1),
        Some(&txt("Underlying-1")),
        "$-anchored named whole-column exact match"
    );
    assert_eq!(
        e.value(s0(), 1, 1),
        Some(&Value::Error(ErrorKind::Na)),
        "$-anchored named whole-column absent key → #N/A"
    );
}

/// L2-A, the *actual* refusing corpus shape (workbook `89d88509…`, 1,959
/// cells): the lookup key cell (`T8`) is **blank** (a template row), the named
/// whole-column table's first column is all non-empty text — every populated
/// cell a confirmed NoMatch, every absent row a truly-blank cell OXP-104 pins
/// as NoMatch for a Blank key. OXP-104 H3 (RUN-2026-07-11-oracle01) pins
/// `VLOOKUP(<blank>, A:B, 2, FALSE)` over exactly this whole-column no-match
/// shape to `#N/A`, so `IF(ISERROR(VLOOKUP(…)),"",VLOOKUP(…))` computes `""` —
/// no longer the extra-conservative `#UNSUPPORTED!` defer.
#[test]
fn vlookup_blank_key_no_match_named_whole_column_computes_na() {
    let names = vec![DefinedName {
        name: "WC_ISIN_Lookup".to_string(),
        formula: "WC_Underlyings!$A:$B".to_string(),
        sheet_scope: None,
    }];
    let mut e = Engine::load(build_two_sheets(
        (
            "Sheet1",
            vec![
                // T8 (A1 here) is left blank: the template-row key.
                (
                    0,
                    1,
                    formula_cell(
                        "IF(ISERROR(VLOOKUP(A1,WC_ISIN_Lookup,2,)),\"\",VLOOKUP(A1,WC_ISIN_Lookup,2,))",
                    ),
                ),
                (1, 1, formula_cell("VLOOKUP(A2,WC_ISIN_Lookup,2,)")), // bare: #N/A
            ],
        ),
        (
            "WC_Underlyings",
            conv_table(&[
                (0, 0, txt("All")),
                (0, 1, txt("ISIN")),
                (1, 0, txt("Energy - Brent Crude Oil")),
                (1, 1, txt("XXCRUDEOIL")),
                (2, 0, txt("Energy - Coal")),
                (2, 1, txt("XXCOAL")),
            ]),
        ),
        names,
    ));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 1),
        Some(&txt("")),
        "blank key, no match: VLOOKUP → #N/A → ISERROR → \"\""
    );
    assert_eq!(
        e.value(s0(), 1, 1),
        Some(&Value::Error(ErrorKind::Na)),
        "bare blank-key no-match VLOOKUP over named whole column → #N/A (OXP-104 H3)"
    );
}

/// L2-A regression: MATCH exact over a whole-column and a whole-row range
/// reaching it through a defined name takes the used-extent walks (RFC 0001 /
/// RFC 0008) exactly like the direct-reference path — position is the absolute
/// (relative-to-range-top/left) index, not the compacted one.
#[test]
fn match_exact_named_whole_column_and_whole_row() {
    let names = vec![
        DefinedName {
            name: "KeyCol".to_string(),
            formula: "Data!$A:$A".to_string(),
            sheet_scope: None,
        },
        DefinedName {
            name: "KeyRow".to_string(),
            formula: "Data!$2:$2".to_string(),
            sheet_scope: None,
        },
    ];
    let mut e = Engine::load(build_two_sheets(
        (
            "Sheet1",
            vec![
                (0, 0, formula_cell("MATCH(\"k3\",KeyCol,0)")),
                (1, 0, formula_cell("MATCH(30,KeyRow,0)")),
            ],
        ),
        (
            "Data",
            conv_table(&[
                (0, 0, txt("k1")),
                (2, 0, txt("k3")), // A3 (gap at A2): absolute position 3
                (1, 1, num(10.0)), // B2
                (1, 3, num(30.0)), // D2 (gap at C2): absolute position 4
            ]),
        ),
        names,
    ));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&num(3.0)),
        "named whole-column exact MATCH → absolute row position"
    );
    assert_eq!(
        e.value(s0(), 1, 0),
        Some(&num(4.0)),
        "named whole-row exact MATCH → absolute column position"
    );
}

// Regression: a defined-name / cross-sheet BOUNDED table_array resolves to the
// correct cells — a text key genuinely absent from a numeric first column
// returns #N/A, and a present key returns the right row (no wrong-sheet or
// wrong-column resolution). Investigated for the reported ~88k-cell corpus
// mismatch; the resolution path is correct for every faithful reconstruction.
#[test]
fn vlookup_defined_name_cross_sheet_resolution_is_correct() {
    // DELIV_CONV = 'DD-EPM'!$AF$10:$AG$11 (quoted, hyphenated sheet; AF/AG cols).
    let names = vec![DefinedName {
        name: "DELIV_CONV".to_string(),
        formula: "'DD-EPM'!$AF$10:$AG$11".to_string(),
        sheet_scope: None,
    }];
    // Plant "Euro" on the CURRENT sheet at AF10 to catch wrong-sheet resolution.
    let cur = vec![
        (0, 0, formula_cell("VLOOKUP(\"Euro\",DELIV_CONV,2,FALSE)")),
        (1, 0, formula_cell("VLOOKUP(2,DELIV_CONV,2,FALSE)")),
        (9, 31, literal_cell(txt("Euro"))), // Sheet1!AF10 (the trap)
        (9, 32, literal_cell(num(999.0))),  // Sheet1!AG10
    ];
    let ddepm = vec![
        (9, 31, literal_cell(num(1.0))),
        (9, 32, literal_cell(num(10.0))), // AF10/AG10
        (10, 31, literal_cell(num(2.0))),
        (10, 32, literal_cell(num(20.0))), // AF11/AG11
    ];
    let mut e = Engine::load(build_two_sheets(("Sheet1", cur), ("DD-EPM", ddepm), names));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Na)),
        "text key absent from DD-EPM's numeric first column → #N/A (not the trap 999)"
    );
    assert_eq!(
        e.value(s0(), 1, 0),
        Some(&num(20.0)),
        "present key 2 resolves to DD-EPM AF11 → returns AG11 = 20"
    );
}

// ---- RFC 0002: nested-SUBTOTAL exclusion, end-to-end --------------------

#[test]
fn grand_total_subtotal_excludes_nested_subtotals() {
    // The canonical Data ▸ Subtotal layout in column A:
    //   A1=10, A2=20, A3=SUBTOTAL(9,A1:A2) = 30   (a sub-total)
    //   A4=100, A5=200, A6=SUBTOTAL(9,A4:A5) = 300 (a sub-total)
    //   A7=SUBTOTAL(9,A1:A6) = the grand total
    // A plain SUM of A1:A6 would be 10+20+30+100+200+300 = 660, double-counting
    // the sub-totals. Excel's nested-exclusion (RFC 0002) skips A3 and A6, so the
    // grand total is 10+20+100+200 = 330.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(10.0))),           // A1
        (1, 0, literal_cell(num(20.0))),           // A2
        (2, 0, formula_cell("SUBTOTAL(9,A1:A2)")), // A3 (sub-total)
        (3, 0, literal_cell(num(100.0))),          // A4
        (4, 0, literal_cell(num(200.0))),          // A5
        (5, 0, formula_cell("SUBTOTAL(9,A4:A5)")), // A6 (sub-total)
        (6, 0, formula_cell("SUBTOTAL(9,A1:A6)")), // A7 (grand total)
    ]));
    e.recalc();
    // The sub-totals compute plainly (their own ranges hold no nested SUBTOTALs).
    assert_eq!(
        e.value(s0(), 2, 0),
        Some(&num(30.0)),
        "A3 = SUBTOTAL(A1:A2)"
    );
    assert_eq!(
        e.value(s0(), 5, 0),
        Some(&num(300.0)),
        "A6 = SUBTOTAL(A4:A5)"
    );
    // The grand total excludes A3 and A6.
    assert_eq!(
        e.value(s0(), 6, 0),
        Some(&num(330.0)),
        "A7 grand total excludes the nested sub-totals (330, not the double-counted 660)"
    );
    assert!(
        e.diagnostics().is_empty(),
        "no refusals: this path is faithful now"
    );
}

#[test]
fn nested_subtotal_inside_a_range_arg_is_excluded_even_when_mixed_with_scalars() {
    // The sub-total lives *inside* a range argument (the Data ▸ Subtotal layout),
    // so it is excluded, while a separate raw scalar argument is still added:
    //   SUBTOTAL(9, A1:A3, B1)  with A3 a sub-total(=30), B1=5
    //   → range A1:A3 contributes 10+20 (A3 excluded) = 30, plus scalar B1=5 → 35.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(10.0))),              // A1
        (1, 0, literal_cell(num(20.0))),              // A2
        (2, 0, formula_cell("SUBTOTAL(9,A1:A2)")),    // A3 sub-total = 30
        (0, 1, literal_cell(num(5.0))),               // B1
        (4, 0, formula_cell("SUBTOTAL(9,A1:A3,B1)")), // A5
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 4, 0),
        Some(&num(35.0)),
        "range member A3 excluded (30), scalar B1 added (5) → 35"
    );
}

#[test]
fn nested_subtotal_as_lone_scalar_arg_is_excluded() {
    // RUN-2026-07-11-oracle01 / OXP-123: a nested SUBTOTAL referenced as a
    // *scalar* single-cell argument is now ALSO excluded (Excel's `=SUBTOTAL(9,A3)`
    // over a lone sub-total returns 0, not the double-counted value). This was the
    // former documented residual; `NestedSubtotalFilter::shape` closes it by
    // reclassifying a scalar arg whose provenance-tagged cell is a SUBTOTAL as
    // Omitted. Here `SUBTOTAL(9, A1, A3)` with A1=10 and A3 a sub-total(=30) drops
    // A3 → 10.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(10.0))),           // A1
        (1, 0, literal_cell(num(20.0))),           // A2
        (2, 0, formula_cell("SUBTOTAL(9,A1:A2)")), // A3 sub-total = 30
        (0, 1, formula_cell("SUBTOTAL(9,A1,A3)")), // B1: A3 passed as a scalar arg
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 1),
        Some(&num(10.0)),
        "scalar-arg nested SUBTOTAL A3 excluded (10 only, not the double-counted 40)"
    );
}

#[test]
fn subtotal_over_raw_range_unchanged_by_exclusion() {
    // Regression: a SUBTOTAL over a pure raw-data range (no nested SUBTOTALs) is
    // still the plain aggregate — the common case must be untouched.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(1.0))),
        (1, 0, literal_cell(num(2.0))),
        (2, 0, literal_cell(num(3.0))),
        (3, 0, formula_cell("SUBTOTAL(9,A1:A3)")),
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 3, 0),
        Some(&num(6.0)),
        "plain SUM of raw range = 6"
    );
}

#[test]
fn nested_subtotal_tag_updated_on_edit() {
    // Editing a cell into / out of a SUBTOTAL must keep the RFC 0002 tag set in
    // sync so the grand total re-includes / re-excludes it.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(10.0))),           // A1
        (1, 0, literal_cell(num(20.0))),           // A2
        (2, 0, formula_cell("SUBTOTAL(9,A1:A2)")), // A3 sub-total = 30
        (3, 0, formula_cell("SUBTOTAL(9,A1:A3)")), // A4 grand total, excludes A3
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 3, 0), Some(&num(30.0)), "excludes A3 → 30");

    // Turn A3 into a plain SUM (no longer a SUBTOTAL): it must now be *included*.
    e.edit(
        CellId::new(s0(), 2, 0),
        CellInput::Formula("SUM(A1:A2)".to_string()),
    );
    assert_eq!(
        e.value(s0(), 2, 0),
        Some(&num(30.0)),
        "A3 still 30 as a plain SUM"
    );
    assert_eq!(
        e.value(s0(), 3, 0),
        Some(&num(60.0)),
        "A3 no longer tagged → grand total re-includes it: 10+20+30 = 60"
    );

    // Turn A3 back into a SUBTOTAL: it must be excluded again.
    e.edit(
        CellId::new(s0(), 2, 0),
        CellInput::Formula("SUBTOTAL(9,A1:A2)".to_string()),
    );
    assert_eq!(
        e.value(s0(), 3, 0),
        Some(&num(30.0)),
        "A3 re-tagged → excluded again: 30"
    );
}

// ---- OXP-121: SUBTOTAL 101-111 exclude manually-hidden rows, end-to-end --

#[test]
fn subtotal_101_to_111_exclude_manually_hidden_rows() {
    // RUN-2026-07-11-oracle01 / OXP-121, exercised through the whole stack
    // io(model.hidden_rows) → engine(hidden_rows set) → EngineArgs::
    // for_each_cell_tagged(CellFlags::is_hidden_row) → func_subtotal's
    // NestedSubtotalFilter(exclude_hidden_rows):
    //   A1:A5 = 10,20,30,40,50 with row 3 (0-based 2) manually hidden.
    //   B1 = SUBTOTAL(109, A1:A5) excludes the hidden 30 → 120.
    //   B2 = SUBTOTAL(9,   A1:A5) includes it            → 150.
    //   B3 = SUBTOTAL(102, A1:A5) counts only visible    → 4.
    let hidden: std::collections::BTreeSet<u32> = [2u32].into_iter().collect();
    let wb = build_named_with_hidden(
        "Sheet1",
        vec![
            (0, 0, literal_cell(num(10.0))),
            (1, 0, literal_cell(num(20.0))),
            (2, 0, literal_cell(num(30.0))), // row 3, hidden
            (3, 0, literal_cell(num(40.0))),
            (4, 0, literal_cell(num(50.0))),
            (0, 1, formula_cell("SUBTOTAL(109,A1:A5)")),
            (1, 1, formula_cell("SUBTOTAL(9,A1:A5)")),
            (2, 1, formula_cell("SUBTOTAL(102,A1:A5)")),
        ],
        Vec::new(),
        hidden,
    );
    let mut e = Engine::load(wb);
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 1),
        Some(&num(120.0)),
        "SUBTOTAL(109) excludes manually-hidden row 3 → 120"
    );
    assert_eq!(
        e.value(s0(), 1, 1),
        Some(&num(150.0)),
        "SUBTOTAL(9) includes the hidden row → 150 (unchanged 1-11 behavior)"
    );
    assert_eq!(
        e.value(s0(), 2, 1),
        Some(&num(4.0)),
        "SUBTOTAL(102) counts only the 4 visible rows"
    );
    assert!(
        e.diagnostics().is_empty(),
        "101-111 now compute cleanly — no #UNSUPPORTED! diagnostic"
    );
}

#[test]
fn subtotal_101_to_111_with_no_hidden_rows_match_their_twins() {
    // Regression: with no hidden rows in the workbook, 109 and 9 agree (150).
    let wb = build(vec![
        (0, 0, literal_cell(num(10.0))),
        (1, 0, literal_cell(num(20.0))),
        (2, 0, literal_cell(num(30.0))),
        (3, 0, literal_cell(num(40.0))),
        (4, 0, literal_cell(num(50.0))),
        (0, 1, formula_cell("SUBTOTAL(109,A1:A5)")),
        (1, 1, formula_cell("SUBTOTAL(9,A1:A5)")),
    ]);
    let mut e = Engine::load(wb);
    e.recalc();
    assert_eq!(e.value(s0(), 0, 1), Some(&num(150.0)));
    assert_eq!(e.value(s0(), 1, 1), Some(&num(150.0)));
    assert!(e.diagnostics().is_empty());
}

// ---- RFC 0003: reference-returning OFFSET / INDIRECT (Phase 1) -----------
//
// Behavior traces to docs/specs/OFFSET.md and docs/specs/INDIRECT.md (which
// cite the Microsoft Learn OFFSET/INDIRECT pages). Deferred corners cite the
// OXP-140..144 family in docs/oracle-experiments.md.

#[test]
fn indirect_literal_reads_a_cell() {
    // INDIRECT.md §1: parses "A1" and returns a Ref; scalar-deref reads it.
    // A1 = 7 (literal); B1 = INDIRECT("A1") + 100 → 107.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(7.0))),
        (0, 1, formula_cell(r#"INDIRECT("A1")+100"#)),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 1), Some(&num(107.0)));
    assert!(e.diagnostics().is_empty());
}

#[test]
fn indirect_computed_ref_text_from_a_cell() {
    // Seam C, choice (b): ref_text may itself be a computed cell value. A1=5,
    // B1="A1" (text), C1 = INDIRECT(B1) → reads A1 → 5.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(5.0))),
        (0, 1, literal_cell(txt("A1"))),
        (0, 2, formula_cell("INDIRECT(B1)")),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 2), Some(&num(5.0)));
    assert!(e.diagnostics().is_empty());
}

#[test]
fn indirect_range_streams_as_a_range_into_sum() {
    // INDIRECT.md §1 (range consumption): SUM(INDIRECT("A1:A3")).
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(1.0))),
        (1, 0, literal_cell(num(2.0))),
        (2, 0, literal_cell(num(10.0))),
        (0, 1, formula_cell(r#"SUM(INDIRECT("A1:A3"))"#)),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 1), Some(&num(13.0)));
    assert!(e.diagnostics().is_empty());
}

#[test]
fn indirect_bad_text_is_ref_error() {
    // INDIRECT.md §Error: a string that does not parse as a reference → #REF!.
    let mut e = Engine::load(build(vec![(0, 0, formula_cell(r#"INDIRECT("1+1")"#))]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&Value::Error(ErrorKind::Ref)));
}

#[test]
fn indirect_nonexistent_sheet_is_ref_error() {
    // A single sheet name that does not exist → #REF! (not #UNSUPPORTED!).
    let mut e = Engine::load(build(vec![(
        0,
        0,
        formula_cell(r#"INDIRECT("NoSuch!A1")"#),
    )]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&Value::Error(ErrorKind::Ref)));
}

#[test]
fn indirect_cross_sheet_resolves() {
    // INDIRECT.md §5: a valid cross-sheet reference resolves. Sheet2!B1 = 42.
    let mut e = Engine::load(build_two_sheets(
        (
            "Sheet1",
            vec![(0, 0, formula_cell(r#"INDIRECT("Sheet2!B1")"#))],
        ),
        ("Sheet2", vec![(0, 1, literal_cell(num(42.0)))]),
        Vec::new(),
    ));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&num(42.0)));
    assert!(e.diagnostics().is_empty());
}

#[test]
fn indirect_r1c1_absolute_reads_a_cell() {
    // INDIRECT.md §4: a1=FALSE → R1C1 parsing. "R3C1" is absolute → A3 = 10.
    let mut e = Engine::load(build(vec![
        (2, 0, literal_cell(num(10.0))),
        (0, 1, formula_cell(r#"INDIRECT("R3C1",FALSE)"#)),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 1), Some(&num(10.0)));
    assert!(e.diagnostics().is_empty());
}

#[test]
fn indirect_r1c1_relative_resolves_against_anchor() {
    // RUN-2026-07-11-oracle01 / OXP-142: relative/partial R1C1 under a1=FALSE
    // resolves against the formula's own cell (the anchor). Each probe formula
    // sits in B3 (anchor row 2, col 1, 0-based); one workbook per probe since a
    // single anchor cell cannot host three formulas:
    //   R[1]C  → (anchor.row+1, anchor.col) = (3,1) = B4
    //   RC[1]  → (anchor.row,   anchor.col+1) = (2,2) = C3
    //   R5     → (absolute row 5 → 4, anchor.col) = (4,1) = B5 (implicit
    //            intersection of a whole-row ref with the anchor's column)
    let mut a = Engine::load(build(vec![
        (3, 1, literal_cell(num(111.0))),
        (2, 1, formula_cell(r#"INDIRECT("R[1]C",FALSE)"#)), // anchor B3 → B4 = 111
    ]));
    a.recalc();
    assert_eq!(a.value(s0(), 2, 1), Some(&num(111.0)), "R[1]C → B4");
    assert!(a.diagnostics().is_empty());

    let mut b = Engine::load(build(vec![
        (2, 2, literal_cell(num(213.0))),
        (2, 1, formula_cell(r#"INDIRECT("RC[1]",FALSE)"#)), // anchor B3 → C3 = 213
    ]));
    b.recalc();
    assert_eq!(b.value(s0(), 2, 1), Some(&num(213.0)), "RC[1] → C3");
    assert!(b.diagnostics().is_empty());

    let mut c = Engine::load(build(vec![
        (4, 1, literal_cell(num(550.0))),
        (2, 1, formula_cell(r#"INDIRECT("R5",FALSE)"#)), // anchor col 1 → B5 = 550
    ]));
    c.recalc();
    assert_eq!(
        c.value(s0(), 2, 1),
        Some(&num(550.0)),
        "R5 → B5 (anchor column)"
    );
    assert!(c.diagnostics().is_empty());
}

#[test]
fn indirect_r1c1_relative_off_edge_is_ref_error() {
    // OXP-142 corollary: a relative R1C1 that lands before row 1 → #REF!. Anchor
    // A1 (row 0); R[-1]C → row -1 → off the sheet.
    let mut e = Engine::load(build(vec![(
        0,
        0,
        formula_cell(r#"INDIRECT("R[-1]C",FALSE)"#),
    )]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&Value::Error(ErrorKind::Ref)));
}

#[test]
fn indirect_three_d_span_is_ref_error() {
    // RUN-2026-07-11-oracle01 / OXP-143: a 3-D span target (Sheet1:Sheet3!A1) →
    // #REF! (v1 single-workbook), not #UNSUPPORTED!.
    let mut e = Engine::load(build(vec![(
        0,
        0,
        formula_cell(r#"INDIRECT("Sheet1:Sheet3!A1")"#),
    )]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&Value::Error(ErrorKind::Ref)));
}

#[test]
fn indirect_external_workbook_is_ref_error() {
    // RUN-2026-07-11-oracle01 / OXP-143: another-workbook target ([Book2]Sheet1!A1)
    // → #REF! (parses as an unsupported bracket group; catch-all yields #REF!).
    let mut e = Engine::load(build(vec![(
        0,
        0,
        formula_cell(r#"INDIRECT("[Book2]Sheet1!A1")"#),
    )]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&Value::Error(ErrorKind::Ref)));
}

#[test]
fn offset_and_indirect_read_the_same_stored_target() {
    // RUN-2026-07-11-oracle01 / OXP-144 (Seam C, choice (b)): both OFFSET and
    // INDIRECT read the dynamic target's current value from the store. For a
    // faithfully-reproduced target (A1=2) they agree with each other and Excel.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(2.0))),
        (0, 1, formula_cell("OFFSET(A1,0,0)")),
        (0, 2, formula_cell(r#"INDIRECT("A1")"#)),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 1), Some(&num(2.0)), "OFFSET(A1,0,0) → 2");
    assert_eq!(e.value(s0(), 0, 2), Some(&num(2.0)), "INDIRECT(\"A1\") → 2");
    assert!(e.diagnostics().is_empty());
}

#[test]
fn offset_into_a_cell_used_in_arithmetic() {
    // OFFSET.md §1 (scalar-lift): OFFSET(A1,2,0) → A3 (=10); *2 → 20.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(1.0))),
        (1, 0, literal_cell(num(2.0))),
        (2, 0, literal_cell(num(10.0))),
        (0, 1, formula_cell("OFFSET(A1,2,0)*2")),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 1), Some(&num(20.0)));
    assert!(e.diagnostics().is_empty());
}

#[test]
fn offset_sized_range_streams_into_sum() {
    // OFFSET.md §1 (range consumption): base A1 (1×1), height 3 width 1 →
    // A1:A3; SUM = 1+2+10 = 13.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(1.0))),
        (1, 0, literal_cell(num(2.0))),
        (2, 0, literal_cell(num(10.0))),
        (0, 1, formula_cell("SUM(OFFSET(A1,0,0,3,1))")),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 1), Some(&num(13.0)));
    assert!(e.diagnostics().is_empty());
}

#[test]
fn offset_shifts_a_base_range() {
    // OFFSET.md §1: base A1:A3 (3×1) shifted right by 1 col → B1:B3 (default
    // dims). SUM(B1:B3) = 100+200+300 = 600.
    let mut e = Engine::load(build(vec![
        (0, 1, literal_cell(num(100.0))),
        (1, 1, literal_cell(num(200.0))),
        (2, 1, literal_cell(num(300.0))),
        (0, 3, formula_cell("SUM(OFFSET(A1:A3,0,1))")),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 3), Some(&num(600.0)));
    assert!(e.diagnostics().is_empty());
}

#[test]
fn offset_off_sheet_is_ref_error() {
    // OFFSET.md §3: shifting before row 1 → #REF!.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(1.0))),
        (0, 1, formula_cell("OFFSET(A1,-1,0)")),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 1), Some(&Value::Error(ErrorKind::Ref)));
}

#[test]
fn offset_huge_offset_argument_is_ref_not_panic() {
    // RFC 0003 review #2: a huge integer offset saturates `n as i64` to i64::MAX;
    // the bounds math must NOT overflow (it panics under overflow-checks/fuzz).
    // `1E19` is integer-valued so it isn't the OXP-140 non-integer defer; it just
    // lands far off-sheet → #REF!, computed in i128 without overflowing.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(1.0))),
        (0, 1, formula_cell("OFFSET(A1,1E19,0)")),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 1), Some(&Value::Error(ErrorKind::Ref)));
}

#[test]
fn indirect_defined_name_resolves() {
    // RFC 0003 review #3: INDIRECT of a defined name must route through the name
    // table (RFC §INDIRECT specifics), not blanket-#REF!. `TaxRate` → Sheet1!$A$1
    // = 0.05; INDIRECT("TaxRate")*1000 → 50.
    let names = vec![DefinedName {
        name: "TaxRate".to_string(),
        formula: "Sheet1!$A$1".to_string(),
        sheet_scope: None,
    }];
    let mut e = Engine::load(build_named(
        "Sheet1",
        vec![
            (0, 0, literal_cell(num(0.05))),
            (1, 0, formula_cell(r#"INDIRECT("TaxRate")*1000"#)),
        ],
        names,
    ));
    e.recalc();
    assert_eq!(e.value(s0(), 1, 0), Some(&num(50.0)));
}

#[test]
fn indirect_undefined_name_is_ref_error() {
    // A name that resolves to nothing → #REF! (Excel's result for INDIRECT of an
    // unresolvable name), distinct from a *defined* name which now resolves.
    let mut e = Engine::load(build(vec![(
        0,
        0,
        formula_cell(r#"INDIRECT("NoSuchName")"#),
    )]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&Value::Error(ErrorKind::Ref)));
}

#[test]
fn offset_non_integer_arg_truncates_toward_zero() {
    // RUN-2026-07-11-oracle01 / OXP-140: a fractional rows/cols/height/width is
    // truncated toward zero (not deferred, not rounded). Observed: OFFSET(A1,1.5,0)
    // → row+1, SUM(OFFSET(A1,0,0,2.9,1)) summed two rows (2.9→2); the sole negative
    // probe OFFSET(A1,-1.5,0) lands off the top edge → #REF!. A1=100, A2=200.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(100.0))),
        (1, 0, literal_cell(num(200.0))),
        (0, 2, formula_cell("OFFSET(A1,1.5,0)")),
        (1, 2, formula_cell("OFFSET(A1,-1.5,0)")),
        (2, 2, formula_cell("SUM(OFFSET(A1,0,0,2.9,1))")),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 2), Some(&num(200.0)), "1.5 → row+1 (A2)");
    assert_eq!(
        e.value(s0(), 1, 2),
        Some(&Value::Error(ErrorKind::Ref)),
        "-1.5 off the top edge → #REF!"
    );
    assert_eq!(
        e.value(s0(), 2, 2),
        Some(&num(300.0)),
        "height 2.9 → 2 rows: A1+A2 = 300"
    );
    assert!(e.diagnostics().is_empty());
}

#[test]
fn offset_zero_or_negative_size_is_ref_error() {
    // RUN-2026-07-11-oracle01 / OXP-141: a zero or negative height/width → #REF!
    // (observed 0, -2, and width 0 all as #REF!), not #UNSUPPORTED!.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(1.0))),
        (0, 2, formula_cell("SUM(OFFSET(A1,0,0,0,1))")),
        (1, 2, formula_cell("SUM(OFFSET(A1,0,0,-2,1))")),
        (2, 2, formula_cell("SUM(OFFSET(A1,0,0,1,0))")),
    ]));
    e.recalc();
    for row in 0..=2 {
        assert_eq!(
            e.value(s0(), row, 2),
            Some(&Value::Error(ErrorKind::Ref)),
            "non-positive height/width → #REF! (row {row})"
        );
    }
}

#[test]
fn offset_non_reference_base_is_unsupported() {
    // OFFSET.md §5: the reference argument must be a real reference; a bare
    // value → deferred (error kind unconfirmed) rather than guessed.
    let mut e = Engine::load(build(vec![(0, 0, formula_cell("OFFSET(5,1,1)"))]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
}

#[test]
fn offset_multi_cell_in_scalar_context_is_unsupported() {
    // A multi-cell OFFSET result used as a bare scalar stays deferred: legacy
    // implicit intersection is implemented for *literal* ranges (OXP-163, see
    // the tests below) but the *computed* (OFFSET/INDIRECT) reference case is
    // unprobed, so it remains `#UNSUPPORTED!` rather than being guessed.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(1.0))),
        (0, 1, formula_cell("OFFSET(A1,0,0,3,1)")),
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 1),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
}

// ---- legacy implicit intersection (OXP-004/163, RUN-2026-07-11-oracle01) ----
//
// A multi-cell *literal* range in scalar context of a **non-array-entered**
// formula reduces to the single cell at the intersection of the range and the
// FORMULA's own row/column. Farm-pinned: with A1=10, A2=20, A3=30, `=A1:A3+1`
// is 11 in row 1, 21 in row 2, 31 in row 3, and #VALUE! in row 10 (outside the
// range's rows). An **array-entered** `{=A1:A3+1}` is exempt: it is NOT
// intersected.

/// Build A1:A3 = 10/20/30 plus a `=A1:A3+1` formula at column B, row
/// `anchor_row0` (0-based), recalc, and return B's value.
fn implicit_intersect_a1a3_plus1(anchor_row0: u32) -> Value {
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(10.0))),
        (1, 0, literal_cell(num(20.0))),
        (2, 0, literal_cell(num(30.0))),
        (anchor_row0, 1, formula_cell("A1:A3+1")),
    ]));
    e.recalc();
    e.value(s0(), anchor_row0, 1)
        .cloned()
        .expect("formula cell has a value")
}

#[test]
fn implicit_intersection_single_column_pins_11_21_31() {
    // OXP-163: the formula's row selects the intersecting cell.
    assert_eq!(implicit_intersect_a1a3_plus1(0), num(11.0)); // row 1 → A1
    assert_eq!(implicit_intersect_a1a3_plus1(1), num(21.0)); // row 2 → A2
    assert_eq!(implicit_intersect_a1a3_plus1(2), num(31.0)); // row 3 → A3
}

#[test]
fn implicit_intersection_row_outside_range_is_value_error() {
    // OXP-163: row 10 is outside the range's rows [1,3] → #VALUE!.
    assert_eq!(
        implicit_intersect_a1a3_plus1(9),
        Value::Error(ErrorKind::Value)
    );
}

#[test]
fn array_entered_formula_is_not_implicitly_intersected() {
    // The array-formula exemption (the v2 fix): an array-entered `{=A1:A3+1}`
    // in a single cell must NOT do implicit intersection. With no dynamic-array
    // spill in v1, the multi-cell range in scalar array context keeps its
    // pre-OXP-163 meaning → #UNSUPPORTED! (never 11/21/31, never #VALUE!).
    //
    // This is the exact regression the unconditional v1 caused (~20k-cell corpus
    // mismatch): CSE array formulas were being intersected. Anchoring the cell
    // at row 1 (col B) — where the non-array formula would have yielded 21 — and
    // asserting #UNSUPPORTED! proves the array path is genuinely exempt (i.e. it
    // is NOT taking the 21 branch).
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(10.0))),
        (1, 0, literal_cell(num(20.0))),
        (2, 0, literal_cell(num(30.0))),
        (1, 1, array_formula_cell("A1:A3+1", "B2")), // {=A1:A3+1} at B2 (row 2)
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 1, 1),
        Some(&Value::Error(ErrorKind::Unsupported)),
        "an array-entered range in scalar context is not intersected"
    );

    // Sanity: the SAME text, NON-array-entered, at the SAME cell DOES intersect
    // (→ 21). This isolates the array-entry flag as the sole cause of the
    // difference.
    let mut e2 = Engine::load(build(vec![
        (0, 0, literal_cell(num(10.0))),
        (1, 0, literal_cell(num(20.0))),
        (2, 0, literal_cell(num(30.0))),
        (1, 1, formula_cell("A1:A3+1")), // =A1:A3+1 at B2 (row 2) → A2+1 = 21
    ]));
    e2.recalc();
    assert_eq!(e2.value(s0(), 1, 1), Some(&num(21.0)));
}

#[test]
fn array_entered_1x1_range_still_lifts() {
    // An array-entered formula still lifts a 1×1 range to its cell (the array
    // path's 1×1 case), so trivial CSE formulas keep working. `{=A1:A1+1}` at B2
    // → A1(10)+1 = 11, regardless of array-entry.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(10.0))),
        (1, 1, array_formula_cell("A1:A1+1", "B2")),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 1, 1), Some(&num(11.0)));
}

#[test]
fn implicit_intersection_single_row_pins_by_column() {
    // Symmetric rule: A6:C6 = 10/20/30 (row 5); `=A6:C6+1` intersects on the
    // formula's COLUMN. Formulas at row 1, cols A/B/C → 11/21/31; col E → #VALUE!.
    let mut e = Engine::load(build(vec![
        (5, 0, literal_cell(num(10.0))),
        (5, 1, literal_cell(num(20.0))),
        (5, 2, literal_cell(num(30.0))),
        (0, 0, formula_cell("A6:C6+1")), // col A → A6
        (0, 1, formula_cell("A6:C6+1")), // col B → B6
        (0, 2, formula_cell("A6:C6+1")), // col C → C6
        (0, 4, formula_cell("A6:C6+1")), // col E (outside) → #VALUE!
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&num(11.0)));
    assert_eq!(e.value(s0(), 0, 1), Some(&num(21.0)));
    assert_eq!(e.value(s0(), 0, 2), Some(&num(31.0)));
    assert_eq!(e.value(s0(), 0, 4), Some(&Value::Error(ErrorKind::Value)));
}

#[test]
fn implicit_intersection_in_comparison_operator() {
    // A comparison is a scalar context too: `=A1:A3=20` intersects on the row,
    // then compares. At B1 (row 1) → A1=10 → FALSE; at B2 (row 2) → A2=20 → TRUE.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(10.0))),
        (1, 0, literal_cell(num(20.0))),
        (2, 0, literal_cell(num(30.0))),
        (0, 1, formula_cell("A1:A3=20")), // B1
        (1, 1, formula_cell("A1:A3=20")), // B2
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 1), Some(&Value::Bool(false)));
    assert_eq!(e.value(s0(), 1, 1), Some(&Value::Bool(true)));
}

#[test]
fn sum_of_range_unchanged_by_implicit_intersection() {
    // Aggregate (range-arg) context is NOT scalar: SUM(A1:A3) still sums all
    // three cells (=60), independent of the formula's row — even at row 10,
    // where the scalar `=A1:A3+1` would be #VALUE!.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(10.0))),
        (1, 0, literal_cell(num(20.0))),
        (2, 0, literal_cell(num(30.0))),
        (9, 1, formula_cell("SUM(A1:A3)")), // row 10, outside the range
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 9, 1), Some(&num(60.0)));
}

#[test]
fn implicit_intersection_2d_range_scalar_context_deferred() {
    // A 2-D range (both axes > 1) in scalar context is unprobed → #UNSUPPORTED!.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(1.0))),
        (0, 1, literal_cell(num(2.0))),
        (1, 0, literal_cell(num(3.0))),
        (1, 1, literal_cell(num(4.0))),
        (5, 5, formula_cell("A1:B2+1")),
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 5, 5),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
}

#[test]
fn offset_cell_reschedules_every_recalc() {
    // OFFSET is volatile: its cell recomputes on every recalc even when its
    // static precedents are unchanged.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(1.0))),
        (0, 1, formula_cell("OFFSET(A1,0,0)")),
        (0, 2, literal_cell(num(9.0))),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 1), Some(&num(1.0)));
    // Editing the unrelated C1 must still reschedule the volatile OFFSET cell.
    e.edit(CellId::new(s0(), 0, 2), CellInput::Literal(num(8.0)));
    assert!(
        e.last_recalc_cells().contains(&CellId::new(s0(), 0, 1)),
        "the OFFSET cell must recompute on every recalc"
    );
}

#[test]
fn nested_offset_indirect_resolves() {
    // RFC 0003 open question: a reference-returning result feeding another.
    // INDIRECT("A1") is the base of OFFSET(...,1,0) → A2 = 2.
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(1.0))),
        (1, 0, literal_cell(num(2.0))),
        (0, 1, formula_cell(r#"OFFSET(INDIRECT("A1"),1,0)"#)),
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 1), Some(&num(2.0)));
    assert!(e.diagnostics().is_empty());
}

#[test]
fn indirect_reference_intersection_text_is_unsupported() {
    // A reference intersection string is a reference expression Excel computes,
    // but the engine does not resolve intersection anywhere yet → defer, not a
    // guessed #REF!.
    let mut e = Engine::load(build(vec![(
        0,
        0,
        formula_cell(r#"INDIRECT("A1:A5 A3:A9")"#),
    )]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
}

#[test]
fn paren_wrapped_range_arg_is_aggregated_not_intersected() {
    // Contentious-areas audit (2026-07-13), eval.rs:457: a paren-wrapped range
    // argument, e.g. =SUM((A1:A5)), was classified Aggregate by eff_shape
    // (arg_ref_extent unwraps Paren) but for_each_cell/dims/classify_shape had no
    // Paren arm, so the aggregate walk streamed ONE implicitly-intersected cell —
    // a silently-wrong, formula-position-dependent total (3 at C3, #VALUE! at C10).
    // Parentheses are pure grouping: SUM((A1:A5)) == SUM(A1:A5) == 15 at every
    // position. Values pinned by OXP-186 (RUN 2026-07-12, Excel 16.0, job
    // 9173aa7f): SUM((A1:A5))=15 on- AND off-band, MAX((A1:A5))=5, COUNT=5,
    // SUM((A1))=1, SUM(((A1:A5)))=15. The union-in-parens SUM((A1:A5,C1:C3))
    // Excel-pins to 75, but the union operator is a separate unimplemented
    // feature so recalc defers to a LOUD #UNSUPPORTED! (never silently wrong).
    let cells = vec![
        (0, 0, formula_cell("1")),
        (1, 0, formula_cell("2")),
        (2, 0, formula_cell("3")),
        (3, 0, formula_cell("4")),
        (4, 0, formula_cell("5")),
        (0, 2, formula_cell("10")),
        (1, 2, formula_cell("20")),
        (2, 2, formula_cell("SUM((A1:A5))")), // C3 — inside the row-band
        (9, 2, formula_cell("SUM((A1:A5))")), // C10 — outside the row-band
        (2, 3, formula_cell("SUM(A1:A5)")),   // D3 — control: plain range
        (2, 4, formula_cell("MAX((A1:A5))")), // E3 — a second aggregate consumer
        (2, 5, formula_cell("COUNT((A1:A5))")), // F3
        (2, 6, formula_cell("SUM((A1))")),    // G3 — single-cell paren
        (2, 7, formula_cell("SUM(((A1:A5)))")), // H3 — double-paren
        (2, 8, formula_cell("SUM((A1:A5,C2:C3))")), // I3 — union-in-parens (loud defer)
    ];
    let mut e = Engine::load(build(cells));
    e.recalc();
    assert_eq!(e.value(s0(), 2, 2), Some(&num(15.0)), "SUM((A1:A5)) @C3");
    assert_eq!(
        e.value(s0(), 9, 2),
        Some(&num(15.0)),
        "SUM((A1:A5)) @C10 (off-band)"
    );
    assert_eq!(e.value(s0(), 2, 3), Some(&num(15.0)), "SUM(A1:A5) control");
    assert_eq!(e.value(s0(), 2, 4), Some(&num(5.0)), "MAX((A1:A5))");
    assert_eq!(e.value(s0(), 2, 5), Some(&num(5.0)), "COUNT((A1:A5))");
    assert_eq!(
        e.value(s0(), 2, 6),
        Some(&num(1.0)),
        "SUM((A1)) single-cell paren"
    );
    assert_eq!(
        e.value(s0(), 2, 7),
        Some(&num(15.0)),
        "SUM(((A1:A5))) double-paren"
    );
    // Union operator unimplemented → loud #UNSUPPORTED!, not a silent wrong total.
    assert_eq!(
        e.value(s0(), 2, 8),
        Some(&Value::Error(ErrorKind::Unsupported)),
        "SUM((union)) defers loudly"
    );
}

// ---- OXP-169: implicit-intersection reductions (RUN-2026-07-13) ----------
// The 4 routes the 2026-07-12 the contract review flagged as confident-but-unpinned
// are now farm-pinned: every 1-D reduction intersects (by row for a single
// column, by column for a single row) and yields #VALUE! off-band — recalc's
// long-standing behavior is correct, NOT the top/left cell ROW/COLUMN take.

#[test]
fn oxp169_scalar_fn_arg_intersects_by_row() {
    // =ABS(A1:A3) intersects the formula's ROW (H2 -> ABS(A2)=20), NOT the top
    // cell (would be 10 everywhere). Off-band row -> #VALUE!. (OXP-169 b.)
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(10.0))),
        (1, 0, literal_cell(num(20.0))),
        (2, 0, literal_cell(num(30.0))),
        (0, 7, formula_cell("ABS(A1:A3)")), // H1
        (1, 7, formula_cell("ABS(A1:A3)")), // H2
        (2, 7, formula_cell("ABS(A1:A3)")), // H3
        (4, 7, formula_cell("ABS(A1:A3)")), // H5 (off-band)
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 7), Some(&num(10.0)));
    assert_eq!(
        e.value(s0(), 1, 7),
        Some(&num(20.0)),
        "intersect by row, not top"
    );
    assert_eq!(e.value(s0(), 2, 7), Some(&num(30.0)));
    assert_eq!(e.value(s0(), 4, 7), Some(&Value::Error(ErrorKind::Value)));
}

#[test]
fn oxp169_single_row_transpose_intersects_by_column() {
    // =A6:C6+1 intersects the formula's COLUMN (B8 -> B6+1=201). Off-band
    // column -> #VALUE!. (OXP-169 a; column-axis analogue of OXP-163.)
    let mut e = Engine::load(build(vec![
        (5, 0, literal_cell(num(100.0))), // A6
        (5, 1, literal_cell(num(200.0))), // B6
        (5, 2, literal_cell(num(300.0))), // C6
        (7, 0, formula_cell("A6:C6+1")),  // A8
        (7, 1, formula_cell("A6:C6+1")),  // B8
        (7, 2, formula_cell("A6:C6+1")),  // C8
        (7, 4, formula_cell("A6:C6+1")),  // E8 (off-band col)
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 7, 0), Some(&num(101.0)));
    assert_eq!(
        e.value(s0(), 7, 1),
        Some(&num(201.0)),
        "intersect by column"
    );
    assert_eq!(e.value(s0(), 7, 2), Some(&num(301.0)));
    assert_eq!(e.value(s0(), 7, 4), Some(&Value::Error(ErrorKind::Value)));
}

#[test]
fn oxp169_named_range_intersects_by_row() {
    // =MyName+1 (MyName -> A1:A3) intersects the formula's ROW (J2 -> A2+1=21).
    // Off-band -> #VALUE!. (OXP-169 c.)
    let names = vec![DefinedName {
        name: "MyName".to_string(),
        formula: "Sheet1!$A$1:$A$3".to_string(),
        sheet_scope: None,
    }];
    let mut e = Engine::load(build_named(
        "Sheet1",
        vec![
            (0, 0, literal_cell(num(10.0))),
            (1, 0, literal_cell(num(20.0))),
            (2, 0, literal_cell(num(30.0))),
            (0, 9, formula_cell("MyName+1")), // J1
            (1, 9, formula_cell("MyName+1")), // J2
            (4, 9, formula_cell("MyName+1")), // J5 (off-band)
        ],
        names,
    ));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 9), Some(&num(11.0)));
    assert_eq!(
        e.value(s0(), 1, 9),
        Some(&num(21.0)),
        "named range intersects by row"
    );
    assert_eq!(e.value(s0(), 4, 9), Some(&Value::Error(ErrorKind::Value)));
}

#[test]
fn oxp169_cross_sheet_intersects_by_formula_row() {
    // =Data!A1:A3+1 on Sheet1 intersects the formula's ROW applied to the source
    // sheet (L2 -> Data!A2+1=2001). Off-band -> #VALUE!. (OXP-169 d.)
    let mut e = Engine::load(build_two_sheets(
        (
            "Sheet1",
            vec![
                (0, 11, formula_cell("Data!A1:A3+1")), // L1
                (1, 11, formula_cell("Data!A1:A3+1")), // L2
                (4, 11, formula_cell("Data!A1:A3+1")), // L5 (off-band)
            ],
        ),
        (
            "Data",
            vec![
                (0, 0, literal_cell(num(1000.0))),
                (1, 0, literal_cell(num(2000.0))),
                (2, 0, literal_cell(num(3000.0))),
            ],
        ),
        vec![],
    ));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 11), Some(&num(1001.0)));
    assert_eq!(
        e.value(s0(), 1, 11),
        Some(&num(2001.0)),
        "cross-sheet intersects by formula row"
    );
    assert_eq!(e.value(s0(), 4, 11), Some(&Value::Error(ErrorKind::Value)));
}

#[test]
fn consumed_array_eval_computes_sum_idioms_subtotal_still_refuses() {
    // M2 lane 6 (OXP-201): the RFC-0011 array-context gate no longer refuses a
    // BOUNDED multi-cell range in a SUM value argument — it materializes the
    // range into a `Value::Array`, the operators / IF / ISBLANK / NOT broadcast
    // element-wise, and SUM folds the result. So the four previously-refused
    // idioms now compute their Excel values. Bare ranges, lazy untaken branches,
    // and nested range-native calls stay UNTOUCHED; SUBTOTAL (NOT the pinned SUM
    // aggregator) still refuses the materialized array even though its delegate
    // shims forward the array-context flag — the SUM-only born-refusing boundary.
    let cells = vec![
        (0, 0, formula_cell("1")), // A1
        (1, 0, formula_cell("2")), // A2
        (2, 0, formula_cell("3")), // A3
        (3, 0, formula_cell("4")), // A4
        (4, 0, formula_cell("5")), // A5
        // --- KEEP (correct today, must stay correct) ---
        (0, 2, formula_cell("SUM(A1:A5)")), // C1 bare range -> 15
        (1, 2, formula_cell("SUM((A1:A5))")), // C2 RFC-0010 paren -> 15
        (2, 2, formula_cell("SUM(IF(TRUE,5,A1:A5))")), // C3 lazy: else never eval -> 5
        (3, 2, formula_cell("SUM(A1*2)")),  // C4 single-cell op -> 2
        (4, 2, formula_cell("SUM(SUBTOTAL(9,A1:A5))")), // C5 nested range-native -> 15
        // --- NOW COMPUTED (consumed-array eval; were #UNSUPPORTED! under RFC-0011) ---
        (5, 2, formula_cell("SUM(A1:A5*2)")), // C6 rule (a): (1..5)*2 -> 30
        (6, 2, formula_cell("SUM(IF(NOT(ISBLANK(A1:A5)),A1:A5,0))")), // C7 idiom -> 15
        (7, 2, formula_cell("SUM(IF(ISBLANK(A1:A5),0,1))")), // C8 all non-blank -> 5
        (8, 2, formula_cell("SUM(IF(A1>0,A1:A5))")), // C9 scalar cond picks range -> 15
        // --- STILL REFUSE: SUBTOTAL is not the SUM aggregator (SUM-only landing).
        // The shims forward eval_scalar_array_arg AND array_arg_ctx, so the
        // materialized/broadcast array reaches SUBTOTAL, which does NOT fold it
        // (keeps CoercionMode::Scalar) -> #UNSUPPORTED!. Guards both overrides. ---
        (9, 2, formula_cell("SUBTOTAL(9,A1:A3*2)")), // C10 OffsetArgs shim
        (10, 2, formula_cell("SUBTOTAL(109,A1:A3*2)")), // C11 NestedSubtotalFilter shim
    ];
    let mut e = Engine::load(build(cells));
    e.recalc();
    let uns = Some(&Value::Error(ErrorKind::Unsupported));
    // keeps
    assert_eq!(e.value(s0(), 0, 2), Some(&num(15.0)), "SUM(A1:A5)");
    assert_eq!(e.value(s0(), 1, 2), Some(&num(15.0)), "SUM((A1:A5))");
    assert_eq!(
        e.value(s0(), 2, 2),
        Some(&num(5.0)),
        "SUM(IF(TRUE,5,range)) lazy"
    );
    assert_eq!(e.value(s0(), 3, 2), Some(&num(2.0)), "SUM(A1*2)");
    assert_eq!(
        e.value(s0(), 4, 2),
        Some(&num(15.0)),
        "SUM(SUBTOTAL(9,A1:A5)) nested"
    );
    // now computed (consumed-array eval)
    assert_eq!(e.value(s0(), 5, 2), Some(&num(30.0)), "SUM(A1:A5*2) = 30");
    assert_eq!(
        e.value(s0(), 6, 2),
        Some(&num(15.0)),
        "SUM(IF(NOT(ISBLANK(range)),range,0)) = 15"
    );
    assert_eq!(
        e.value(s0(), 7, 2),
        Some(&num(5.0)),
        "SUM(IF(ISBLANK(range),0,1)) = 5"
    );
    assert_eq!(
        e.value(s0(), 8, 2),
        Some(&num(15.0)),
        "SUM(IF(A1>0,range)) = 15"
    );
    // SUBTOTAL still refuses through BOTH delegate shims (SUM-only fold).
    assert_eq!(
        e.value(s0(), 9, 2),
        uns,
        "SUBTOTAL(9,range*2) OffsetArgs shim refuses"
    );
    assert_eq!(
        e.value(s0(), 10, 2),
        uns,
        "SUBTOTAL(109,range*2) NestedSubtotalFilter shim refuses"
    );
}

// ── M2 lane-6 consumed-array evaluation (OXP-201) — engine-level pins ──────────

/// The RFC-0011 residual idiom with a numeric `0` else-branch (OXP-201 #10):
/// `SUM(IF(range>2, range, 0))`. A1:A5 = [1,2,3,4,5] → [0,0,3,4,5] → 12.
#[test]
fn consumed_array_sum_if_gt_selects_branches() {
    let cells = vec![
        (0, 0, formula_cell("1")),
        (1, 0, formula_cell("2")),
        (2, 0, formula_cell("3")),
        (3, 0, formula_cell("4")),
        (4, 0, formula_cell("5")),
        (0, 2, formula_cell("SUM(IF(A1:A5>2,A1:A5,0))")),
    ];
    let mut e = Engine::load(build(cells));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 2), Some(&num(12.0)));
}

/// Rule (a) scalar ↔ array in both operand orders: `SUM(range*2)` and
/// `SUM(2*range)` both = 30 over [1..5].
#[test]
fn consumed_array_sum_range_times_scalar() {
    let cells = vec![
        (0, 0, formula_cell("1")),
        (1, 0, formula_cell("2")),
        (2, 0, formula_cell("3")),
        (3, 0, formula_cell("4")),
        (4, 0, formula_cell("5")),
        (0, 2, formula_cell("SUM(A1:A5*2)")),
        (1, 2, formula_cell("SUM(2*A1:A5)")),
    ];
    let mut e = Engine::load(build(cells));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 2), Some(&num(30.0)), "range*2");
    assert_eq!(e.value(s0(), 1, 2), Some(&num(30.0)), "2*range");
}

/// The canonical FUSE composition end to end: `SUM(NOT(ISBLANK(range))*range)`
/// with an interior blank. B1=10, B2 blank, B3=30 → NOT(ISBLANK)=[T,F,T],
/// times [10,0,30] → [10,0,30] → 40. Exercises ISBLANK + NOT element-wise and
/// same-shape elementwise multiply (rule b) with a materialized blank.
#[test]
fn consumed_array_not_isblank_times_range() {
    let cells = vec![
        (0, 1, formula_cell("10")), // B1
        // B2 (row 1, col 1) intentionally absent → Blank
        (2, 1, formula_cell("30")),                             // B3
        (0, 3, formula_cell("SUM(NOT(ISBLANK(B1:B3))*B1:B3)")), // D1
    ];
    let mut e = Engine::load(build(cells));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 3), Some(&num(40.0)));
}

/// The dominant corpus variant uses a **text `""`** false-branch (spec §R1):
/// `SUM(IF(NOT(ISBLANK(range)),range,""))`. The `""` element is skipped by the
/// `RangeAggregate` fold exactly as text-in-a-range is (OXP-006 reuse), so the
/// result is still a number. B1=10, B2 blank, B3=30 → [10,"",30] → 40.
#[test]
fn consumed_array_empty_string_false_branch_skips() {
    let cells = vec![
        (0, 1, formula_cell("10")), // B1
        // B2 absent → Blank → ISBLANK true → false-branch "" selected
        (2, 1, formula_cell("30")), // B3
        (
            0,
            3,
            formula_cell("SUM(IF(NOT(ISBLANK(B1:B3)),B1:B3,\"\"))"),
        ), // D1
    ];
    let mut e = Engine::load(build(cells));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 3), Some(&num(40.0)));
}

/// Rule (d): two column vectors of unequal length pad the shorter with `#N/A`,
/// which propagates through SUM. `SUM(A1:A3+A1:A2)` over [1,2,3]/[1,2] →
/// [2,4,#N/A] → `#N/A` (OXP-201 #4).
#[test]
fn consumed_array_length_mismatch_propagates_na() {
    let cells = vec![
        (0, 0, formula_cell("1")),
        (1, 0, formula_cell("2")),
        (2, 0, formula_cell("3")),
        (0, 2, formula_cell("SUM(A1:A3+A1:A2)")),
    ];
    let mut e = Engine::load(build(cells));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 2), Some(&Value::Error(ErrorKind::Na)));
}

/// Born-refusing boundary #1: a **whole-column** range in the array-context
/// idiom stays `#UNSUPPORTED!` (the gate refuses the unbounded range; defers
/// FUSE variant #6). `SUM(IF(A:A>2,A:A,0))`.
#[test]
fn consumed_array_whole_column_still_refuses() {
    let cells = vec![
        (0, 0, formula_cell("1")),
        (1, 0, formula_cell("2")),
        (2, 0, formula_cell("3")),
        (0, 2, formula_cell("SUM(IF(A:A>2,A:A,0))")),
    ];
    let mut e = Engine::load(build(cells));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 2),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
}

/// Born-refusing boundary #2: SUM is the ONLY OXP-201-pinned aggregator to fold
/// a consumed array. A different aggregator receiving the materialized array
/// stays `#UNSUPPORTED!` (defers FUSE variants #6/#7). `MAX(IF(range>2,range,0))`.
#[test]
fn consumed_array_non_sum_aggregator_still_refuses() {
    let cells = vec![
        (0, 0, formula_cell("1")),
        (1, 0, formula_cell("2")),
        (2, 0, formula_cell("3")),
        (3, 0, formula_cell("4")),
        (4, 0, formula_cell("5")),
        (0, 2, formula_cell("MAX(IF(A1:A5>2,A1:A5,0))")),
    ];
    let mut e = Engine::load(build(cells));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 2),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
}

/// Born-refusing boundary #2 (COUNTA regression): COUNTA is an RFC-0011 seam
/// aggregator but, unlike MAX/MIN/AVERAGE/…, does NOT route its scalar arm
/// through `coerce_number_arg`. Before the explicit refuse guard, a consumed
/// array (not an error, not blank) silently miscounted as 1 — an
/// `unsupported → mismatch` Principle-2 violation (Excel counts the elements,
/// which is UNPINNED for COUNTA; only SUM's fold is pinned by OXP-201). Both the
/// broadcast form and the SUM(IF())-shaped form must stay `#UNSUPPORTED!`.
#[test]
fn consumed_array_counta_still_refuses() {
    // (a) COUNTA over a broadcast consumed array.
    let cells = vec![
        (0, 0, formula_cell("1")),
        (1, 0, formula_cell("2")),
        (2, 0, formula_cell("3")),
        (0, 2, formula_cell("COUNTA(A1:A3*2)")),
    ];
    let mut e = Engine::load(build(cells));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 2),
        Some(&Value::Error(ErrorKind::Unsupported)),
        "COUNTA(A1:A3*2) must refuse a consumed array, not miscount it as 1"
    );

    // (b) COUNTA over the SUM(IF())-shaped consumed array (the corpus idiom's
    // shape applied to COUNTA).
    let cells = vec![
        (0, 0, formula_cell("1")),
        (1, 0, formula_cell("2")),
        (2, 0, formula_cell("3")),
        (
            0,
            2,
            formula_cell("COUNTA(IF(NOT(ISBLANK(A1:A3)),A1:A3,\"\"))"),
        ),
    ];
    let mut e = Engine::load(build(cells));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 2),
        Some(&Value::Error(ErrorKind::Unsupported)),
        "COUNTA(IF(...)) must refuse a consumed array, not miscount it as 1"
    );
}

// ── SUMPRODUCT consumed-array arguments — the SAME array-context gate as SUM ──
//
// A *computed* SUMPRODUCT argument (an operator applied to a range) is an
// array-position argument: Excel array-evaluates it in every version, never
// implicit-intersects it. Pins: OXP-201 #6 `SUMPRODUCT(SEQUENCE(3),SEQUENCE(3))`
// = 14 and #9 `SUMPRODUCT(--(SEQUENCE(3)>1))` = 2 (computed arrays in
// SUMPRODUCT positions; comparison → bool array → `--` coercion; a single
// array sums its elements), composed with the broadcasting rules (a) OXP-201
// #1 and (b) OXP-201 #2/#7 over a consumed range materialized by the RFC-0011
// gate (`docs/plans/2026-07-14-consumed-array-eval-spec.md` §2a/§3).
//
// Fixture: A1:A7 = ab, ac, bx, -cd, cz, ce, df; B1:B7 = 1..7. The host cell is
// D1 (in-band with the ranges' rows — the position where legacy implicit
// intersection silently picked row 1) or D9 (off-band — where it gave the
// OXP-163 `#VALUE!`). Both hosts must agree: array evaluation is host-independent.

/// Build the SUMPRODUCT fixture with `formula` in column D of `host_row`
/// (0-based) and return its value plus whether the cell carries an
/// `UnsupportedConstruct` diagnostic.
fn sp_eval(formula: &str, host_row: u32) -> (Option<Value>, bool) {
    let mut cells = Vec::new();
    for (i, t) in ["ab", "ac", "bx", "-cd", "cz", "ce", "df"]
        .iter()
        .enumerate()
    {
        cells.push((i as u32, 0, literal_cell(Value::text(t))));
        cells.push((i as u32, 1, literal_cell(num((i + 1) as f64))));
    }
    cells.push((host_row, 3, formula_cell(formula)));
    let mut e = Engine::load(build(cells));
    e.recalc();
    let loud = e
        .diagnostics_for(s0(), host_row, 3)
        .iter()
        .any(|d| d.kind == DiagnosticKind::UnsupportedConstruct);
    (e.value(s0(), host_row, 3).cloned(), loud)
}

/// Assert `formula` computes `expected` from BOTH the in-band (D1) and the
/// off-band (D9) host — never a host-dependent implicit intersection.
fn assert_sp_both_hosts(formula: &str, expected: Value) {
    for host_row in [0, 8] {
        let (v, _) = sp_eval(formula, host_row);
        assert_eq!(
            v.as_ref(),
            Some(&expected),
            "{formula} at host row {}",
            host_row + 1
        );
    }
}

/// Assert `formula` is a LOUD `#UNSUPPORTED!` (value + engine diagnostic) from
/// both hosts — never a silent scalar, never the off-band `#VALUE!`.
fn assert_sp_loud_refusal_both_hosts(formula: &str) {
    for host_row in [0, 8] {
        let (v, loud) = sp_eval(formula, host_row);
        assert_eq!(
            v.as_ref(),
            Some(&Value::Error(ErrorKind::Unsupported)),
            "{formula} at host row {} must be #UNSUPPORTED!",
            host_row + 1
        );
        assert!(
            loud,
            "{formula} at host row {} must carry an UnsupportedConstruct diagnostic",
            host_row + 1
        );
    }
}

/// `SUMPRODUCT(--(A1:A7="cz"))` = 1: comparison → bool array, `--` coerces
/// element-wise (OXP-201 #9 shape), one match. Was a silent 0 in-band.
#[test]
fn sumproduct_double_negated_range_comparison_counts_matches() {
    assert_sp_both_hosts("SUMPRODUCT(--(A1:A7=\"cz\"))", num(1.0));
}

/// `SUMPRODUCT((A1:A7="cz")*B1:B7)` = 5: bool array × same-shape number array
/// (rule b, OXP-201 #2/#7) picks B5. Was a silent 0 in-band.
#[test]
fn sumproduct_range_comparison_times_range() {
    assert_sp_both_hosts("SUMPRODUCT((A1:A7=\"cz\")*B1:B7)", num(5.0));
}

/// `SUMPRODUCT(B1:B7*2)` = 56: range × scalar (rule a, OXP-201 #1), then the
/// single-array sum. Was a silent 2 (row 1 intersected) in-band.
#[test]
fn sumproduct_range_times_scalar() {
    assert_sp_both_hosts("SUMPRODUCT(B1:B7*2)", num(56.0));
}

/// Two computed/reference arguments of the same shape multiply positionally:
/// `SUMPRODUCT(B1:B7*2,B1:B7)` = 2·Σb² = 280, and a compound condition
/// `SUMPRODUCT((B1:B7>3)*(A1:A7="cz"))` = 1.
#[test]
fn sumproduct_computed_and_reference_arguments_zip_positionally() {
    assert_sp_both_hosts("SUMPRODUCT(B1:B7*2,B1:B7)", num(280.0));
    assert_sp_both_hosts("SUMPRODUCT((B1:B7>3)*(A1:A7=\"cz\"))", num(1.0));
}

/// Regression: plain reference arguments are untouched by the gate.
/// `SUMPRODUCT(B1:B7,B1:B7)` = 140 from both hosts.
#[test]
fn sumproduct_reference_arguments_unchanged() {
    assert_sp_both_hosts("SUMPRODUCT(B1:B7,B1:B7)", num(140.0));
}

/// Function broadcasting over a range (`LEN(range)`, `LEFT(range,1)`,
/// `MID(range,…)`) is NOT oracle-pinned: it must be a LOUD `#UNSUPPORTED!`
/// with a diagnostic — exactly as inside SUM — never the silent scalar 2 that
/// implicit intersection produced in-band, never the off-band `#VALUE!`.
#[test]
fn sumproduct_function_broadcast_over_range_refuses_loudly() {
    assert_sp_loud_refusal_both_hosts("SUMPRODUCT(LEN(A1:A7))");
    assert_sp_loud_refusal_both_hosts("SUMPRODUCT(--(LEFT(A1:A7,1)=\"c\"))");
    assert_sp_loud_refusal_both_hosts("SUMPRODUCT((MID(A1:A7,2,1)=\"c\")*B1:B7)");
    // The SUM twin stays loud too (the shared gate, unchanged).
    assert_sp_loud_refusal_both_hosts("SUM(LEN(A1:A7))");
}

/// The variant-inspecting `IS*` functions used to return a silent `FALSE` for
/// a materialized multi-cell array (so `SUM(ISNUMBER(range)*1)` was a silent
/// 0). Element-wise `IS*` over an array is unpinned → loud `#UNSUPPORTED!`.
#[test]
fn sumproduct_and_sum_is_function_over_range_refuse_loudly() {
    assert_sp_loud_refusal_both_hosts("SUMPRODUCT(ISNUMBER(B1:B7)*1)");
    assert_sp_loud_refusal_both_hosts("SUMPRODUCT(--ISTEXT(A1:A7))");
    assert_sp_loud_refusal_both_hosts("SUM(ISNUMBER(B1:B7)*1)");
    assert_sp_loud_refusal_both_hosts("SUM(ISERROR(B1:B7)*1)");
    assert_sp_loud_refusal_both_hosts("SUM(ISNA(B1:B7)*1)");
    assert_sp_loud_refusal_both_hosts("SUM(ISERR(B1:B7)*1)");
}

/// Born-refusing boundary, unchanged: a whole-column computed argument
/// (`A:A*2`) refuses at the gate (spec §4.1 / OXP-113 posture), and the 1×1
/// scalar-broadcast mismatch stays deferred (OXP-115, scalar branch open).
#[test]
fn sumproduct_unbounded_and_scalar_broadcast_still_refuse() {
    assert_sp_loud_refusal_both_hosts("SUMPRODUCT(A:A*2)");
    for host_row in [0, 8] {
        let (v, _) = sp_eval("SUMPRODUCT(B1:B7*2,3)", host_row);
        assert_eq!(v, Some(Value::Error(ErrorKind::Unsupported)));
    }
}

// ── Audit: computed expressions in OTHER functions' array positions ──────────
//
// At top level (outside any array-context aggregator) a lazy operator
// expression over a range in an array-position argument used to be evaluated in
// scalar context — a silent implicit intersection (`LARGE(B1:B7*1,1)` = 1
// in-band, `#VALUE!` off-band; Excel: 7). Computing it is not on the
// OXP-201 pinned list for those functions, so it is now a LOUD refusal.
// Nested inside an array-context aggregator the same expressions keep their
// (already pinned) array evaluation.

#[test]
fn top_level_operator_over_range_in_array_position_refuses_loudly() {
    for f in [
        "LARGE(B1:B7*1,1)",
        "LOOKUP(5,B1:B7*1)",
        "MATCH(10,B1:B7*2,0)",
        "INDEX(B1:B7*2,3)",
        "OR(B1:B7>3)",
        "AND(B1:B7>3)",
        "SUMIF(B1:B7*1,\">3\")",
        "COUNTIF(B1:B7*1,\">3\")",
        "_xlfn.SORT(B1:B7*1)",
        "_xlfn.TEXTJOIN(\",\",TRUE,A1:A7&\"x\")",
        "ROWS(B1:B7*1)",
        "COLUMNS(B1:B7*1)",
    ] {
        assert_sp_loud_refusal_both_hosts(f);
    }
}

/// Review B1: an `IF`/`CHOOSE` root over ranges in a criteria/sum position
/// materializes under the gate; a walk cannot consume it, so it must be a loud
/// refusal from both hosts — never the silent `0` a scalar-context
/// re-evaluation produced (Excel: 22 for every formula here).
#[test]
fn if_and_choose_roots_in_array_positions_refuse_loudly() {
    for f in [
        "SUMIF(IF(TRUE,B1:B7,K1:K7),\">3\")",
        "SUMIF(IF(TRUE,B1:B7,K1:K7),\">3\",K1:K7)",
        "SUMIF(CHOOSE(1,B1:B7,K1:K7),\">3\")",
        "SUMIF(IF(A1=\"ab\",B1:B7,K1:K7),\">3\")",
        "COUNTIF(CHOOSE(1,B1:B7,K1:K7),\">3\")",
        "INDEX(IF(TRUE,B1:B7,K1:K7),3)",
        "MATCH(5,IF(TRUE,B1:B7,K1:K7),0)",
        "LOOKUP(5,IF(TRUE,B1:B7,K1:K7))",
        "ROWS(IF(TRUE,B1:B7,K1:K7))",
    ] {
        assert_sp_loud_refusal_both_hosts(f);
    }
}

/// A walk that retries a lazy array-position expression (dense → used-row →
/// used-col) must not report the same refusal three times: identical
/// diagnostics collapse to one per cell.
#[test]
fn repeated_lazy_refusals_are_reported_once_per_cell() {
    for f in [
        "MATCH(10,B1:B7*2,0)",
        "SUMIF(B1:B7*1,\">3\")",
        "COUNTIF(B1:B7*1,\">3\")",
    ] {
        let mut cells = Vec::new();
        for i in 0..7u32 {
            cells.push((i, 1, literal_cell(num(f64::from(i + 1)))));
        }
        cells.push((0, 3, formula_cell(f)));
        let mut e = Engine::load(build(cells));
        e.recalc();
        let diags = e.diagnostics_for(s0(), 0, 3);
        assert_eq!(diags.len(), 1, "{f}: {diags:?}");
        assert_eq!(diags[0].kind, DiagnosticKind::UnsupportedConstruct);
    }
}

/// A refusal that merely propagates an upstream `#UNSUPPORTED!` element through
/// a materialized array is not attributed to the consuming function's array
/// semantics: the only diagnostic stays at the source cell.
#[test]
fn propagated_refusal_element_is_not_attributed_to_the_consumer() {
    let mut cells = Vec::new();
    for i in 0..7u32 {
        cells.push((i, 1, literal_cell(num(f64::from(i + 1)))));
    }
    // B4 becomes a refusal source (whole-column range in scalar context).
    cells[3] = (3, 1, formula_cell("SUMPRODUCT(A:A*2)"));
    cells.push((8, 3, formula_cell("SUMPRODUCT(B1:B7*2)")));
    let mut e = Engine::load(build(cells));
    e.recalc();
    assert_eq!(
        e.value(s0(), 8, 3),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
    assert!(
        e.diagnostics_for(s0(), 8, 3).is_empty(),
        "consumer must not be blamed: {:?}",
        e.diagnostics_for(s0(), 8, 3)
    );
    assert!(
        !e.diagnostics_for(s0(), 3, 1).is_empty(),
        "source cell keeps its diagnostic"
    );
}

/// Nested inside SUM the same array-position expressions still array-evaluate
/// (the inherited array context): `SUM(INDEX(B1:B7*2,3))` = 6,
/// `SUM(INDEX(B1:B7*2,0,1))` = 56, `SUM(MATCH(10,B1:B7*2,0))` = 5.
#[test]
fn nested_array_position_expressions_keep_array_evaluation() {
    assert_sp_both_hosts("SUM(INDEX(B1:B7*2,3))", num(6.0));
    assert_sp_both_hosts("SUM(INDEX(B1:B7*2,0,1))", num(56.0));
    assert_sp_both_hosts("SUM(MATCH(10,B1:B7*2,0))", num(5.0));
}

/// Top-level behavior must stay byte-identical (Principle 2): a bare array
/// reaching an operator OUTSIDE array context still refuses. `INDEX` returns a
/// multi-cell array; `INDEX(A1:A5,0,1)+1` at top level is `#UNSUPPORTED!`
/// (the `array_arg_ctx` gate ensures broadcasting never fires here).
#[test]
fn top_level_index_plus_one_still_refuses() {
    let cells = vec![
        (0, 0, formula_cell("1")),
        (1, 0, formula_cell("2")),
        (2, 0, formula_cell("3")),
        (3, 0, formula_cell("4")),
        (4, 0, formula_cell("5")),
        (0, 2, formula_cell("INDEX(A1:A5,0,1)+1")),
    ];
    let mut e = Engine::load(build(cells));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 2),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
}

// ---- M2 lane 2: LET/LAMBDA interpreter -----------------------------------
//
// Semantics oracle-pinned by OXP-200 (LET scoping, closure capture, arity →
// `#VALUE!`, bare lambda → `#CALC!`) and OXP-207 (MAP element-wise + positional
// zip, SCAN output shape, BYROW→column / BYCOL→row vectors, MAKEARRAY 1-based).
// The OXP-207 farm probes use `SEQUENCE(...)` as the array source; `SEQUENCE`
// is a lane-3 dynamic-array function not yet landed, so these tests substitute
// the equivalent array literal (`{1;2;3}` ≡ `SEQUENCE(3)`), which exercises the
// identical interpreter semantics under test. Formulas are authored in the
// stored OOXML form (`_xlfn.` on future functions, `_xlpm.` on parameters), the
// exact spelling the parser sees from a real workbook.

/// Evaluate a single formula in cell A1 and return its recalculated value.
fn eval1(formula: &str) -> Option<Value> {
    let mut e = Engine::load(build(vec![(0, 0, formula_cell(formula))]));
    e.recalc();
    e.value(s0(), 0, 0).cloned()
}

#[test]
fn map_is_element_wise_oxp207_a1() {
    // SUM(MAP(SEQUENCE(3), LAMBDA(x, x*x))) = 1+4+9 = 14.
    assert_eq!(
        eval1("SUM(_xlfn.MAP({1;2;3},_xlfn.LAMBDA(_xlpm.x,_xlpm.x*_xlpm.x)))"),
        Some(num(14.0))
    );
}

#[test]
fn index_of_map_result_oxp207_a2() {
    // INDEX(MAP(SEQUENCE(3), LAMBDA(x, x*10)), 2) = 20 (1-based INDEX over a
    // computed array — exercises the for_each_row computed-array unwrap).
    assert_eq!(
        eval1("INDEX(_xlfn.MAP({1;2;3},_xlfn.LAMBDA(_xlpm.x,_xlpm.x*10)),2)"),
        Some(num(20.0))
    );
}

#[test]
fn map_multi_array_zips_positionally_oxp207_a3() {
    // SUM(MAP(SEQUENCE(3), SEQUENCE(3), LAMBDA(a,b,a*b))) = 1·1+2·2+3·3 = 14
    // (a positional zip; an outer product would be 36).
    assert_eq!(
        eval1("SUM(_xlfn.MAP({1;2;3},{1;2;3},_xlfn.LAMBDA(_xlpm.a,_xlpm.b,_xlpm.a*_xlpm.b)))"),
        Some(num(14.0))
    );
}

#[test]
fn map_unequal_shapes_refuse_loudly() {
    // Unequal-shape MAP is not oracle-pinned → loud `#UNSUPPORTED!`, never a
    // silent broadcast guess (Principle 2).
    assert_eq!(
        eval1("SUM(_xlfn.MAP({1;2;3},{1;2},_xlfn.LAMBDA(_xlpm.a,_xlpm.b,_xlpm.a*_xlpm.b)))"),
        Some(Value::Error(ErrorKind::Unsupported))
    );
}

#[test]
fn scan_emits_running_fold_oxp207_a4_a5() {
    // SCAN(0, SEQUENCE(4), LAMBDA(a,b,a+b)) = [1,3,6,10]; INDEX(..,4)=10, ..,1=1.
    // Output has the same shape as the input; element 1 = lambda(init, arr[0]).
    assert_eq!(
        eval1("INDEX(_xlfn.SCAN(0,{1;2;3;4},_xlfn.LAMBDA(_xlpm.a,_xlpm.b,_xlpm.a+_xlpm.b)),4)"),
        Some(num(10.0))
    );
    assert_eq!(
        eval1("INDEX(_xlfn.SCAN(0,{1;2;3;4},_xlfn.LAMBDA(_xlpm.a,_xlpm.b,_xlpm.a+_xlpm.b)),1)"),
        Some(num(1.0))
    );
}

#[test]
fn byrow_returns_column_vector_oxp207_a6() {
    // SUM(BYROW({1,2;3,4}, LAMBDA(r, SUM(r)))) = SUM([3;7]) = 10.
    assert_eq!(
        eval1("SUM(_xlfn.BYROW({1,2;3,4},_xlfn.LAMBDA(_xlpm.r,SUM(_xlpm.r))))"),
        Some(num(10.0))
    );
}

#[test]
fn bycol_returns_row_vector_oxp207_a7() {
    // SUM(BYCOL({1,2;3,4}, LAMBDA(c, SUM(c)))) = SUM([4,6]) = 10.
    assert_eq!(
        eval1("SUM(_xlfn.BYCOL({1,2;3,4},_xlfn.LAMBDA(_xlpm.c,SUM(_xlpm.c))))"),
        Some(num(10.0))
    );
}

#[test]
fn makearray_indices_are_one_based_oxp207_a8_a9_a10() {
    // SUM(MAKEARRAY(2,2, LAMBDA(i,j, i*10+j))) = 11+12+21+22 = 66 (0-based → 22).
    assert_eq!(
        eval1("SUM(_xlfn.MAKEARRAY(2,2,_xlfn.LAMBDA(_xlpm.i,_xlpm.j,_xlpm.i*10+_xlpm.j)))"),
        Some(num(66.0))
    );
    // Row index i is 1-based: element (1,1) = 1.
    assert_eq!(
        eval1("INDEX(_xlfn.MAKEARRAY(2,2,_xlfn.LAMBDA(_xlpm.i,_xlpm.j,_xlpm.i)),1,1)"),
        Some(num(1.0))
    );
    // Column index j is 1-based: element (1,2) = 2.
    assert_eq!(
        eval1("INDEX(_xlfn.MAKEARRAY(2,2,_xlfn.LAMBDA(_xlpm.i,_xlpm.j,_xlpm.j)),1,2)"),
        Some(num(2.0))
    );
}

#[test]
fn let_binds_sequentially_oxp200() {
    // LET(x, 2, y, x*3, y) = 6 (a later binding references an earlier one).
    assert_eq!(
        eval1("_xlfn.LET(_xlpm.x,2,_xlpm.y,_xlpm.x*3,_xlpm.y)"),
        Some(num(6.0))
    );
}

#[test]
fn lambda_named_application() {
    // LET(f, LAMBDA(x, x*x), f(7)) = 49.
    assert_eq!(
        eval1("_xlfn.LET(_xlpm.f,_xlfn.LAMBDA(_xlpm.x,_xlpm.x*_xlpm.x),_xlpm.f(7))"),
        Some(num(49.0))
    );
}

#[test]
fn lambda_closure_captures_let_binding_oxp200() {
    // LET(f, LET(x, 5, LAMBDA(y, x+y)), f(10)) = 15 — the returned lambda closes
    // over x=5 from its definition environment (OXP-200 closure capture; the
    // direct `LAMBDA(...)(10)` spelling is a parser-level loud refusal, so the
    // capture is exercised through a named binding).
    assert_eq!(
        eval1(
            "_xlfn.LET(_xlpm.f,_xlfn.LET(_xlpm.x,5,_xlfn.LAMBDA(_xlpm.y,_xlpm.x+_xlpm.y)),_xlpm.f(10))"
        ),
        Some(num(15.0))
    );
}

#[test]
fn reduce_text_accumulator_oxp200() {
    // REDUCE("", SEQUENCE(3), LAMBDA(a,b, a&b)) = "123".
    assert_eq!(
        eval1("_xlfn.REDUCE(\"\",{1;2;3},_xlfn.LAMBDA(_xlpm.a,_xlpm.b,_xlpm.a&_xlpm.b))"),
        Some(Value::text("123"))
    );
}

#[test]
fn bare_lambda_in_cell_is_calc_error_oxp200() {
    // A bare LAMBDA that is a cell's direct result displays as `#CALC!`.
    assert_eq!(
        eval1("_xlfn.LAMBDA(_xlpm.x,_xlpm.x+1)"),
        Some(Value::Error(ErrorKind::Calc))
    );
}

#[test]
fn lambda_arity_over_and_under_supply_are_value_error_oxp200() {
    // Over-supply: LAMBDA(x, x+1) called with two args → `#VALUE!`.
    assert_eq!(
        eval1("_xlfn.LET(_xlpm.f,_xlfn.LAMBDA(_xlpm.x,_xlpm.x+1),_xlpm.f(5,6))"),
        Some(Value::Error(ErrorKind::Value))
    );
    // Under-supply: LAMBDA(x, y, x+y) called with one arg → `#VALUE!`.
    assert_eq!(
        eval1("_xlfn.LET(_xlpm.f,_xlfn.LAMBDA(_xlpm.x,_xlpm.y,_xlpm.x+_xlpm.y),_xlpm.f(5))"),
        Some(Value::Error(ErrorKind::Value))
    );
}

#[test]
fn isomitted_refuses_loudly_pending_probe() {
    // ISOMITTED is not yet oracle-pinned (M2 lane 2 defers it) → `#UNSUPPORTED!`.
    assert_eq!(
        eval1("_xlfn.ISOMITTED(1)"),
        Some(Value::Error(ErrorKind::Unsupported))
    );
}

#[test]
fn free_body_name_unbound_refuses_loudly() {
    // A lambda body referencing a name that is neither a parameter nor captured
    // (here the recursion attempt `f` inside its own body) refuses loudly rather
    // than guess call-time-vs-capture-time resolution — recursion is not yet
    // OXP-pinned. `_xlpm.g` is undefined at capture time, so `g(1)` is unknown.
    assert_eq!(
        eval1("_xlfn.LET(_xlpm.f,_xlfn.LAMBDA(_xlpm.n,_xlpm.g(_xlpm.n)),_xlpm.f(1))"),
        Some(Value::Error(ErrorKind::Unsupported))
    );
}

#[test]
fn builtin_call_not_hijacked_by_let_binding() {
    // A LET binding named `_xlpm.sum` must NOT capture a later builtin `SUM(3)`
    // call — Excel's stored form distinguishes `_xlpm.sum(` from the builtin
    // `SUM(`, so only `_xlpm.`-prefixed references resolve against the lexical
    // env. Regression for the review-caught silent-wrong (returned 0, Excel = 3).
    assert_eq!(
        eval1("_xlfn.LET(_xlpm.sum,_xlfn.LAMBDA(_xlpm.x,0),SUM(3))"),
        Some(num(3.0))
    );
}

#[test]
fn lambda_param_shadows_captured_same_name() {
    // Outer LET binds x=1; the inner lambda's own parameter x=5 shadows the
    // captured x when applied: LET(x,1, LET(f, LAMBDA(x, x*10), f(5))) = 50.
    assert_eq!(
        eval1(
            "_xlfn.LET(_xlpm.x,1,_xlfn.LET(_xlpm.f,_xlfn.LAMBDA(_xlpm.x,_xlpm.x*10),_xlpm.f(5)))"
        ),
        Some(num(50.0))
    );
}

#[test]
fn let_binding_drives_sequence_length_oxp200() {
    // OXP-200 A8: SUM(LET(n, 3, SEQUENCE(n))) = 6 — a LET binding drives the
    // length of a real dynamic-array function (not an array literal).
    assert_eq!(
        eval1("SUM(_xlfn.LET(_xlpm.n,3,_xlfn.SEQUENCE(_xlpm.n)))"),
        Some(num(6.0))
    );
}

#[test]
fn let_aggregate_binding_oxp200() {
    // OXP-200 A9: LET(x, SUM({1;2;3}), x*2) = 12 — a binding may hold an
    // aggregate result.
    assert_eq!(
        eval1("_xlfn.LET(_xlpm.x,SUM({1;2;3}),_xlpm.x*2)"),
        Some(num(12.0))
    );
}

#[test]
fn higher_order_probe_mirrors_over_real_sequence_oxp199() {
    // OXP-199 column-B probe mirrors, over a real SEQUENCE source (the farm
    // probes used SEQUENCE, the earlier tests the equivalent array literal):
    // A5 MAP ×10 = 60; A6 REDUCE running sum = 10; A7 SCAN last = 6;
    // A8 BYROW = 10; A9 MAKEARRAY r*c = 9.
    assert_eq!(
        eval1("SUM(_xlfn.MAP(_xlfn.SEQUENCE(3),_xlfn.LAMBDA(_xlpm.x,_xlpm.x*10)))"),
        Some(num(60.0))
    );
    assert_eq!(
        eval1("_xlfn.REDUCE(0,_xlfn.SEQUENCE(4),_xlfn.LAMBDA(_xlpm.a,_xlpm.b,_xlpm.a+_xlpm.b))"),
        Some(num(10.0))
    );
    assert_eq!(
        eval1(
            "INDEX(_xlfn.SCAN(0,_xlfn.SEQUENCE(3),_xlfn.LAMBDA(_xlpm.a,_xlpm.b,_xlpm.a+_xlpm.b)),3)"
        ),
        Some(num(6.0))
    );
    // The exact OXP-199 A8 probe shape is `BYROW(SEQUENCE(2,2), …)`; a 2-D
    // SEQUENCE currently refuses loudly (its fill order is unpinned — the
    // lane-3a probe queue), so the BYROW-over-SEQUENCE mirror uses the 1-D
    // form here; the 2×2 element-wise BYROW/BYCOL semantics are covered by the
    // array-literal OXP-207 A6/A7 tests above.
    assert_eq!(
        eval1("SUM(_xlfn.BYROW(_xlfn.SEQUENCE(4),_xlfn.LAMBDA(_xlpm.r,SUM(_xlpm.r))))"),
        Some(num(10.0))
    );
    assert_eq!(
        eval1("SUM(_xlfn.MAKEARRAY(2,2,_xlfn.LAMBDA(_xlpm.r,_xlpm.c,_xlpm.r*_xlpm.c)))"),
        Some(num(9.0))
    );
}

#[test]
fn duplicate_let_param_is_load_rejected_oxp200() {
    // OXP-200 edge: `LET(x,1,x,2,x)` is LOAD-REJECTED by Excel (validated at
    // open, not a computed error). Mirror: the cell's program is refused at
    // load — a ParseError-kind diagnostic visible BEFORE recalc, and the cell
    // is `#UNSUPPORTED!`, never a silently-shadowed computed value.
    let mut e = Engine::load(build(vec![(
        0,
        0,
        formula_cell("_xlfn.LET(_xlpm.x,1,_xlpm.x,2,_xlpm.x)"),
    )]));
    let diags = e.diagnostics();
    assert_eq!(diags.len(), 1, "load-time rejection must precede recalc");
    assert_eq!(diags[0].kind, DiagnosticKind::ParseError);
    assert!(diags[0].message.contains("duplicate LET parameter"));
    assert!(diags[0].message.contains("OXP-200"));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
}

#[test]
fn duplicate_let_param_nested_is_load_rejected_oxp200() {
    // The load-level validation must see a duplicate-param LET anywhere in the
    // tree, not only at the formula head.
    let mut e = Engine::load(build(vec![(
        0,
        0,
        formula_cell("SUM(_xlfn.LET(_xlpm.a,1,_xlpm.a,2,_xlpm.a))"),
    )]));
    let diags = e.diagnostics();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].kind, DiagnosticKind::ParseError);
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
}

#[test]
fn duplicate_let_param_edit_is_load_rejected_oxp200() {
    // The programmatic-edit compile path must apply the same load-level
    // rejection as workbook load.
    let mut e = Engine::load(build(vec![(0, 0, literal_cell(num(1.0)))]));
    e.recalc();
    e.edit(
        CellId {
            sheet: s0(),
            row: 0,
            col: 0,
        },
        CellInput::Formula("=_xlfn.LET(_xlpm.x,1,_xlpm.x,2,_xlpm.x)".to_string()),
    );
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
    let diags = e.diagnostics();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].kind, DiagnosticKind::ParseError);
    assert!(diags[0].message.contains("duplicate LET parameter"));
}

#[test]
fn duplicate_let_param_shared_follow_on_is_load_rejected_oxp200() {
    // A shared-formula follow-on expands its master's formula through the same
    // compile path — a duplicate-param LET master must poison the follow-on at
    // load too (the master's own cell likewise).
    let mut e = Engine::load(build(vec![
        (
            0,
            0,
            shared_master_cell("_xlfn.LET(_xlpm.x,1,_xlpm.x,2,_xlpm.x)", 0, "A1:A2"),
        ),
        (1, 0, shared_follow_cell(0)),
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
    assert_eq!(
        e.value(s0(), 1, 0),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
    let diags = e.diagnostics();
    assert_eq!(diags.len(), 2);
    assert!(diags.iter().all(|d| d.kind == DiagnosticKind::ParseError));
}

#[test]
fn nested_let_shadowing_outer_binding_is_not_a_duplicate() {
    // Only a duplicate within ONE LET's own parameter list is load-rejected;
    // an inner LET re-binding an outer LET's name is ordinary lexical
    // shadowing: LET(x,1, LET(x,2, x)) = 2 evaluates normally (consistent with
    // `lambda_param_shadows_captured_same_name`).
    assert_eq!(
        eval1("_xlfn.LET(_xlpm.x,1,_xlfn.LET(_xlpm.x,2,_xlpm.x))"),
        Some(num(2.0))
    );
}

#[test]
fn duplicate_lambda_param_refuses_loudly() {
    // A duplicate LAMBDA parameter list is NOT oracle-pinned (OXP-200 pinned
    // only the duplicate-LET load rejection; Excel's open-time validation of a
    // duplicate LAMBDA param is unprobed). Refuse loudly, never resolve the
    // ambiguity by silent shadowing.
    assert_eq!(
        eval1("_xlfn.LET(_xlpm.f,_xlfn.LAMBDA(_xlpm.x,_xlpm.x,_xlpm.x),_xlpm.f(1,2))"),
        Some(Value::Error(ErrorKind::Unsupported))
    );
}

#[test]
fn direct_lambda_application_spelling_refuses_loudly() {
    // OXP-200 A7 pins `LET(x,5,LAMBDA(y,x+y))(10)` = 15 (closure capture — the
    // semantics are covered via named application above). The direct `)(`
    // SPELLING, however, is a parser-level loud refusal per the ratified
    // RFC-0012 §10 (supporting it is a separate, RFC-gated `xl-ast` grammar
    // change): the trailing `(10)` is an unconsumed token → ParseError →
    // `#UNSUPPORTED!`. This test pins the loud-refusal posture so the gap is
    // visible, never silent.
    let mut e = Engine::load(build(vec![(
        0,
        0,
        formula_cell("_xlfn.LET(_xlpm.x,5,_xlfn.LAMBDA(_xlpm.y,_xlpm.x+_xlpm.y))(10)"),
    )]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported))
    );
    let diags = e.diagnostics();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].kind, DiagnosticKind::ParseError);
}

// ============================================================================
// M2 lane 4 — dynamic-array SPILL (compute-only).
//
// Productionizes the proven `spike/spill-sequence` mechanism against the current
// engine: a top-level `Value::Array` result spills into neighbouring cells; an
// obstruction yields `#SPILL!`; the spilled-range operator `A1#`
// (`_xlfn.ANCHORARRAY(A1)`) resolves through the RFC-0003 reference seam; `@`
// implicit-intersects a computed array to its top-left (OXP-201); a
// symmetric-difference grow/shrink-dirtying fixpoint (RFC-0012 §3 / BC-1
// protocol B, BC-4 determinism) reconciles the eval-time footprint with the
// static graph; a lambda-valued element makes the whole spill `#VALUE!`
// (OXP-203); and overlapping anchors loud-refuse (`#SPILL!` + diagnostic) under
// deterministic claim order (BC-5 / OXP-202).
//
// The spike faked SEQUENCE (a front-end shortcut); lane 2 landed real
// array-producing functions, so these drive spill off genuine
// `MAKEARRAY`/`MAP` results — the more faithful productionization. Formulas use
// the stored OOXML spelling (`_xlfn.` / `_xlpm.`).
// ============================================================================

/// `=MAKEARRAY(rows, 1, LAMBDA(i,j,i))` — a `rows`×1 column dynamic array whose
/// element at row `i` (1-based) is `i`, i.e. the column `1;2;…;rows`.
fn makecol(rows: u32) -> String {
    format!("_xlfn.MAKEARRAY({rows},1,_xlfn.LAMBDA(_xlpm.i,_xlpm.j,_xlpm.i))")
}

#[test]
fn spill_makearray_column_spills_and_a1hash_streams() {
    // A1=MAKEARRAY(3,1,…) spills into A1:A3 = 1,2,3 (top-level Value::Array →
    // spill); C1=SUM(A1#) and D1=SUM(ANCHORARRAY(A1)) both stream the region = 6.
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell(&makecol(3))),                  // A1 anchor
        (0, 2, formula_cell("SUM(A1#)")),                   // C1 (postfix `#`)
        (0, 3, formula_cell("SUM(_xlfn.ANCHORARRAY(A1))")), // D1 (serialized form)
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&num(1.0)), "A1 anchor = top-left");
    assert_eq!(
        e.value(s0(), 1, 0),
        Some(&num(2.0)),
        "A2 written by the spill"
    );
    assert_eq!(
        e.value(s0(), 2, 0),
        Some(&num(3.0)),
        "A3 written by the spill"
    );
    assert_eq!(e.value(s0(), 0, 2), Some(&num(6.0)), "SUM(A1#) = 1+2+3");
    assert_eq!(
        e.value(s0(), 0, 3),
        Some(&num(6.0)),
        "SUM(ANCHORARRAY(A1)) == SUM(A1#)"
    );
}

#[test]
fn spill_region_reconstructs_anchor_array_for_ffi_bindings() {
    // `Engine::spill_region` is the SINGLE read-only query all three FFI
    // bindings (Python `spill_region`, Node/WASM `spillRegion`) route through
    // (RFC-0013 §3), so proving it here proves tri-binding congruence at the
    // shared source of truth — a live host-loaded `.node`/`.wasm` diff remains
    // the pre-release gate, but the *values* are identical by construction
    // because the bindings are thin pass-throughs to this method.
    //
    // A1 = MAKEARRAY(2,2, i*10+j) spills A1:B2 = [[11,12],[21,22]] (1-based
    // indices, OXP-207; row-major), which distinguishes a correct row-major
    // reconstruction from a transposed one.
    let mut e = Engine::load(build(vec![(
        0,
        0,
        formula_cell("_xlfn.MAKEARRAY(2,2,_xlfn.LAMBDA(_xlpm.i,_xlpm.j,_xlpm.i*10+_xlpm.j))"),
    )]));
    e.recalc();

    // The anchor reconstructs the full 2×2 region as a row-major Value::Array.
    let region = e.spill_region(s0(), 0, 0).expect("A1 is a spill anchor");
    let Value::Array(arr) = region else {
        panic!("spill_region must yield a Value::Array");
    };
    assert_eq!((arr.rows(), arr.cols()), (2, 2), "2×2 region shape");
    assert_eq!(arr.get(0, 0), Some(&num(11.0)), "row-major [0][0]");
    assert_eq!(arr.get(0, 1), Some(&num(12.0)), "row-major [0][1]");
    assert_eq!(arr.get(1, 0), Some(&num(21.0)), "row-major [1][0]");
    assert_eq!(arr.get(1, 1), Some(&num(22.0)), "row-major [1][1]");

    // A spilled-INTO cell (B2) is not itself an anchor → None (BC-10), so the
    // FFI surface reports `null`/`None` there rather than a bogus region.
    assert_eq!(
        e.spill_region(s0(), 1, 1),
        None,
        "a spilled-into cell is not an anchor"
    );
    // A plain empty cell far from any spill → None (never a guess).
    assert_eq!(
        e.spill_region(s0(), 5, 5),
        None,
        "an empty cell is not an anchor"
    );
}

#[test]
fn spill_map_top_level_spills() {
    // A top-level MAP result spills exactly like MAKEARRAY: MAP({1;2;3}, x=>x*x)
    // spills A1:A3 = 1,4,9.
    let mut e = Engine::load(build(vec![(
        0,
        0,
        formula_cell("_xlfn.MAP({1;2;3},_xlfn.LAMBDA(_xlpm.x,_xlpm.x*_xlpm.x))"),
    )]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&num(1.0)));
    assert_eq!(e.value(s0(), 1, 0), Some(&num(4.0)));
    assert_eq!(e.value(s0(), 2, 0), Some(&num(9.0)));
}

#[test]
fn spill_1x1_anchor_is_registered_oxp204() {
    // OXP-204 / RFC-0012 BC-10: a 1×1 dynamic-array result IS an anchor — `A1#`
    // resolves to the 1×1 region. A1=MAKEARRAY(1,1,LAMBDA(i,j,i*10+j))=11.
    let mut e = Engine::load(build(vec![
        (
            0,
            0,
            formula_cell("_xlfn.MAKEARRAY(1,1,_xlfn.LAMBDA(_xlpm.i,_xlpm.j,_xlpm.i*10+_xlpm.j))"),
        ),
        (0, 2, formula_cell("A1#")), // C1 = A1#  → 11 (scalar deref of 1×1)
        (0, 3, formula_cell("ROWS(A1#)")), // D1 = ROWS(A1#) → 1
        (0, 4, formula_cell("SUM(A1#)")), // E1 = SUM(A1#) → 11
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&num(11.0)), "A1 anchor value");
    assert_eq!(
        e.value(s0(), 0, 2),
        Some(&num(11.0)),
        "C1 = A1# resolves 1×1"
    );
    assert_eq!(
        e.value(s0(), 0, 3),
        Some(&num(1.0)),
        "ROWS(A1#) = 1 (1×1 region)"
    );
    assert_eq!(e.value(s0(), 0, 4), Some(&num(11.0)), "SUM(A1#) = 11");
}

#[test]
fn spill_obstruction_yields_spill_error() {
    // A1=MAKEARRAY(3,1,…) wants A1:A3, but A2 holds a literal 99 → the anchor is
    // `#SPILL!`, the blocker is untouched, and A3 is never written (Principle 2:
    // a diagnostic is recorded, never a silent partial spill).
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell(&makecol(3))), // A1 anchor
        (1, 0, literal_cell(num(99.0))),   // A2 blocks the spill
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Spill)),
        "anchor is #SPILL! when blocked"
    );
    assert_eq!(
        e.value(s0(), 1, 0),
        Some(&num(99.0)),
        "blocker NOT overwritten"
    );
    assert_eq!(
        e.value(s0(), 2, 0),
        None,
        "A3 never written under obstruction"
    );
    assert!(
        !e.diagnostics_for(s0(), 0, 0).is_empty(),
        "a #SPILL! diagnostic is recorded"
    );
}

#[test]
fn spill_nonconvergence_refuses_loudly_and_reconciles_readers_bc4b() {
    // review B2: a spill whose dimension reads its OWN footprint through a static
    // range oscillates — and crucially forms NO static cycle (the spilled cells
    // are plain data, so Tarjan sees no back-edge), so only the fixpoint planner
    // can catch it. It must (a) refuse the anchor loudly at the cap (BC-4b) AND
    // (b) reconcile the readers of the cells the refusal vacates — never leave
    // them holding a stale spilled value.
    //
    // A1 = MAKEARRAY(3 - COUNT(A2:A9), 1, LAMBDA(_,_,1)):
    //   COUNT=0 → dim 3 → spills A1:A3 = 1,1,1 → COUNT(A2:A9)=2 → dim 1 → spills
    //   A1 only → A2,A3 vacated → COUNT=0 → dim 3 → … a 2-cycle that never
    //   settles (both dims are valid, so it oscillates rather than erroring).
    // B1 = A3 + 0 reads a cell the oscillation vacates; after the refusal A3 is
    // Blank, so B1 must reconcile to 0.0 (blank→0), NOT a stale 1.0.
    let mut e = Engine::load(build(vec![
        (
            0,
            0,
            formula_cell("_xlfn.MAKEARRAY(3-COUNT(A2:A9),1,_xlfn.LAMBDA(_xlpm.r,_xlpm.c,1))"),
        ),
        (0, 1, formula_cell("A3+0")), // B1: reader of a vacated spilled cell
    ]));
    e.recalc();

    // (a) The non-converging anchor refuses loudly.
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported)),
        "non-converging anchor refuses #UNSUPPORTED! (BC-4b)"
    );
    assert!(
        !e.diagnostics_for(s0(), 0, 0).is_empty(),
        "a non-convergence diagnostic is recorded on the anchor"
    );
    // (b) The reader of a vacated cell reconciles to blank→0, not a stale 1.0.
    assert_eq!(
        e.value(s0(), 0, 1),
        Some(&num(0.0)),
        "reader of a vacated spilled cell reconciles (never stale)"
    );
}

#[test]
fn spill_bare_multicell_a1hash_in_scalar_context_refuses() {
    // A bare `=A1#` naming a MULTI-cell spilled range in scalar context would
    // re-spill in Excel (unpinned by any OXP), so the engine refuses loudly
    // rather than guess a value. (A 1×1 `A1#` derefs to its single element —
    // covered by `spill_1x1_anchor_is_registered_oxp204`.)
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell(&makecol(3))), // A1: 3×1 spill
        (0, 1, formula_cell("A1#")),       // B1: bare multi-cell A1# in scalar ctx
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 1),
        Some(&Value::Error(ErrorKind::Unsupported)),
        "bare multi-cell =A1# in scalar context refuses"
    );
    assert!(
        !e.diagnostics_for(s0(), 0, 1).is_empty(),
        "a diagnostic is recorded for the refused A1#"
    );
}

#[test]
fn spill_at_range_in_array_arg_context_intersects_not_top_left_b3() {
    // review B3: `@` over a STATIC range must implicit-intersect at the formula's
    // own row/column even INSIDE an array-arg consumer — not silently take the
    // top-left. A1:A3 = 10,20,30; B2 = SUM(@A1:A3) sits in row 2, so @A1:A3
    // intersects to A2 = 20 and SUM(20) = 20 (the pre-fix bug gave SUM(A1)=10
    // because the inner range materialized as an array in array-arg context).
    let mut e = Engine::load(build(vec![
        (0, 0, literal_cell(num(10.0))),     // A1
        (1, 0, literal_cell(num(20.0))),     // A2
        (2, 0, literal_cell(num(30.0))),     // A3
        (1, 1, formula_cell("SUM(@A1:A3)")), // B2 (row 2) → @A1:A3 = A2 = 20
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 1, 1),
        Some(&num(20.0)),
        "@ static range in array-arg context intersects at the formula row (20), not top-left (10)"
    );
}

#[test]
fn spill_race_over_foreign_owned_blank_slot_refuses_b1() {
    // review B1: a spilled cell OWNED by one anchor but holding `Blank` must still
    // block a second anchor — otherwise the second silently claims it and both
    // anchors own the cell (spill_region would then report a foreign element).
    // B1 = MAKEARRAY(2,1, LAMBDA(_,_ , Z99)) spills B1:B2 = [Blank, Blank] (Z99 is
    // empty), both owned by B1. A2 = MAKEARRAY(1,2, …) then wants A2:B2, and B2 is
    // B1's foreign-owned Blank slot → A2 must be #SPILL! (pre-fix it claimed B2).
    // Plan order: B1=(row0,col1) precedes A2=(row1,col0), so B1 spills first.
    let mut e = Engine::load(build(vec![
        (
            0,
            1,
            formula_cell("_xlfn.MAKEARRAY(2,1,_xlfn.LAMBDA(_xlpm.r,_xlpm.c,Z99))"),
        ),
        (
            1,
            0,
            formula_cell("_xlfn.MAKEARRAY(1,2,_xlfn.LAMBDA(_xlpm.r,_xlpm.c,9))"),
        ),
    ]));
    e.recalc();
    // The second anchor loses the race loudly.
    assert_eq!(
        e.value(s0(), 1, 0),
        Some(&Value::Error(ErrorKind::Spill)),
        "second anchor over a foreign-owned Blank slot is #SPILL!"
    );
    assert!(
        !e.diagnostics_for(s0(), 1, 0).is_empty(),
        "a #SPILL! diagnostic is recorded for the losing anchor"
    );
    // The winner's Blank slot is untouched (not overwritten with the loser's 9).
    assert_eq!(
        e.value(s0(), 1, 1),
        Some(&Value::Blank),
        "B2 stays the winner's Blank, not the loser's 9"
    );
    // The winner still owns its 2×1 region.
    assert!(
        e.spill_region(s0(), 0, 1).is_some(),
        "B1 remains a live spill anchor"
    );
}

#[test]
fn spill_off_sheet_edge_refuses_no_phantom_write_b4() {
    // review B4: a spill whose footprint would run past the sheet edge is #SPILL!,
    // never a phantom write past MAX_ROW0. Anchor at the last row spilling 3 rows
    // down overflows → #SPILL! + diagnostic; no registry entry.
    let last = 1_048_575u32; // MAX_ROW0
    let mut e = Engine::load(build(vec![(last, 0, formula_cell(&makecol(3)))]));
    e.recalc();
    assert_eq!(
        e.value(s0(), last, 0),
        Some(&Value::Error(ErrorKind::Spill)),
        "off-sheet spill is #SPILL!"
    );
    assert!(
        !e.diagnostics_for(s0(), last, 0).is_empty(),
        "an off-sheet-edge diagnostic is recorded"
    );
    assert_eq!(
        e.spill_region(s0(), last, 0),
        None,
        "no region for a refused off-sheet anchor"
    );
}

#[test]
fn spill_exact_fit_to_last_row_still_spills_b4() {
    // The B4 bound is INCLUSIVE: an anchor whose last spilled row is exactly the
    // sheet's last row spills normally (no off-by-one over-refusal).
    let anchor = 1_048_575u32 - 2; // spills 3 rows → last row = MAX_ROW0 exactly
    let mut e = Engine::load(build(vec![(anchor, 0, formula_cell(&makecol(3)))]));
    e.recalc();
    assert_eq!(
        e.value(s0(), anchor, 0),
        Some(&num(1.0)),
        "exact-fit anchor spills its top element"
    );
    assert_eq!(
        e.value(s0(), anchor + 2, 0),
        Some(&num(3.0)),
        "the last row (== MAX_ROW0) is written on an exact fit"
    );
}

#[test]
fn spill_at_implicit_intersection_over_computed_array_oxp201() {
    // OXP-201: `@` over a computed dynamic array takes its top-left element (the
    // array is anchored at the formula cell, so the intersection is the origin).
    // @MAKEARRAY(3,1,LAMBDA(i,j,i)) = 1; @MAKEARRAY(1,3,LAMBDA(i,j,j)) = 1;
    // @1×1 = the single element.
    assert_eq!(
        eval1("@_xlfn.MAKEARRAY(3,1,_xlfn.LAMBDA(_xlpm.i,_xlpm.j,_xlpm.i))"),
        Some(num(1.0)),
        "@ column array → first element"
    );
    assert_eq!(
        eval1("@_xlfn.MAKEARRAY(1,3,_xlfn.LAMBDA(_xlpm.i,_xlpm.j,_xlpm.j))"),
        Some(num(1.0)),
        "@ row array → first element"
    );
    assert_eq!(
        eval1("@_xlfn.MAKEARRAY(1,1,_xlfn.LAMBDA(_xlpm.i,_xlpm.j,_xlpm.i*10+_xlpm.j))"),
        Some(num(11.0)),
        "@ 1×1 → the single element"
    );
}

#[test]
fn spill_shrink_vacates_and_redirties_reader() {
    // Spike's shrink demonstration, productionized on MAKEARRAY: A1 spills A1:A3,
    // C1=SUM(A1#)=6. Editing A1 down to a 2-row array vacates A3 (→ Blank) and the
    // A1# reader re-reads 1+2=3 — the static A1→C1 edge + the fixpoint reconcile.
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell(&makecol(3))),
        (0, 2, formula_cell("SUM(A1#)")),
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 2, 0),
        Some(&num(3.0)),
        "A3 spilled before shrink"
    );
    assert_eq!(
        e.value(s0(), 0, 2),
        Some(&num(6.0)),
        "SUM(A1#) = 6 before shrink"
    );

    e.edit(CellId::new(s0(), 0, 0), CellInput::Formula(makecol(2)));
    assert_eq!(e.value(s0(), 0, 0), Some(&num(1.0)), "A1 after shrink");
    assert_eq!(e.value(s0(), 1, 0), Some(&num(2.0)), "A2 after shrink");
    assert_eq!(
        e.value(s0(), 2, 0),
        Some(&Value::Blank),
        "A3 VACATED to Blank on shrink"
    );
    assert_eq!(
        e.value(s0(), 0, 2),
        Some(&num(3.0)),
        "SUM(A1#) re-read = 1+2"
    );
}

#[test]
fn spill_grow_redirties_direct_reader_via_fixpoint() {
    // THE CRUX (RFC-0012 §3): a formula that reads a cell a spill GROWS into — via
    // a plain reference, NOT `A1#` — must recompute even though no static edge
    // links it to the anchor. Z1=A3 reads A3; A1 initially spills only A1:A2, so
    // A3 is blank→0 and Z1=0. Growing A1 to 3 rows fills A3=3; the fixpoint's
    // symmetric-difference dirtying re-dirties A3, whose reverse edge pulls Z1 in.
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell(&makecol(2))), // A1 spills A1:A2
        (0, 25, formula_cell("A3")),       // Z1 reads A3 (a would-be-spilled cell)
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 2, 0), None, "A3 not yet spilled into");
    assert_eq!(e.value(s0(), 0, 25), Some(&num(0.0)), "Z1 = A3 (blank→0)");

    e.edit(CellId::new(s0(), 0, 0), CellInput::Formula(makecol(3)));
    assert_eq!(e.value(s0(), 2, 0), Some(&num(3.0)), "A3 now spilled = 3");
    assert_eq!(
        e.value(s0(), 0, 25),
        Some(&num(3.0)),
        "Z1 re-read A3=3 via grow-dirtying (footprint-growth closure)"
    );
}

#[test]
fn spill_full_recalc_reconciles_reader_ordered_before_anchor() {
    // The other half of the crux, on a *full* recalc: the reader A1=(0,0) is
    // ordered before the anchor B2=(1,1) in the canonical plan (both are roots,
    // tie-broken by CellId), so pass 1 reads B4 stale. The fixpoint re-runs A1
    // after B2 spills B4=3. Proves `recalc()` is self-consistent, not just edits.
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell("B4")), // A1 reads B4 (filled only by the spill)
        (1, 1, formula_cell(&makecol(3))), // B2 spills B2:B4 = 1,2,3
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 3, 1), Some(&num(3.0)), "B4 spilled = 3");
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&num(3.0)),
        "A1=B4 reconciled to 3 despite being ordered before its anchor"
    );
}

#[test]
fn spill_lambda_valued_element_is_value_error_oxp203() {
    // OXP-203 / RFC-0012 BC-7: a lambda-valued element in a spilling array makes
    // the WHOLE spill `#VALUE!` (NOT `#CALC!`, and NOT a spill). Distinct from a
    // bare lambda that is a cell's direct result, which is `#CALC!` (OXP-200).
    let mut e = Engine::load(build(vec![(
        0,
        0,
        formula_cell(
            "_xlfn.MAKEARRAY(2,2,_xlfn.LAMBDA(_xlpm.r,_xlpm.c,_xlfn.LAMBDA(_xlpm.x,_xlpm.x+1)))",
        ),
    )]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Value)),
        "lambda-valued spill element → whole spill #VALUE! (OXP-203)"
    );
    assert_eq!(
        e.value(s0(), 0, 1),
        None,
        "no spill occurred (B1 unwritten)"
    );
    assert_eq!(
        e.value(s0(), 1, 0),
        None,
        "no spill occurred (A2 unwritten)"
    );
    // The distinction: a bare lambda in a cell is #CALC! (OXP-200), not #VALUE!.
    assert_eq!(
        eval1("_xlfn.LAMBDA(_xlpm.x,_xlpm.x+1)"),
        Some(&Value::Error(ErrorKind::Calc)).cloned(),
    );
}

#[test]
fn spill_retract_to_scalar_vacates_region() {
    // Editing a spilling anchor to a scalar formula retracts its whole region:
    // A1 spills A1:A3, C1=A2 reads the spilled 2. After A1 := =5, A2/A3 are Blank
    // and C1 re-reads A2 (blank→0).
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell(&makecol(3))),
        (0, 2, formula_cell("A2")), // C1 reads a spilled cell directly
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 2),
        Some(&num(2.0)),
        "C1 = A2 (spilled) = 2"
    );

    e.edit(CellId::new(s0(), 0, 0), CellInput::Formula("5".to_string()));
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&num(5.0)),
        "A1 is now the scalar 5"
    );
    assert_eq!(e.value(s0(), 1, 0), Some(&Value::Blank), "A2 vacated");
    assert_eq!(e.value(s0(), 2, 0), Some(&Value::Blank), "A3 vacated");
    assert_eq!(
        e.value(s0(), 0, 2),
        Some(&num(0.0)),
        "C1 re-read A2 (blank→0)"
    );
}

#[test]
fn spill_recalc_is_idempotent() {
    // Determinism (RFC-0012 §5): a spill re-spilling over its own prior region is
    // not self-obstructed — a second full recalc reproduces the same values and
    // never spuriously turns the anchor `#SPILL!` (the reverse-index reclaim).
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell(&makecol(3))),
        (0, 2, formula_cell("SUM(A1#)")),
    ]));
    e.recalc();
    let first: Vec<Option<Value>> = (0..3).map(|r| e.value(s0(), r, 0).cloned()).collect();
    let first_c1 = e.value(s0(), 0, 2).cloned();
    e.recalc();
    let second: Vec<Option<Value>> = (0..3).map(|r| e.value(s0(), r, 0).cloned()).collect();
    assert_eq!(first, second, "spill values identical across recalcs");
    assert_eq!(first, vec![Some(num(1.0)), Some(num(2.0)), Some(num(3.0))]);
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&num(1.0)),
        "anchor never spuriously #SPILL!"
    );
    assert_eq!(e.value(s0(), 0, 2), first_c1.as_ref(), "SUM(A1#) stable");
}

#[test]
fn spill_edit_recalc_equals_full_rebuild() {
    // Determinism (BC-4 intent, sans seeded RAND which is #UNSUPPORTED in v0): the
    // incremental edit path and a from-scratch full rebuild of the post-edit
    // workbook must agree cell-for-cell over a spill + its readers.
    let edited = || {
        let mut e = Engine::load(build(vec![
            (0, 0, formula_cell(&makecol(2))),
            (0, 2, formula_cell("SUM(A1#)")),
            (0, 25, formula_cell("A3")),
        ]));
        e.recalc();
        e.edit(CellId::new(s0(), 0, 0), CellInput::Formula(makecol(3)));
        e
    };
    let rebuilt = || {
        let mut e = Engine::load(build(vec![
            (0, 0, formula_cell(&makecol(3))),
            (0, 2, formula_cell("SUM(A1#)")),
            (0, 25, formula_cell("A3")),
        ]));
        e.recalc();
        e
    };
    let a = edited();
    let b = rebuilt();
    for &(r, c) in &[(0u32, 0u32), (1, 0), (2, 0), (0, 2), (0, 25)] {
        assert_eq!(
            a.value(s0(), r, c),
            b.value(s0(), r, c),
            "edit-recalc ≡ full-rebuild at ({r},{c})"
        );
    }
    assert_eq!(
        a.value(s0(), 0, 2),
        Some(&num(6.0)),
        "SUM(A1#) = 6 after grow"
    );
    assert_eq!(
        a.value(s0(), 0, 25),
        Some(&num(3.0)),
        "Z1 = A3 = 3 after grow"
    );
}

#[test]
fn spill_two_anchor_race_is_loud_refuse_oxp202() {
    // RFC-0012 BC-5 / OXP-202: two overlapping dynamic-array anchors conflict.
    // A1 wants A1:A2 but A2 is itself an anchor (a formula node) → A1 is blocked
    // (`#SPILL!`), A2 then spills A2:A3 = 1,2. Deterministic claim (= plan) order;
    // the exact error kind for a pure race is unpinned, so the diagnostic cites
    // the ledger rather than guessing.
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell(&makecol(2))), // A1 wants A1:A2
        (1, 0, formula_cell(&makecol(2))), // A2 wants A2:A3 (overlaps A1's A2)
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Spill)),
        "the earlier-claiming anchor is blocked → #SPILL!"
    );
    assert_eq!(
        e.value(s0(), 1, 0),
        Some(&num(1.0)),
        "A2 anchor spills top-left"
    );
    assert_eq!(e.value(s0(), 2, 0), Some(&num(2.0)), "A2 spills into A3");
    let diag = e.diagnostics_for(s0(), 0, 0);
    assert!(!diag.is_empty(), "a loud #SPILL! diagnostic is recorded");
    assert!(
        diag.iter().any(|d| d.message.contains("OXP-202")),
        "the two-anchor-race diagnostic cites the unpinned-kind ledger entry"
    );
}

#[test]
fn spill_edit_into_region_blocks_the_anchor() {
    // Robustness (Principle 2, never silently wrong): typing a value into a cell
    // that a dynamic array spilled into blocks the array. A1 spills A1:A3; editing
    // A2 to a literal 99 breaks A2's spill ownership + re-dirties A1, which now
    // finds A2 occupied → `#SPILL!`. The user's 99 is NOT clobbered, and A3 (the
    // rest of the now-retracted region) is vacated.
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell(&makecol(3))),
        (0, 2, formula_cell("SUM(A1#)")),
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 1, 0),
        Some(&num(2.0)),
        "A2 spilled = 2 initially"
    );

    e.edit(CellId::new(s0(), 1, 0), CellInput::Literal(num(99.0)));
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Spill)),
        "A1 anchor now #SPILL! (blocked by the edit into its region)"
    );
    assert_eq!(
        e.value(s0(), 1, 0),
        Some(&num(99.0)),
        "the edited 99 survives"
    );
    assert_eq!(e.value(s0(), 2, 0), Some(&Value::Blank), "A3 vacated");
    // The A1# reader now streams the retracted anchor as a lone cell (#SPILL!),
    // so its SUM propagates the error — loud, never a stale 6.
    assert_eq!(
        e.value(s0(), 0, 2),
        Some(&Value::Error(ErrorKind::Spill)),
        "SUM(A1#) propagates the anchor's #SPILL!"
    );
}

#[test]
fn spill_anchorarray_mutual_reference_is_static_cycle() {
    // A spill-dependency cycle expressed through `A1#` is caught by the STATIC
    // graph, not the fixpoint: `A1#`/`ANCHORARRAY(A1)` contributes the anchor A1
    // as a plain precedent (RFC-0012 finding 4), so A1=SUM(B1#), B1=SUM(A1#) form
    // a real graph cycle → both `#UNSUPPORTED!` (circular). This is why the
    // fixpoint's non-convergence path is reserved for *direct*-reference spill
    // cycles, which the available array producers cannot express deterministically.
    let mut e = Engine::load(build(vec![
        (0, 0, formula_cell("SUM(B1#)")),
        (0, 1, formula_cell("SUM(A1#)")),
    ]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported)),
        "A1 in an A1#-mediated cycle is a static circular reference"
    );
    assert_eq!(
        e.value(s0(), 0, 1),
        Some(&Value::Error(ErrorKind::Unsupported)),
        "B1 likewise"
    );
}

// ---- shared-formula expansion (ECMA-376 §18.17.2) ------------------------

#[test]
fn shared_follow_ons_translate_relative_refs() {
    // Column A is a shared group: A1 (master) = `B1*2`, with A2/A3 bodyless
    // follow-ons (si=0, ref A1:A3). Each follow-on's `B1` shifts by its row
    // offset from the master, so A2 reads B2 and A3 reads B3.
    let mut e = Engine::load(build(vec![
        (0, 0, shared_master_cell("B1*2", 0, "A1:A3")),
        (1, 0, shared_follow_cell(0)),
        (2, 0, shared_follow_cell(0)),
        (0, 1, literal_cell(num(5.0))),  // B1
        (1, 1, literal_cell(num(10.0))), // B2
        (2, 1, literal_cell(num(20.0))), // B3
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&num(10.0)), "A1 = B1*2");
    assert_eq!(e.value(s0(), 1, 0), Some(&num(20.0)), "A2 = B2*2");
    assert_eq!(e.value(s0(), 2, 0), Some(&num(40.0)), "A3 = B3*2");
    // No follow-on refused: shared expansion computes them, not `#UNSUPPORTED!`.
    assert!(
        e.diagnostics().is_empty(),
        "no diagnostics expected: {:?}",
        e.diagnostics()
    );
}

#[test]
fn shared_follow_on_absolute_ref_does_not_shift() {
    // Master A1 = `$B$1+B1`: the absolute `$B$1` is fixed for every follow-on,
    // while the relative `B1` shifts. A2 = $B$1 + B2, A3 = $B$1 + B3.
    let mut e = Engine::load(build(vec![
        (0, 0, shared_master_cell("$B$1+B1", 0, "A1:A3")),
        (1, 0, shared_follow_cell(0)),
        (2, 0, shared_follow_cell(0)),
        (0, 1, literal_cell(num(100.0))), // B1
        (1, 1, literal_cell(num(1.0))),   // B2
        (2, 1, literal_cell(num(2.0))),   // B3
    ]));
    e.recalc();
    assert_eq!(e.value(s0(), 0, 0), Some(&num(200.0)), "A1 = $B$1 + B1");
    assert_eq!(e.value(s0(), 1, 0), Some(&num(101.0)), "A2 = $B$1 + B2");
    assert_eq!(e.value(s0(), 2, 0), Some(&num(102.0)), "A3 = $B$1 + B3");
    assert!(e.diagnostics().is_empty());
}

#[test]
fn shared_follow_on_offgrid_overflow_wraps_modulo_grid() {
    // OXP-210 (RUN 2026-07-16, Excel 16.0): the shared-formula GROUP-LOAD path
    // does NOT emit `#REF!` for an off-grid OVERFLOW relative shift — it WRAPS
    // the reference modulo the grid (row mod 1,048,576). Master A1 = `=Z1048576`
    // (last row of col Z); the follow-on one row below (A2, drow=+1) shifts the
    // relative row to 1,048,577, which wraps back to row 1 → `=Z1`. A sentinel in
    // Z1 makes the wrap target observable end-to-end (not just at the AST level).
    let mut e = Engine::load(build(vec![
        (0, 0, shared_master_cell("Z1048576", 0, "A1:A2")),
        (1, 0, shared_follow_cell(0)),
        (0, 25, literal_cell(num(42.0))), // Z1 — the cell the wrap lands on
    ]));
    e.recalc();
    // Master A1 reads the (blank) last row → 0; the follow-on A2 reads the
    // wrapped `Z1` → 42. The wrap is Excel-faithful (OXP-210), not `#REF!`.
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&num(0.0)),
        "A1 = Z1048576 (blank) = 0"
    );
    assert_eq!(
        e.value(s0(), 1, 0),
        Some(&num(42.0)),
        "A2 = wrapped Z1 = 42 (row 1,048,577 → 1, OXP-210)"
    );
    assert!(
        e.diagnostics_for(s0(), 1, 0).is_empty(),
        "a wrapped in-grid reference yields no diagnostic"
    );
}

#[test]
fn orphan_shared_follow_on_still_declines() {
    // A bodyless follow-on whose `si` has no master on the sheet cannot be
    // expanded; it stays a loud `#UNSUPPORTED!` refusal (never a guess).
    let mut e = Engine::load(build(vec![(0, 0, shared_follow_cell(5))]));
    e.recalc();
    assert_eq!(
        e.value(s0(), 0, 0),
        Some(&Value::Error(ErrorKind::Unsupported)),
        "orphan shared follow-on → #UNSUPPORTED!"
    );
    let diags = e.diagnostics_for(s0(), 0, 0);
    assert_eq!(diags.len(), 1, "one refusal diagnostic");
    assert_eq!(diags[0].kind, DiagnosticKind::UnsupportedConstruct);
    assert!(
        diags[0].message.contains("orphan") && diags[0].message.contains("si=5"),
        "message names the missing master: {}",
        diags[0].message
    );
}

#[test]
fn shared_group_si_namespace_is_per_sheet() {
    // Two sheets each carry a group with the SAME `si=0` but a different master.
    // Each follow-on must resolve against ITS OWN sheet's master, proving the
    // `si` namespace is per-worksheet, not workbook-global.
    let sheet1 = Sheet {
        name: "Sheet1".to_string(),
        sheet_id: 1,
        sheets_index: 0,
        cells: BTreeMap::from([
            ((0, 0), shared_master_cell("B1+1", 0, "A1:A2")),
            ((1, 0), shared_follow_cell(0)),
            ((0, 1), literal_cell(num(10.0))),
            ((1, 1), literal_cell(num(20.0))),
        ]),
        hidden_rows: std::collections::BTreeSet::new(),
    };
    let sheet2 = Sheet {
        name: "Sheet2".to_string(),
        sheet_id: 2,
        sheets_index: 1,
        cells: BTreeMap::from([
            ((0, 0), shared_master_cell("B1*100", 0, "A1:A2")),
            ((1, 0), shared_follow_cell(0)),
            ((0, 1), literal_cell(num(3.0))),
            ((1, 1), literal_cell(num(4.0))),
        ]),
        hidden_rows: std::collections::BTreeSet::new(),
    };
    let wb = Workbook {
        sheets: vec![sheet1, sheet2],
        date_system: DateSystem::default(),
        calc_settings: CalcSettings::default(),
        defined_names: Vec::new(),
        flags: WorkbookFlags::default(),
    };
    let mut e = Engine::load(wb);
    e.recalc();
    let s1 = e.sheet_id("Sheet1").unwrap();
    let s2 = e.sheet_id("Sheet2").unwrap();
    // Sheet1: A1 = B1+1 = 11, A2 = B2+1 = 21.
    assert_eq!(e.value(s1, 0, 0), Some(&num(11.0)));
    assert_eq!(e.value(s1, 1, 0), Some(&num(21.0)));
    // Sheet2: A1 = B1*100 = 300, A2 = B2*100 = 400.
    assert_eq!(e.value(s2, 0, 0), Some(&num(300.0)));
    assert_eq!(e.value(s2, 1, 0), Some(&num(400.0)));
    assert!(e.diagnostics().is_empty());
}

// ---- M2 lane 9 (RFC-0014): parallel recalc determinism -------------------
//
// These run only under `--features parallel` (the CI matrix builds both). They
// assert the parallel path is bit-identical to the forced-serial path — the
// the rayon dependency policy, condition-4 obligation — on a battery of workbooks, and prove
// the gate opens (R8 non-vacuity) / closes (R1) correctly.
#[cfg(feature = "parallel")]
mod parallel_determinism {
    use super::*;

    /// Diagnostics as a sorted, comparable projection.
    fn diag_key(e: &Engine) -> Vec<(CellId, String, String)> {
        let mut v: Vec<(CellId, String, String)> = e
            .diagnostics()
            .into_iter()
            .map(|d| (d.cell, format!("{:?}", d.kind), d.message.clone()))
            .collect();
        v.sort();
        v
    }

    /// Build the workbook twice, load into two engines; recalc one in parallel
    /// and one forced-serial; assert bit-identical value/diagnostic/order state.
    fn assert_identical(mk: impl Fn() -> Workbook, probe: &[(u32, u32)], want_parallel: bool) {
        let s = SheetId(0);
        let mut par = Engine::load(mk());
        let mut ser = Engine::load(mk());
        // R8 non-vacuity: confirm the gate is (or isn't) open as intended, so
        // `recalc()` actually takes the path under test.
        assert_eq!(
            !par.parallel_unsafe(),
            want_parallel,
            "gate openness mismatch for this workbook"
        );
        let rp = par.recalc(); // parallel iff want_parallel
        let rs = ser.recalc_serial(); // always serial
        assert_eq!(rp.evaluated, rs.evaluated, "evaluated count");
        assert_eq!(par.eval_count(), ser.eval_count(), "eval_count");
        assert_eq!(
            par.last_recalc_cells(),
            ser.last_recalc_cells(),
            "last_recalc_cells must be canonical plan order on both paths (R2)"
        );
        for &(r, c) in probe {
            let cid = CellId::new(s, r, c);
            assert_eq!(
                par.value_at(cid),
                ser.value_at(cid),
                "value divergence at ({r},{c})"
            );
        }
        assert_eq!(diag_key(&par), diag_key(&ser), "diagnostics divergence");
    }

    #[test]
    fn wide_independent_plus_reduction() {
        // 20×10 grid of independent `=r*100+c` cells (all wave 0) + a SUM over
        // the block (wave 1) + a chain off the SUM. Heavy antichain width.
        let mk = || {
            let mut cells = Vec::new();
            for r in 0..20u32 {
                for c in 0..10u32 {
                    cells.push((r, c, formula_cell(&format!("{}*100+{}", r, c))));
                }
            }
            // Reduction in a far column; then a short chain off it.
            cells.push((0, 20, formula_cell("SUM(A1:J20)")));
            cells.push((1, 20, formula_cell("U1*2")));
            cells.push((2, 20, formula_cell("U2+U1")));
            build(cells)
        };
        assert_identical(mk, &[(0, 0), (19, 9), (0, 20), (1, 20), (2, 20)], true);
    }

    #[test]
    fn deep_chain_one_cell_per_wave() {
        // A1=1; A2=A1+1; … A50=A49+1 — 50 waves of one cell each. No speedup,
        // but must be bit-identical (and exercises the wave loop's degenerate
        // shape).
        let mk = || {
            let mut cells = vec![(0, 0, formula_cell("1"))];
            for r in 1..50u32 {
                cells.push((r, 0, formula_cell(&format!("A{}+1", r))));
            }
            build(cells)
        };
        assert_identical(mk, &[(0, 0), (24, 0), (49, 0)], true);
    }

    #[test]
    fn diamonds_and_errors_and_unsupported() {
        // Diamond joins + a #DIV/0! value + an unsupported-function diagnostic —
        // all parallel-safe. Exercises value AND diagnostic parity.
        let mk = || {
            build(vec![
                (0, 0, formula_cell("10")),
                (0, 1, formula_cell("A1*2")),
                (0, 2, formula_cell("A1+5")),
                (0, 3, formula_cell("B1+C1")),       // diamond join
                (1, 0, formula_cell("1/0")),         // #DIV/0! (a value, no diag)
                (1, 1, formula_cell("NOSUCHFN(1)")), // #UNSUPPORTED! + diagnostic
            ])
        };
        assert_identical(mk, &[(0, 3), (1, 0), (1, 1)], true);
    }

    #[test]
    fn cycle_wave_is_a_serial_barrier() {
        // A 2-cycle {A1,B1} plus a dependent C1 and an independent D1. The cycle
        // wave runs serially in both paths; the rest matches.
        let mk = || {
            build(vec![
                (0, 0, formula_cell("B1+1")), // A1  ┐ cycle
                (0, 1, formula_cell("A1+1")), // B1  ┘
                (0, 2, formula_cell("A1+B1")),
                (0, 3, formula_cell("99")),
            ])
        };
        assert_identical(mk, &[(0, 0), (0, 1), (0, 2), (0, 3)], true);
    }

    #[test]
    fn gate_closes_on_spill_and_transformers() {
        // R1: a workbook with a spiller (SEQUENCE) OR a reference transformer
        // (INDIRECT/OFFSET) must take the SERIAL path (gate closed); results
        // still match trivially (both serial).
        let seq = || build(vec![(0, 0, formula_cell("_xlfn.SEQUENCE(3)"))]);
        assert_identical(seq, &[(0, 0)], false);

        let indirect = || {
            build(vec![
                (0, 0, formula_cell("42")),
                (0, 1, formula_cell("INDIRECT(\"A1\")")),
            ])
        };
        assert_identical(indirect, &[(0, 0), (0, 1)], false);

        let arr_lit = || build(vec![(0, 0, formula_cell("SUM({1,2,3})"))]);
        assert_identical(arr_lit, &[(0, 0)], false);

        // A lambda array special form (MAKEARRAY) is not a registry function, so
        // the gate must recognise it via `is_array_special_form` (R3-adjacent).
        let makearray = || build(vec![(0, 0, formula_cell(&makecol(3)))]);
        assert_identical(makearray, &[(0, 0)], false);
    }

    #[test]
    fn r3_staged_array_backstop_bails_and_serial_fallback_is_identical() {
        // Directly exercise the R3 backstop by simulating a gate MISS: drive the
        // parallel executor at a workbook that DOES spill, as if
        // `parallel_unsafe` had failed to flag it. `try_recalc_parallel` must
        // apply only the spill-free prefix (wave 0: D1), detect the wave-1
        // staged array (B1 = SEQUENCE), bail (return `false`), and roll back so
        // the caller's serial `drive_recalc` — what `recalc()` runs on a bail —
        // is bit-identical to a clean serial recalc.
        let s = SheetId(0);
        let mk = || {
            build(vec![
                (0, 3, formula_cell("1+1")), // D1 — wave 0, spill-free prefix
                (0, 1, formula_cell("_xlfn.SEQUENCE(D1)")), // B1 — wave 1, spills 1;2
            ])
        };

        // Clean serial baseline.
        let mut ser = Engine::load(mk());
        ser.recalc_serial();

        // Parallel attempt bypassing the gate: `try_recalc_parallel` must bail,
        // then `recalc()`'s fallback path runs `drive_recalc` on the same plan.
        let mut par = Engine::load(mk());
        let plan = par.graph.full_plan(par.calc);
        assert!(
            !par.try_recalc_parallel(&plan),
            "the staged array must trip the R3 backstop and bail"
        );
        par.drive_recalc(plan);
        par.graph.clear_dirty();

        // Bit-identical: values (incl. the spilled B1:B2), counters, order, diags.
        for &(r, c) in &[(0u32, 3u32), (0, 1), (1, 1)] {
            let cid = CellId::new(s, r, c);
            assert_eq!(
                par.value_at(cid),
                ser.value_at(cid),
                "R3 fallback value divergence at ({r},{c})"
            );
        }
        assert_eq!(par.eval_count(), ser.eval_count(), "R3 fallback eval_count");
        assert_eq!(
            par.last_recalc_cells(),
            ser.last_recalc_cells(),
            "R3 fallback last_recalc_cells order"
        );
        assert_eq!(diag_key(&par), diag_key(&ser), "R3 fallback diagnostics");
    }
}
