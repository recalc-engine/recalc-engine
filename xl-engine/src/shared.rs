//! Shared-formula expansion (ECMA-376 §18.17.2 "Shared Formulas").
//!
//! OOXML stores a run of structurally-identical formulas as one **shared
//! group**: a MASTER cell carries the formula text plus `<f t="shared"
//! ref="A1:A10" si="0">A1*B1</f>`, and every other cell in the group is a
//! bodyless FOLLOW-ON `<f t="shared" si="0"/>` (`text = None`). A follow-on's
//! formula is the master's formula with every **relative** reference shifted by
//! the follow-on's offset from the master, while **absolute** (`$`) references
//! stay put — exactly Excel's fill/copy semantics. Recalc does not persist the
//! group; it expands each follow-on to a concrete AST at load and compiles it
//! through the same path a normal parsed formula uses.
//!
//! This module is **engine-local**: it consumes already-public `xl-io`
//! (`RawFormula`/`FormulaKind`) and `xl-ast` (`Expr`/`Reference`/`RefKind`/
//! `Axis`) shapes and changes no cross-crate interface.
//!
//! # Scope
//! Only `t="shared"` groups are expanded here. `t="array"` (legacy CSE) and
//! `t="dataTable"` (what-if data tables) follow-on materialization is a
//! **separate lane** and stays a loud `#UNSUPPORTED!` refusal (a few cells on
//! the corpus); see [`crate::Engine::load`].
//!
//! # Off-grid (two directions, **not symmetric** — each pinned separately)
//! An off-grid relative shift is handled by [`shift_axis`], and the two
//! directions are implemented to exactly what was measured for each (they were
//! pinned by *different* experiments, so a symmetric rule would be a guess):
//!
//! * **Overflow** — a relative axis pushed past the grid maximum (`> XFD` /
//!   `> row 1,048,576`) **wraps modulo the grid dimension** (row mod 1,048,576,
//!   col mod 16,384), matching real Excel's shared-formula GROUP-LOAD path
//!   ([`translate`] → the wrapped reference). Measured by **OXP-210**
//!   (`=Z1048576` +1 row → `=Z1`; `=XFD1` +1 col → `=A1`). This is the direction
//!   reachable with the canonical master-at-group-top-left group shape Excel
//!   itself emits.
//! * **Underflow** — a relative axis pushed below 1 (`< A` / `< row 1`) is
//!   replaced with [`ExprKind::Error`]`(`[`ErrorKind::Ref`]`)` ([`translate`] →
//!   `#REF!`) and is **NOT wrapped**. This branch is **structurally
//!   unauthorable** (OXP-222): Excel accepts a shared group only with the master
//!   at the group top-left, so every follow-on offset is ≥0 on both axes and the
//!   translated index is always ≥1 — a group-load translation can only overflow,
//!   never underflow, so no Excel-*loadable* file ever reaches it. The interim
//!   `#REF!` is retained because it is exactly what the only authorable off-edge
//!   mechanism, interactive `Range.FillUp`/`FillLeft`, produces (OXP-222) — a
//!   *different* mechanism than group-load (it yields `#REF!` on OVERFLOW too,
//!   where group-load wraps), so it cannot pin group-load underflow. Applying the
//!   overflow wrap symmetrically to underflow would assert an unmeasured *and*
//!   unauthorable reading (a guess). See the OXP-210 / OXP-212 / OXP-222 blocks
//!   below.
//!
//! # Provenance
//! Rule and element/attribute names: ECMA-376 5th ed. Part 1, §18.17.2 (shared
//! formulas) and §18.3.1.40 (`CT_CellFormula` / `ST_CellFormulaType`).
//
// OXP-210 — RUN 2026-07-16, Excel 16.0 (job 7d9335ab, sha 5f915fe293b3),
// `RUN-2026-07-16-oracle01`. Authored as REAL shared groups
// (`tools/oracle/manual_probes.py::oxp_210`) so Excel itself performs the
// translation on load; every observed value is Excel's own. Results
// (`docs/oracle-experiments.md`, OXP-210 → run):
//   CONFIRMED (8/10 edges — this module matches Excel exactly):
//     * relative shift down a column (`=A1`→`=A2`→`=A3`);
//     * whole-column `A:A` shift by the column delta (`SUM(A:A)`→`SUM(B:B)`);
//     * mixed `$A1` — row axis shifts under a row shift, column stays `A` under
//       a column shift (Excel stored `=$A1` unchanged at S4);
//     * mixed `A$1` — column axis shifts under a column shift (`=B$1`), row
//       stays `1` under a row shift (Excel stored `=A$1` unchanged at T4);
//     * fully-absolute `$A$1` never moves under either shift;
//     * per-sheet `si` scoping — sheet `Scope2`'s `si=0` follow-on expanded
//       against Scope2's OWN master (`=AA2`), not sheet 1's `si=0` master.
//   OFF-GRID OVERFLOW (Excel WRAPS modulo the grid — now IMPLEMENTED here):
//     * row overflow: master `=Z1048576`, +1-row follow-on → Excel stored
//       **`=Z1`** (row 1,048,577 wrapped to row 1), NOT `#REF!`;
//     * col overflow: master `=XFD1`, +1-col follow-on → Excel stored
//       **`=A1`** (col 16,385 wrapped to col 1), NOT `#REF!`.
// RESOLVED (task #34, this change): [`shift_axis`]'s overflow branch now applies
// the measured modular wrap `((idx-1).rem_euclid(max)) + 1` (an identity for
// on-grid axes), so the OVERFLOW cases above are Excel-faithful. The two
// `offgrid_*_overflow_*` tests below assert the wrap. OXP-210 pinned ONLY the
// OVERFLOW direction; the UNDERFLOW branch (row 0 / col 0) is handled separately
// per OXP-222 (see below) and stays `#REF!` — it was NOT changed, because a
// symmetric `rem_euclid` there would assert an unmeasured *and* unauthorable
// reading. The `offgrid_*_underflow_is_ref_error` tests below assert that
// retained `#REF!`.
//
// OXP-212 (RUN 2026-07-16, job 55485189) first attempted the underflow direction
// with a STATIC fill-up/fill-left fixture (master at the bottom/right of its
// group); Excel **REFUSED TO OPEN the workbook** (HRESULT 0x800A03EC, the
// refusal-as-signal of OXP-052), so no static fixture can reach underflow. The
// prescribed fallback — a live COM `Range.FillUp`/`FillLeft` probe — then RAN
// (`oracle/live_probe.py::probe_oxp212`, RUN-2026-07-16-oracle01) and returned a
// DECISIVE-but-DISQUALIFYING result: interactive Fill off-grid yields plain
// `#REF!` in ALL FOUR directions — FillUp/FillLeft (underflow) AND, in the
// built-in cross-check, FillDown/FillRight (OVERFLOW). But OXP-210 measured the
// OVERFLOW of the shared-formula GROUP-LOAD path as a modular WRAP (`=Z1`,`=A1`),
// NOT `#REF!`. Fill-overflow (`#REF!`) therefore CONTRADICTS load-overflow
// (wrap): interactive Fill/Copy and shared-group load-time expansion are two
// DIFFERENT translation mechanisms. So the Fill probe does not — cannot — pin
// the shared-load UNDERFLOW: it measures the wrong mechanism (proven by the
// overflow disagreement). With static fixtures refused and Fill disqualified,
// the shared-load underflow direction is UNMEASURABLE by any available authoring
// path (and, per OXP-222 below, structurally UNAUTHORABLE). The `shift_axis`
// rem_euclid wrap is therefore applied to the OVERFLOW branch ONLY (OXP-210);
// the UNDERFLOW branch keeps `#REF!` — a symmetric wrap there would assert an
// unmeasured underflow reading (a guess). The `offgrid_*_underflow_is_ref_error`
// tests pin that retained `#REF!`. (The `#REF!` also positively MATCHES Excel for
// the interactive Fill/Copy path.)
//
// OXP-222 (RUN 2026-07-18, Excel 16.0, RUN-2026-07-18-oracle01, live-COM
// `oracle/live_probe.py::probe_oxp222`) supersedes OXP-212's refused static
// fixture: it re-ran the four Fill directions (fresh, independent) and confirmed
// interactive Fill stores `=#REF!` off BOTH the top/left edge (FillUp/FillLeft,
// underflow) AND the bottom/right edge (FillDown/FillRight, overflow) — again
// contradicting OXP-210's group-load WRAP, so the two-mechanisms conclusion holds
// under re-measurement. Its NEW contribution is a STRUCTURAL PROOF that the
// group-load underflow branch of `shift_axis` is UNAUTHORABLE, not just
// unmeasured: a shared group is loadable only with its master at the group
// top-left (the invariant OXP-212's fixture violated → Excel refused the file),
// so every follow-on offset is (Δrow ≥ 0, Δcol ≥ 0) and the master's own relative
// ref is ≥ row 1 / col A ⇒ the translated index is ALWAYS ≥ 1. A group-load
// translation can therefore only overflow (OXP-210's wrap), never underflow; the
// `< 1` guard in `shift_axis` is unreachable from any Excel-loadable file and has
// zero corpus-fidelity bearing. Its `#REF!` also positively matches the only
// authorable off-edge mechanism (Fill/Copy). This is exactly what was done in
// task #34 (this change): the OVERFLOW wrap (OXP-210) is implemented, the
// UNDERFLOW branch stays `#REF!` — a symmetric `rem_euclid` on `< 1` would assert
// an unmeasured *and* unauthorable reading. (Probe also attempts a
// SaveAs→reopen→XML persistence capture; SaveAs is unavailable in the
// non-interactive SSH session — 0x800A03EC, empty DefaultFilePath — but `.Formula`
// == the persisted `<f>` text `=#REF!`, so the committed representation is pinned:
// a literal error cell, never a `t="shared"` master.)
// See docs/oracle-experiments.md (OXP-210, OXP-212, OXP-222).

use std::collections::BTreeMap;

use xl_ast::{Axis, Expr, ExprKind, RefKind, Reference};
use xl_io::{FormulaKind, Workbook};
use xl_value::{ErrorKind, SheetId};

/// Largest **1-based** row index (Excel's 1,048,576 rows). Derived from the
/// 0-based [`crate::analyze::MAX_ROW0`] so the two definitions cannot drift.
const MAX_ROW_1BASED: u32 = crate::analyze::MAX_ROW0 + 1;
/// Largest **1-based** column index (Excel's 16,384 columns, `XFD`). Derived
/// from the 0-based [`crate::analyze::MAX_COL0`].
const MAX_COL_1BASED: u32 = crate::analyze::MAX_COL0 + 1;

/// One shared-formula group's master: its **0-based** grid position and its
/// **parsed** formula. A follow-on's formula is [`translate`]d from this by the
/// `(row, col)` delta between the follow-on and the master.
pub(crate) struct SharedMaster {
    /// The master cell's 0-based row.
    pub(crate) row: u32,
    /// The master cell's 0-based column.
    pub(crate) col: u32,
    /// The master's parsed formula (its `<f>` body).
    pub(crate) expr: Expr,
}

/// Collect every sheet's shared-formula masters, keyed by `(sheet, si)`.
///
/// The `si` (shared-index) namespace is **per-worksheet**, not workbook-global
/// (ECMA-376 §18.17.2), so the key carries the [`SheetId`]. A master cell is one
/// whose formula is `t="shared"`, carries body text, and declares an `si`; the
/// master's origin is simply its own cell `(row, col)` — the group's `ref`
/// attribute is not needed. A master whose body fails to parse is **not**
/// recorded (its follow-ons then keep refusing rather than expand a broken AST).
///
/// Deterministic: `workbook.sheets` is tab-ordered and each sheet's `cells` is a
/// `BTreeMap`, so iteration and the resulting `BTreeMap` are order-stable.
pub(crate) fn collect_masters(workbook: &Workbook) -> BTreeMap<(SheetId, u32), SharedMaster> {
    let mut masters: BTreeMap<(SheetId, u32), SharedMaster> = BTreeMap::new();
    for (idx, sheet) in workbook.sheets.iter().enumerate() {
        let sid = SheetId(idx as u32);
        for (&(row, col), cell) in &sheet.cells {
            let Some(raw) = &cell.formula else {
                continue;
            };
            if raw.kind != FormulaKind::Shared {
                continue;
            }
            // A master carries both a body and an `si`; a follow-on has `text =
            // None` and is skipped here (it is expanded against this map later).
            let (Some(text), Some(si)) = (&raw.text, raw.shared_index) else {
                continue;
            };
            if let Ok(expr) = xl_ast::parse(text) {
                masters.insert((sid, si), SharedMaster { row, col, expr });
            }
        }
    }
    masters
}

/// Translate a master formula for a follow-on `drow`/`dcol` cells away
/// (ECMA-376 §18.17.2): deep-clone `expr`, shifting every **relative** A1 axis
/// and leaving **absolute** (`$`) axes, sheet qualifiers, R1C1 refs, and defined
/// names untouched. An off-grid shift is direction-dependent (see [`shift_axis`]):
/// **overflow** past the grid maximum wraps modulo the grid (OXP-210), while
/// **underflow** below 1 becomes `#REF!`
/// ([`ExprKind::Error`]`(`[`ErrorKind::Ref`]`)`, OXP-222).
///
/// `drow` / `dcol` are the follow-on's signed offset from the master
/// (`follow.row - master.row`, `follow.col - master.col`), so a follow-on below
/// and right of its master has positive deltas.
pub(crate) fn translate(expr: &Expr, drow: i64, dcol: i64) -> Expr {
    let kind = match &expr.kind {
        // A reference is the only node whose payload shifts — and the only one
        // that can turn into `#REF!` — so it builds its own node (span-preserved).
        ExprKind::Ref(r) => return translate_ref(r, expr.span, drow, dcol),

        // Names never shift (a defined name resolves to its own target).
        ExprKind::Name(_) => expr.kind.clone(),

        // Structural recursion: rebuild each child translated, preserve operators.
        ExprKind::Unary { op, expr: inner } => ExprKind::Unary {
            op: *op,
            expr: Box::new(translate(inner, drow, dcol)),
        },
        ExprKind::Postfix { op, expr: inner } => ExprKind::Postfix {
            op: *op,
            expr: Box::new(translate(inner, drow, dcol)),
        },
        ExprKind::Binary { op, lhs, rhs } => ExprKind::Binary {
            op: *op,
            lhs: Box::new(translate(lhs, drow, dcol)),
            rhs: Box::new(translate(rhs, drow, dcol)),
        },
        ExprKind::Call { name, args } => ExprKind::Call {
            name: name.clone(),
            args: args.iter().map(|a| translate(a, drow, dcol)).collect(),
        },
        ExprKind::Array(rows) => ExprKind::Array(
            rows.iter()
                .map(|row| row.iter().map(|e| translate(e, drow, dcol)).collect())
                .collect(),
        ),
        ExprKind::Paren(inner) => ExprKind::Paren(Box::new(translate(inner, drow, dcol))),
        ExprKind::ImplicitIntersection(inner) => {
            ExprKind::ImplicitIntersection(Box::new(translate(inner, drow, dcol)))
        }

        // Literals and intentionally-unparsed constructs never shift.
        ExprKind::Number(_)
        | ExprKind::Text(_)
        | ExprKind::Bool(_)
        | ExprKind::Error(_)
        | ExprKind::Missing
        | ExprKind::Unsupported { .. } => expr.kind.clone(),
    };
    Expr::new(kind, expr.span)
}

/// Translate one reference node, reusing `span` for the produced node (a
/// shifted `Ref` or, when an axis underflows off-grid, an `#REF!` `Error`; an
/// overflowing axis wraps instead — see [`shift_axis`]). The sheet qualifier —
/// including cross-sheet (`Sheet2!`) and external (`[1]Sheet!`) prefixes — is
/// carried through unchanged; only the grid part shifts.
fn translate_ref(r: &Reference, span: xl_ast::Span, drow: i64, dcol: i64) -> Expr {
    let shifted_kind: Option<RefKind> = match r.kind {
        // A1 cell: shift the column axis by `dcol` and the row axis by `drow`.
        // An axis underflowing off-grid invalidates the whole reference (`#REF!`);
        // an axis overflowing wraps modulo the grid, so it stays a valid `Cell`.
        RefKind::Cell(col_axis, row_axis) => {
            match (
                shift_axis(col_axis, dcol, MAX_COL_1BASED),
                shift_axis(row_axis, drow, MAX_ROW_1BASED),
            ) {
                (Some(c), Some(r)) => Some(RefKind::Cell(c, r)),
                _ => None,
            }
        }
        // Whole column (`A:A` endpoint): shifts by `dcol` only.
        RefKind::Col(axis) => shift_axis(axis, dcol, MAX_COL_1BASED).map(RefKind::Col),
        // Whole row (`1:1` endpoint): shifts by `drow` only.
        RefKind::Row(axis) => shift_axis(axis, drow, MAX_ROW_1BASED).map(RefKind::Row),
        // R1C1 is position-independent (relative = offset, absolute = fixed), so
        // it is a no-op either way — and OOXML stores shared formulas in A1
        // anyway, so this arm is defensive. Carry the reference through verbatim.
        RefKind::R1C1(_) => Some(r.kind),
    };

    match shifted_kind {
        Some(kind) => Expr::new(
            ExprKind::Ref(Reference {
                sheet: r.sheet.clone(),
                kind,
            }),
            span,
        ),
        None => Expr::new(ExprKind::Error(ErrorKind::Ref), span),
    }
}

/// Shift one A1 axis by `delta`, returning the new **1-based** axis.
///
/// The two off-grid directions are **not symmetric** — they are pinned by two
/// different measurements (see the module-header OXP-210 / OXP-222 blocks), so
/// each is implemented to exactly what was measured for it:
///
/// * **Overflow** (`> max_1based`): Excel **wraps the relative reference modulo
///   the grid dimension** (row mod 1,048,576, col mod 16,384) — measured on the
///   shared-formula GROUP-LOAD path by OXP-210 (`=Z1048576` +1 row → `=Z1`;
///   `=XFD1` +1 col → `=A1`), the only reachable off-edge direction for a
///   canonical top-left-master group. Computed 1-based via
///   `((index - 1).rem_euclid(max)) + 1`, which is an identity for on-grid
///   values and the modular wrap past the edge.
/// * **Underflow** (`< 1`): returns `None`, so the caller emits `#REF!`. This
///   branch is **structurally unreachable** from any Excel-*loadable* shared
///   group (OXP-222: a group loads only with its master at the group top-left,
///   so every follow-on offset is ≥0 on both axes and the translated index is
///   always ≥1), so it has zero corpus-fidelity bearing. `#REF!` is retained
///   because it is what the only authorable off-edge mechanism — interactive
///   `Range.FillUp`/`FillLeft` — produces (OXP-222); a symmetric `rem_euclid`
///   wrap here would assert an unmeasured *and* unauthorable reading (a guess),
///   which the never-guess rule forbids.
///
/// An **absolute** (`$`) axis is returned unchanged — absolute references do not
/// move under fill/copy (ECMA-376 §18.17.2) and so can never go off-grid here.
fn shift_axis(axis: Axis, delta: i64, max_1based: u32) -> Option<Axis> {
    if axis.absolute {
        return Some(axis);
    }
    let shifted = i64::from(axis.index) + delta;
    if shifted < 1 {
        // Underflow: unreachable in any Excel-loadable shared group (OXP-222) and
        // NOT wrapped — emit `#REF!` (matches interactive Fill/Copy off the edge).
        return None;
    }
    // On-grid is an identity; overflow wraps modulo the grid dimension (OXP-210).
    let max = i64::from(max_1based);
    let wrapped = (shifted - 1).rem_euclid(max) + 1;
    Some(Axis {
        index: wrapped as u32,
        absolute: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xl_ast::{R1C1Axis, R1C1Ref, Span};

    /// Parse `src` (A1 mode), translate it by `(drow, dcol)`, and render the
    /// result as canonical formula text — the compact way to assert the shift.
    fn tr(src: &str, drow: i64, dcol: i64) -> String {
        let expr = xl_ast::parse(src).expect("fixture formula should parse");
        translate(&expr, drow, dcol).to_string()
    }

    #[test]
    fn relative_cell_shifts_down() {
        // A1 one row down → A2 (the canonical relative fill).
        assert_eq!(tr("A1", 1, 0), "A2");
    }

    #[test]
    fn relative_cell_shifts_right() {
        assert_eq!(tr("A1", 0, 1), "B1");
    }

    #[test]
    fn absolute_cell_never_moves() {
        // $A$1 is fully anchored; no offset shifts it.
        assert_eq!(tr("$A$1", 5, 7), "$A$1");
    }

    #[test]
    fn mixed_absolute_column_relative_row() {
        // $A1: column anchored, row relative → row shifts, column fixed.
        assert_eq!(tr("$A1", 1, 1), "$A2");
    }

    #[test]
    fn mixed_relative_column_absolute_row() {
        // A$1: row anchored, column relative → column shifts, row fixed.
        assert_eq!(tr("A$1", 1, 1), "B$1");
    }

    #[test]
    fn whole_column_shifts_by_dcol() {
        // A:A moves by the column delta only; the row delta is irrelevant.
        assert_eq!(tr("A:A", 9, 1), "B:B");
    }

    #[test]
    fn whole_row_shifts_by_drow() {
        // 1:1 moves by the row delta only.
        assert_eq!(tr("1:1", 1, 9), "2:2");
    }

    #[test]
    fn range_shifts_both_endpoints() {
        assert_eq!(tr("A1:B2", 1, 0), "A2:B3");
    }

    #[test]
    fn function_call_shifts_only_relative_args() {
        // SUM(A1,$B$2) one row down → SUM(A2,$B$2): the absolute arg is fixed.
        assert_eq!(tr("SUM(A1,$B$2)", 1, 0), "SUM(A2,$B$2)");
    }

    #[test]
    fn cross_sheet_qualifier_preserved() {
        // The grid part shifts; the sheet qualifier is carried through intact.
        assert_eq!(tr("Sheet2!A1", 1, 0), "Sheet2!A2");
    }

    // Off-grid, two directions, NOT symmetric (module-header OXP-210 / OXP-222):
    // OVERFLOW past the grid maximum WRAPS modulo the grid (measured, OXP-210);
    // UNDERFLOW below 1 stays `#REF!` (structurally unauthorable in group-load,
    // OXP-222 — and what interactive Fill/Copy produces off the edge).

    // --- OVERFLOW: wraps modulo the grid (OXP-210, RUN 2026-07-16) ---
    #[test]
    fn offgrid_row_overflow_wraps_modulo_grid() {
        // OXP-210: master `=Z1048576` +1 row → Excel stored `=Z1`. Here the col
        // is A: row 1,048,576 + 1 = 1,048,577 wraps to row 1.
        assert_eq!(tr("A1048576", 1, 0), "A1");
    }

    #[test]
    fn offgrid_column_overflow_wraps_modulo_grid() {
        // OXP-210: master `=XFD1` +1 col → Excel stored `=A1` (col 16,385 → 1).
        assert_eq!(tr("XFD1", 0, 1), "A1");
    }

    #[test]
    fn offgrid_overflow_wraps_deep_past_the_edge() {
        // The wrap is a true modulo, not a single-step clamp: XFD (col 16,384)
        // shifted +2 → col 16,386 → wraps to col 2 (B).
        assert_eq!(tr("XFD1", 0, 2), "B1");
        // Row 1,048,575 shifted +3 → 1,048,578 → wraps to row 2.
        assert_eq!(tr("A1048575", 3, 0), "A2");
    }

    #[test]
    fn overflow_wrap_lands_on_grid_maximum_exactly() {
        // A boundary check that the modular arithmetic is inclusive of the max:
        // col XFC (16,383) + 1 = 16,384 = XFD is on-grid (no wrap); XFD + 1 wraps.
        assert_eq!(tr("XFC1", 0, 1), "XFD1");
        assert_eq!(tr("XFD1", 0, 1), "A1");
        // Row 1,048,575 + 1 = 1,048,576 is the last row (on-grid); +2 wraps to 1.
        assert_eq!(tr("A1048575", 1, 0), "A1048576");
        assert_eq!(tr("A1048575", 2, 0), "A1");
    }

    // --- UNDERFLOW: stays `#REF!` (OXP-222; unauthorable in group-load) ---
    #[test]
    fn offgrid_row_underflow_is_ref_error() {
        // A1 shifted up one row lands on row 0 (< 1). Underflow is NOT wrapped
        // (unauthorable in any Excel-loadable group, OXP-222) → `#REF!`.
        assert_eq!(tr("A1", -1, 0), "#REF!");
    }

    #[test]
    fn offgrid_column_underflow_is_ref_error() {
        // A1 shifted left one column lands on col 0 (< 1) → `#REF!` (not wrapped).
        assert_eq!(tr("A1", 0, -1), "#REF!");
    }

    #[test]
    fn underflow_is_ref_error_not_wrapped_to_the_far_edge() {
        // Guard against an accidental `rem_euclid` on the underflow branch: a
        // symmetric wrap would have turned this into the LAST row/col (XFD /
        // 1,048,576). It must be `#REF!` instead.
        assert_eq!(tr("A1", 0, -1), "#REF!"); // NOT `XFD1`
        assert_eq!(tr("A1", -1, 0), "#REF!"); // NOT `A1048576`
    }

    // --- mixed overflow/underflow on the two axes of one cell ref ---
    #[test]
    fn offgrid_whole_column_wraps_each_endpoint() {
        // Whole-column `XFD:XFD` shifted +1 col: each endpoint's column axis
        // overflows and WRAPS to A (same axis primitive as the OXP-210 cell wrap;
        // the whole-range form itself was not independently pinned, but it is the
        // identical `shift_axis` operation), so the range becomes `A:A`.
        assert_eq!(tr("XFD:XFD", 0, 1), "A:A");
    }

    #[test]
    fn cell_overflow_on_one_axis_still_wraps_whole_ref() {
        // Row overflows (wraps) while the column stays on-grid → a valid wrapped
        // cell, not `#REF!`: `A1048576` +1 row, +1 col → row wraps to 1, col A→B.
        assert_eq!(tr("A1048576", 1, 1), "B1");
    }

    #[test]
    fn cell_underflow_on_one_axis_invalidates_whole_ref() {
        // Column stays on-grid (A→B) but the row underflows → the entire cell
        // reference is `#REF!`, not a half-shifted address. Underflow on EITHER
        // axis invalidates the whole ref even if the other axis (here col) would
        // otherwise overflow-wrap.
        assert_eq!(tr("A1", -1, 1), "#REF!");
        // Column underflows while row overflow-wraps → still `#REF!`.
        assert_eq!(tr("A1048576", 1, -1), "#REF!");
    }

    #[test]
    fn absolute_axis_never_goes_offgrid() {
        // Absolute axes do not move under fill/copy, so a huge delta cannot push
        // an anchored axis off-grid in either direction — no wrap, no `#REF!`.
        assert_eq!(tr("$A$1", -100, -100), "$A$1");
        assert_eq!(tr("$XFD$1048576", 100, 100), "$XFD$1048576");
        // Mixed: only the relative axis moves; here the relative row overflows and
        // wraps while the absolute column stays anchored at A.
        assert_eq!(tr("$A1048576", 1, 5), "$A1");
    }

    #[test]
    fn r1c1_reference_is_unchanged() {
        // R1C1 refs are position-independent; translate is a structural no-op.
        let expr = Expr::new(
            ExprKind::Ref(Reference {
                sheet: None,
                kind: RefKind::R1C1(R1C1Ref {
                    row: Some(R1C1Axis::Relative(2)),
                    col: Some(R1C1Axis::Absolute(3)),
                }),
            }),
            Span::new(0, 4),
        );
        assert_eq!(translate(&expr, 5, 5), expr);
    }

    #[test]
    fn defined_name_is_unchanged() {
        // A bare defined name does not shift.
        assert_eq!(tr("SomeName", 3, 4), "SomeName");
    }

    #[test]
    fn span_preserved_on_shifted_ref() {
        let expr = xl_ast::parse("A1").unwrap();
        let out = translate(&expr, 1, 0);
        assert_eq!(out.span, expr.span);
    }

    #[test]
    fn span_preserved_on_offgrid_error() {
        let expr = xl_ast::parse("A1").unwrap();
        let out = translate(&expr, -1, 0);
        assert!(matches!(out.kind, ExprKind::Error(ErrorKind::Ref)));
        // The #REF! node reuses the original reference's span for diagnostics.
        assert_eq!(out.span, expr.span);
    }
}
