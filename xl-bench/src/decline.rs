//! Decline attribution — root-cause classification of every *declined* cell.
//!
//! A **declined** cell is one the benchmark funnel scores as
//! [`crate::diff::CellStatus::EngineUnsupported`] — i.e. Recalc explicitly refused
//! to produce a scorable value for a cell the oracle *does* have a value for.
//! In practice its recalculated value is one of Recalc's own sentinel errors
//! (`#UNSUPPORTED!` / `#BLOCKED!` / `#RESOURCE!` —
//! [`xl_value::ErrorKind::is_recalc_sentinel`]) or an engine-internal lambda,
//! AND the oracle recorded a real value for the cell (a sentinel-valued cell
//! with **no** oracle is `NoOracle`, not declined — [`crate::diff::classify`]
//! resolves `NoOracle` first). Recalc emits these instead of guessing
//! (`implementation-plan.md` §0, "Never silently wrong"), so on a real corpus
//! the declined mass is the explicit-gap budget: this module answers *why* each
//! declined cell declined, by tracing the cascade back to its origin. The tool's
//! `total_declined` equals the funnel's `EngineUnsupported` count by
//! construction (see [`attribute_workbook`]).
//!
//! # The ten classes (mutually exclusive, exhaustive)
//! Every declined cell is assigned **exactly one** [`DeclineClass`]; the ten
//! counts sum to the total declined-cell count (asserted in code and tested):
//!
//! 1. [`DeclineClass::ExternalDirect`] — the cell's OWN formula directly
//!    references an external workbook (`[1]Sheet1!A1`, `'[1]Book'!A1`).
//! 2. [`DeclineClass::ExternalCascade`] — declined only because a precedent
//!    (transitively) roots in an `ExternalDirect` cell.
//! 3. [`DeclineClass::UnimplementedFnDirect`] — the cell's own formula directly
//!    calls a function not in the registry. (A structured/table reference is
//!    NOT counted here — it is a construct refusal, see class 10.)
//! 4. [`DeclineClass::UnimplementedFnCascade`] — roots (transitively) in an
//!    `UnimplementedFnDirect` cell.
//! 5. [`DeclineClass::VolatileAntitarget`] — own formula names a volatile
//!    anti-target (`TODAY`/`NOW`), OR roots in one (direct or cascade).
//! 6. [`DeclineClass::BlockedIo`] — computed result is `#BLOCKED!`, or the
//!    formula names `WEBSERVICE`/`RTD`/`STOCKHISTORY` (direct or cascade).
//! 7. [`DeclineClass::VbaUdfAddin`] — the formula calls an add-in / UDF
//!    (`_XLL.*`, or a workbook defined-name invoked as a function) — direct or
//!    cascade.
//! 8. [`DeclineClass::SharedFollowonUnexpanded`] — a bodyless shared-formula
//!    follow-on (`<f t="shared">` carrying no body of its own) that **genuinely
//!    could not be expanded**: it has no `si`, no master cell exists for its
//!    `(sheet, si)`, or the master's body fails to parse. A follow-on whose
//!    master *does* parse is re-expanded (the master [`translate`]d to the
//!    follow-on's position) and attributed to its TRUE cause — external,
//!    unimplemented-fn, cascade, etc. — so this class is now only the small,
//!    genuinely-unexpandable residual (see `docs/shared-residual-classification.md`).
//! 9. [`DeclineClass::ArrayFollowonUnmaterialized`] — a bodyless array-entered
//!    (`<f t="array">` CSE) or data-table follow-on whose master's spill was not
//!    materialized into this cell. (These are unexpanded by design — the engine
//!    does not translate array / data-table groups — so no master check applies.)
//! 10. [`DeclineClass::OtherUnattributed`] — declined, but the cascade cannot be
//!     traced to any of the above (a structured/table reference construct, a
//!     reference-form construct like a whole-column/whole-row/3-D reference that
//!     is separately unsupported, a `#REF!`-rooted chain, a bodyless *normal*
//!     follow-on, an unparseable formula, a **would-expand shared follow-on that
//!     declines for a runtime reason** with no static cause, or genuinely
//!     unclear). Reported explicitly; **never** folded into another class.
//!
//! # Direct beats cascade; the deterministic tiebreak
//! A cell is classified by its **own** direct cause when it has one; only when
//! its own formula has no direct cause are its precedents traced. When several
//! causes coexist — several direct causes in one formula, or several declined
//! roots with different causes across a cascade — a single fixed priority order
//! breaks the tie:
//!
//! ```text
//! external  >  unimplemented_fn  >  volatile  >  blocked_io  >  vba_udf
//! ```
//!
//! (See [`CauseSet::pick`].) The order is applied identically to the
//! multi-direct-cause case and the multi-root cascade case, so the result is
//! deterministic and independent of formula/traversal order. Because the
//! cascade cause is the minimum-priority (highest-precedence) cause over the
//! union of every reachable declined precedent's direct cause, and the tiebreak
//! is that same minimum, tracing is order-insensitive.
//!
//! # Determinism
//! Every structure is a `BTreeMap`/`BTreeSet`; declined cells are processed in
//! [`xl_engine::CellId`] order; the function tally and per-class output sort
//! deterministically. No `HashMap` iteration order can leak into a count.
//!
//! # Provenance
//! - The declined predicate is [`crate::diff::classify`] returning
//!   [`crate::diff::CellStatus::EngineUnsupported`] — which layers
//!   [`xl_value::ErrorKind::is_recalc_sentinel`] (`xl-value/src/error.rs`) under
//!   a `NoOracle`-wins-first rule — so the tool reconciles exactly with the
//!   benchmark funnel.
//! - The registry-miss ground truth is `xl_fn::lookup` — the same table the
//!   engine consults at `xl-engine/src/eval.rs` before emitting
//!   `DiagnosticKind::UnknownFunction`. The reference-transformer set
//!   `{OFFSET, INDIRECT, ANCHORARRAY}` (supported outside the registry) mirrors
//!   `xl-engine/src/refx.rs::is_ref_returning`; it is duplicated here as a small
//!   constant because that predicate is `pub(crate)` to `xl-engine`.
//! - External-workbook reference syntax was read from `xl-ast` (`SheetRef`
//!   carries the `[n]` index inside the sheet-name string for the quoted form
//!   `'[1]Sheet1'!A1`; the unquoted `[1]…` form lexes to a `Brackets` token that
//!   the parser turns into `ExprKind::Unsupported`, or fails to parse — both
//!   covered by the raw-text `[digits]` fallback).

use std::collections::{BTreeMap, BTreeSet};

use xl_ast::{Axis, Expr, ExprKind, RefKind, Reference, SheetRef, parse};
use xl_engine::{CellId, Engine};
use xl_io::FormulaKind;
use xl_value::{ErrorKind, SheetId, Value};

use crate::diff::{CellStatus, DiffConfig, classify};
use crate::report::RunError;
use crate::sidecar::{CachedValueSource, SidecarSource};

/// Largest 0-based row index (Excel `1048576` rows).
const MAX_ROW0: u32 = 1_048_575;
/// Largest 0-based column index (Excel `XFD` = 16384 columns).
const MAX_COL0: u32 = 16_383;
/// Largest **1-based** row index — the off-grid guard for shared-formula
/// translation (mirrors `xl-engine/src/shared.rs::MAX_ROW_1BASED`).
const MAX_ROW_1BASED: u32 = MAX_ROW0 + 1;
/// Largest **1-based** column index — the off-grid guard for shared-formula
/// translation (mirrors `xl-engine/src/shared.rs::MAX_COL_1BASED`).
const MAX_COL_1BASED: u32 = MAX_COL0 + 1;

/// Reference-transformer functions the engine supports **outside** the registry
/// (RFC-0003; `xl-engine/src/refx.rs::is_ref_returning`). A call to one of these
/// is *not* an unimplemented function even though `xl_fn::lookup` misses it.
pub(crate) const REF_TRANSFORMERS: &[&str] = &["OFFSET", "INDIRECT", "ANCHORARRAY"];

/// Special-form functions the engine evaluates itself (M2 lane 2), dispatched in
/// `xl-engine/src/eval.rs::eval_special_form` **before** the registry lookup, so
/// `xl_fn::lookup` misses them yet they are supported (not unimplemented). A
/// declined cell using one of these declines because of an unsupported *edge*
/// (broadcasting, a lambda-valued element, an unpinned corner) — a construct
/// refusal that belongs in `other_unattributed`, never in `unimplemented_fn`.
/// `ISOMITTED` is recognized-but-deferred here; it is treated as supported so a
/// rare `ISOMITTED` decline is not overstated as a missing function.
pub(crate) const SPECIAL_FORMS: &[&str] = &[
    "LET",
    "LAMBDA",
    "MAP",
    "REDUCE",
    "SCAN",
    "BYROW",
    "BYCOL",
    "MAKEARRAY",
    "ISOMITTED",
];

/// Sandbox-blocked I/O functions (`WEBSERVICE`/`RTD`/`STOCKHISTORY`). Named
/// here because they are unregistered today (so a bare `lookup` miss would
/// otherwise route them to `unimplemented_fn`); the sandbox contract
/// (a Recalc design rule) is that they are refused as blocked I/O.
const BLOCKED_IO_FNS: &[&str] = &["WEBSERVICE", "RTD", "STOCKHISTORY"];

/// The volatile **anti-targets**: volatile functions Recalc deliberately leaves
/// uncomputed because their cached oracle value is the file's unreproducible
/// save-timestamp (`xl-bench/src/tier0.rs` `T0_UNSUPPORTED_V1`). Narrowly
/// `TODAY`/`NOW` per the class definition.
///
/// The other volatiles are **not** anti-targets and are deliberately excluded:
/// `RAND`, `RANDBETWEEN`, `CELL`, and `INFO` are **unregistered** today, so they
/// evaluate to `#UNSUPPORTED!` and correctly fall to `unimplemented_fn` (a
/// missing function, not a save-timestamp refusal); only `OFFSET`/`INDIRECT` are
/// supported outside the registry (see `REF_TRANSFORMERS`).
const VOLATILE_ANTITARGETS: &[&str] = &["TODAY", "NOW"];

/// The placeholder text a bodyless shared/array follow-on formula cell is given
/// (mirrors [`crate::report::run_workbook`]). Such a cell carries no formula
/// body of its own; a **shared** follow-on is resolved by translating its group
/// master (see [`translate`] / [`SharedMaster`]), while an **array**/data-table
/// follow-on stays unmaterialized ([`DeclineClass::ArrayFollowonUnmaterialized`]).
pub(crate) const BODYLESS_PLACEHOLDER: &str = "<shared/array follow-on, no formula body>";

/// A single declined cell's root-cause class. See the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeclineClass {
    ExternalDirect,
    ExternalCascade,
    UnimplementedFnDirect,
    UnimplementedFnCascade,
    VolatileAntitarget,
    BlockedIo,
    VbaUdfAddin,
    SharedFollowonUnexpanded,
    ArrayFollowonUnmaterialized,
    OtherUnattributed,
}

impl DeclineClass {
    /// All ten classes, in report order (also their [`Ord`] order).
    pub const ALL: [DeclineClass; 10] = [
        DeclineClass::ExternalDirect,
        DeclineClass::ExternalCascade,
        DeclineClass::UnimplementedFnDirect,
        DeclineClass::UnimplementedFnCascade,
        DeclineClass::VolatileAntitarget,
        DeclineClass::BlockedIo,
        DeclineClass::VbaUdfAddin,
        DeclineClass::SharedFollowonUnexpanded,
        DeclineClass::ArrayFollowonUnmaterialized,
        DeclineClass::OtherUnattributed,
    ];

    /// A short, stable machine-readable tag.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            DeclineClass::ExternalDirect => "external_direct",
            DeclineClass::ExternalCascade => "external_cascade",
            DeclineClass::UnimplementedFnDirect => "unimplemented_fn_direct",
            DeclineClass::UnimplementedFnCascade => "unimplemented_fn_cascade",
            DeclineClass::VolatileAntitarget => "volatile_antitarget",
            DeclineClass::BlockedIo => "blocked_io",
            DeclineClass::VbaUdfAddin => "vba_udf_addin",
            DeclineClass::SharedFollowonUnexpanded => "shared_followon_unexpanded",
            DeclineClass::ArrayFollowonUnmaterialized => "array_followon_unmaterialized",
            DeclineClass::OtherUnattributed => "other_unattributed",
        }
    }

    /// Stable index into the per-class count array.
    #[must_use]
    fn idx(self) -> usize {
        match self {
            DeclineClass::ExternalDirect => 0,
            DeclineClass::ExternalCascade => 1,
            DeclineClass::UnimplementedFnDirect => 2,
            DeclineClass::UnimplementedFnCascade => 3,
            DeclineClass::VolatileAntitarget => 4,
            DeclineClass::BlockedIo => 5,
            DeclineClass::VbaUdfAddin => 6,
            DeclineClass::SharedFollowonUnexpanded => 7,
            DeclineClass::ArrayFollowonUnmaterialized => 8,
            DeclineClass::OtherUnattributed => 9,
        }
    }
}

/// One root cause. Ranked by the fixed priority order (lower rank wins).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cause {
    External,
    UnimplementedFn,
    Volatile,
    BlockedIo,
    VbaUdf,
}

/// The set of direct causes a single formula exhibits. Combined across a
/// cascade by [`CauseSet::or`], then resolved to a single winner by
/// [`CauseSet::pick`] using the documented priority order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CauseSet {
    external: bool,
    unimplemented_fn: bool,
    volatile: bool,
    blocked_io: bool,
    vba_udf: bool,
}

impl CauseSet {
    /// Union of two cause sets.
    #[must_use]
    fn or(self, o: CauseSet) -> CauseSet {
        CauseSet {
            external: self.external || o.external,
            unimplemented_fn: self.unimplemented_fn || o.unimplemented_fn,
            volatile: self.volatile || o.volatile,
            blocked_io: self.blocked_io || o.blocked_io,
            vba_udf: self.vba_udf || o.vba_udf,
        }
    }

    /// How many distinct causes are set (0..=5). Used to count multi-cause
    /// declined cells — cells whose own formula exhibits several independent
    /// causes, so the deterministic tiebreak in [`CauseSet::pick`] actually had
    /// a choice to make (see [`WorkbookDeclineResult::multi_cause_cells`]).
    #[must_use]
    fn distinct_count(self) -> usize {
        [
            self.external,
            self.unimplemented_fn,
            self.volatile,
            self.blocked_io,
            self.vba_udf,
        ]
        .into_iter()
        .filter(|b| *b)
        .count()
    }

    /// The winning cause under the fixed priority order
    /// `external > unimplemented_fn > volatile > blocked_io > vba_udf`, or
    /// `None` if the set is empty.
    #[must_use]
    fn pick(self) -> Option<Cause> {
        if self.external {
            Some(Cause::External)
        } else if self.unimplemented_fn {
            Some(Cause::UnimplementedFn)
        } else if self.volatile {
            Some(Cause::Volatile)
        } else if self.blocked_io {
            Some(Cause::BlockedIo)
        } else if self.vba_udf {
            Some(Cause::VbaUdf)
        } else {
            None
        }
    }
}

/// The per-function category used to classify one call name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FnCategory {
    /// Registered, or a supported reference-transformer — not a decline cause.
    Supported,
    Volatile,
    BlockedIo,
    VbaUdf,
    Unimplemented,
}

/// Classify one canonical (uppercased, `_xlfn`/`_xlws`-stripped) call name.
///
/// Order matters: the add-in / blocked / volatile / ref-transformer buckets are
/// checked before the registry so a name that is *both* (e.g. `WEBSERVICE`,
/// which is unregistered) is routed to its semantic bucket rather than to the
/// registry-miss `Unimplemented` bucket. A name matching a workbook defined name
/// is a UDF invocation.
#[must_use]
fn fn_category(canonical: &str, defined_names_uc: &BTreeSet<String>) -> FnCategory {
    if canonical.starts_with("_XLPM.") {
        // Named-lambda parameter application (the `_xlpm.` authoring prefix). The
        // engine binds and evaluates these through LET/LAMBDA, so a bare
        // `xl_fn::lookup` miss must never tally them as an unimplemented function
        // in a published number.
        return FnCategory::Supported;
    }
    if canonical.starts_with("_XLL.") {
        return FnCategory::VbaUdf;
    }
    if BLOCKED_IO_FNS.contains(&canonical) {
        return FnCategory::BlockedIo;
    }
    if VOLATILE_ANTITARGETS.contains(&canonical) {
        return FnCategory::Volatile;
    }
    if REF_TRANSFORMERS.contains(&canonical) || SPECIAL_FORMS.contains(&canonical) {
        return FnCategory::Supported;
    }
    if xl_fn::lookup(canonical).is_some() {
        return FnCategory::Supported;
    }
    if defined_names_uc.contains(canonical) {
        return FnCategory::VbaUdf;
    }
    FnCategory::Unimplemented
}

/// Whether `text` contains an external-workbook index token `[<digits>]` — the
/// stored form of a cross-workbook reference (`[1]Sheet1!A1`, `'[1]Book'!A1`).
///
/// Used **only** on the unparsed-fallback path (a formula that failed to parse):
/// a genuine unquoted `[1]…` external ref lexes its leading `[1]` to a
/// `Brackets` atom and then trips a trailing-token parse error, so it never
/// reaches the parsed AST. The quoted form (`'[1]Book'!A1`) parses to a `Ref`
/// caught by [`sheet_is_external`] instead.
///
/// Two guards keep this from firing on shapes that merely *look* bracketed:
/// (a) a `[digits]` match inside a double-quoted **string literal** is skipped
/// (e.g. `="[1]x"`); (b) the `[` must **not** be immediately preceded by an
/// identifier character `[A-Za-z0-9_.]`, which rules out an R1C1 offset
/// (`R[1]C[1]`) and a digit-named table column (`Name[2024]`). Any genuine
/// external ref these guards now miss is an *under*-count — the safe direction
/// for a published number, never a flattering over-count.
#[must_use]
pub(crate) fn has_external_bracket(text: &str) -> bool {
    let b = text.as_bytes();
    let mut in_str = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'"' {
            // Toggle string-literal state. An escaped `""` toggles off then on,
            // leaving the inside-string state correct at any later `[`.
            in_str = !in_str;
            i += 1;
            continue;
        }
        if !in_str && c == b'[' {
            let preceded_by_ident = i > 0 && {
                let p = b[i - 1];
                p.is_ascii_alphanumeric() || p == b'_' || p == b'.'
            };
            if !preceded_by_ident {
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                if j > i + 1 && j < b.len() && b[j] == b']' {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

/// Whether a sheet qualifier names an external workbook (its name carries a
/// leading `[n]` index, as in the parsed quoted form `'[1]Sheet1'!A1`).
#[must_use]
fn sheet_is_external(sheet: &Option<SheetRef>) -> bool {
    match sheet {
        Some(sr) => {
            sr.first.starts_with('[') || sr.last.as_deref().is_some_and(|l| l.starts_with('['))
        }
        None => false,
    }
}

// ── Shared-formula translation (clean-room mirror of `xl-engine/src/shared.rs`)
//
// A bodyless `<f t="shared" si="N"/>` follow-on's real formula is its group
// **master**'s formula with every RELATIVE A1 axis shifted by the follow-on's
// `(drow, dcol)` offset from the master and every ABSOLUTE (`$`) axis left put
// (ECMA-376 §18.17.2). The engine expands each follow-on to exactly this AST at
// load and evaluates it through the normal path, so to attribute a would-expand
// follow-on to its TRUE cause we reconstruct that same AST here. This duplicates
// ~40 lines of engine-local, `pub(crate)` logic rather than widening the engine
// interface — the identical clean-room pattern [`crate::shared_residual`] uses
// to re-derive `collect_masters`. The off-grid `#REF!` guard is kept
// **bit-identical to the engine's interim contract** (OXP-210 divergence and
// all) so the attribution reflects what the engine actually computed, never a
// second, subtly-different translation.

/// Shift one A1 [`Axis`] by `delta`, returning the new 1-based axis, or `None`
/// if it moves off the grid (`< 1` or `> max_1based`). Absolute axes never move.
/// Mirrors `xl-engine/src/shared.rs::shift_axis`.
#[must_use]
fn shift_axis(axis: Axis, delta: i64, max_1based: u32) -> Option<Axis> {
    if axis.absolute {
        return Some(axis);
    }
    let shifted = i64::from(axis.index) + delta;
    if shifted < 1 || shifted > i64::from(max_1based) {
        return None; // off-grid → the caller emits `#REF!`
    }
    Some(Axis {
        index: shifted as u32,
        absolute: false,
    })
}

/// Translate one reference node by `(drow, dcol)`, reusing `span`. An off-grid
/// shift on either axis yields `#REF!` ([`ErrorKind::Ref`]); the sheet qualifier
/// (cross-sheet or external) is carried through unchanged. Mirrors
/// `xl-engine/src/shared.rs::translate_ref`.
#[must_use]
fn translate_ref(r: &Reference, span: xl_ast::Span, drow: i64, dcol: i64) -> Expr {
    let shifted_kind: Option<RefKind> = match r.kind {
        RefKind::Cell(col_axis, row_axis) => match (
            shift_axis(col_axis, dcol, MAX_COL_1BASED),
            shift_axis(row_axis, drow, MAX_ROW_1BASED),
        ) {
            (Some(c), Some(rw)) => Some(RefKind::Cell(c, rw)),
            _ => None,
        },
        RefKind::Col(axis) => shift_axis(axis, dcol, MAX_COL_1BASED).map(RefKind::Col),
        RefKind::Row(axis) => shift_axis(axis, drow, MAX_ROW_1BASED).map(RefKind::Row),
        // R1C1 is position-independent; carry it through verbatim (defensive —
        // OOXML stores shared formulas in A1).
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

/// Deep-clone `expr`, shifting every relative A1 axis by `(drow, dcol)` and
/// leaving absolute axes, sheet qualifiers, R1C1 refs, defined names, and
/// literals untouched. Mirrors `xl-engine/src/shared.rs::translate` exactly.
#[must_use]
pub(crate) fn translate(expr: &Expr, drow: i64, dcol: i64) -> Expr {
    let kind = match &expr.kind {
        ExprKind::Ref(r) => return translate_ref(r, expr.span, drow, dcol),
        ExprKind::Name(_) => expr.kind.clone(),
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
        ExprKind::Number(_)
        | ExprKind::Text(_)
        | ExprKind::Bool(_)
        | ExprKind::Error(_)
        | ExprKind::Missing
        | ExprKind::Unsupported { .. } => expr.kind.clone(),
    };
    Expr::new(kind, expr.span)
}

/// A shared-formula group **master** resolved for decline attribution: its
/// **0-based** grid origin and its parsed body (`None` when the body fails
/// [`xl_ast::parse`], i.e. the group genuinely cannot be expanded). Built by
/// [`attribute_workbook`] (and by tests), keyed `(sheet, si)`; a follow-on's
/// effective formula is [`translate`]d from `expr` by its offset from
/// `(row, col)`.
#[derive(Clone, Debug)]
pub struct SharedMaster {
    /// The master cell's 0-based row.
    pub row: u32,
    /// The master cell's 0-based column.
    pub col: u32,
    /// The master's parsed body, or `None` if it does not parse.
    pub expr: Option<Expr>,
}

/// Walk `expr` accumulating the cell's own direct causes, the names of any
/// unimplemented functions it calls directly, and whether it contains a parsed
/// structured/table-reference construct (`Table1[Col]`).
fn scan_direct(
    expr: &Expr,
    defined_names_uc: &BTreeSet<String>,
    causes: &mut CauseSet,
    unimpl_names: &mut BTreeSet<String>,
    structured_ref: &mut bool,
) {
    match &expr.kind {
        ExprKind::Call { name, args } => {
            match fn_category(&name.canonical, defined_names_uc) {
                FnCategory::VbaUdf => causes.vba_udf = true,
                FnCategory::BlockedIo => causes.blocked_io = true,
                FnCategory::Volatile => causes.volatile = true,
                FnCategory::Unimplemented => {
                    causes.unimplemented_fn = true;
                    unimpl_names.insert(name.canonical.clone());
                }
                FnCategory::Supported => {}
            }
            for a in args {
                scan_direct(a, defined_names_uc, causes, unimpl_names, structured_ref);
            }
        }
        ExprKind::Ref(r) => {
            if sheet_is_external(&r.sheet) {
                causes.external = true;
            }
        }
        ExprKind::Name(n) => {
            if sheet_is_external(&n.sheet) {
                causes.external = true;
            }
        }
        ExprKind::Unsupported { .. } => {
            // A parsed `Brackets`-derived Unsupported node is a structured/table
            // reference construct (`Table1[Col]`, `Sheet1!T[[#Data],[Col]]`) — a
            // genuine unquoted external ref never survives parsing (its leading
            // `[1]` becomes a trailing-token parse error and takes the
            // unparsed-fallback path). It is neither an external ref nor an
            // unimplemented *function*: it declines as a separately-unsupported
            // construct, attributed to other_unattributed (under
            // other_structured_ref), never inflating the unimplemented-fn tally.
            *structured_ref = true;
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::Postfix { expr, .. }
        | ExprKind::Paren(expr)
        | ExprKind::ImplicitIntersection(expr) => {
            scan_direct(expr, defined_names_uc, causes, unimpl_names, structured_ref);
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            scan_direct(lhs, defined_names_uc, causes, unimpl_names, structured_ref);
            scan_direct(rhs, defined_names_uc, causes, unimpl_names, structured_ref);
        }
        ExprKind::Array(rows) => {
            for row in rows {
                for e in row {
                    scan_direct(e, defined_names_uc, causes, unimpl_names, structured_ref);
                }
            }
        }
        ExprKind::Number(_)
        | ExprKind::Text(_)
        | ExprKind::Bool(_)
        | ExprKind::Error(_)
        | ExprKind::Missing => {}
    }
}

/// A resolved precedent: one cell, or a rectangle on one sheet.
enum PrecTarget {
    Cell(CellId),
    Rect {
        sheet: SheetId,
        r0: u32,
        r1: u32,
        c0: u32,
        c1: u32,
    },
}

/// Resolve a sheet qualifier to an in-workbook [`SheetId`]. Returns `None` for
/// external / 3-D / workbook-global / unknown sheets (those are not ordinary
/// in-workbook precedents; external is attributed separately as its own cause).
#[must_use]
fn resolve_sheet(
    sheet: &Option<SheetRef>,
    cur: SheetId,
    sheet_map: &BTreeMap<String, SheetId>,
) -> Option<SheetId> {
    match sheet {
        None => Some(cur),
        Some(sr) => {
            if sr.workbook_global || sr.last.is_some() || sr.first.starts_with('[') {
                None
            } else {
                sheet_map.get(&sr.first.to_ascii_lowercase()).copied()
            }
        }
    }
}

/// Resolve a single `Reference` to a precedent target (a cell or a
/// whole-column/whole-row rectangle). R1C1 and unresolvable sheets yield `None`.
#[must_use]
fn resolve_ref(
    r: &Reference,
    cur: SheetId,
    sheet_map: &BTreeMap<String, SheetId>,
) -> Option<PrecTarget> {
    let sheet = resolve_sheet(&r.sheet, cur, sheet_map)?;
    match r.kind {
        xl_ast::RefKind::Cell(col, row) => Some(PrecTarget::Cell(CellId::new(
            sheet,
            row.index.saturating_sub(1),
            col.index.saturating_sub(1),
        ))),
        xl_ast::RefKind::Col(col) => {
            let c = col.index.saturating_sub(1);
            Some(PrecTarget::Rect {
                sheet,
                r0: 0,
                r1: MAX_ROW0,
                c0: c,
                c1: c,
            })
        }
        xl_ast::RefKind::Row(row) => {
            let rr = row.index.saturating_sub(1);
            Some(PrecTarget::Rect {
                sheet,
                r0: rr,
                r1: rr,
                c0: 0,
                c1: MAX_COL0,
            })
        }
        xl_ast::RefKind::R1C1(_) => None,
    }
}

/// The `(col, row)` (0-based, either axis absent for a whole column/row) of a
/// range endpoint, plus its sheet qualifier.
struct Endpoint<'a> {
    sheet: &'a Option<SheetRef>,
    col: Option<u32>,
    row: Option<u32>,
}

/// Extract a range endpoint from an expression (unwrapping grouping parens).
#[must_use]
fn ref_endpoint(e: &Expr) -> Option<Endpoint<'_>> {
    let inner = match &e.kind {
        ExprKind::Paren(x) => x,
        _ => e,
    };
    match &inner.kind {
        ExprKind::Ref(r) => {
            let (col, row) = match r.kind {
                xl_ast::RefKind::Cell(c, rw) => (
                    Some(c.index.saturating_sub(1)),
                    Some(rw.index.saturating_sub(1)),
                ),
                xl_ast::RefKind::Col(c) => (Some(c.index.saturating_sub(1)), None),
                xl_ast::RefKind::Row(rw) => (None, Some(rw.index.saturating_sub(1))),
                xl_ast::RefKind::R1C1(_) => return None,
            };
            Some(Endpoint {
                sheet: &r.sheet,
                col,
                row,
            })
        }
        _ => None,
    }
}

/// Resolve `lhs:rhs` (the range operator) to a rectangle precedent.
#[must_use]
fn resolve_range(
    lhs: &Expr,
    rhs: &Expr,
    cur: SheetId,
    sheet_map: &BTreeMap<String, SheetId>,
) -> Option<PrecTarget> {
    let le = ref_endpoint(lhs)?;
    let re = ref_endpoint(rhs)?;
    // The rectangle lives on the left endpoint's sheet (a redundant same-sheet
    // right qualifier is normalized away by the lexer; a genuine cross-sheet
    // range is out of scope and falls back to per-endpoint resolution upstream).
    let sheet = resolve_sheet(le.sheet, cur, sheet_map)?;
    let (c0, c1) = axis_bounds(le.col, re.col, 0, MAX_COL0);
    let (r0, r1) = axis_bounds(le.row, re.row, 0, MAX_ROW0);
    Some(PrecTarget::Rect {
        sheet,
        r0,
        r1,
        c0,
        c1,
    })
}

/// Combine two endpoint axis values into an inclusive `[lo, hi]` band. When both
/// are present it is their min/max; otherwise the full grid extent on that axis
/// (a whole-column/-row endpoint) — over-inclusive, which is safe because the
/// band is only ever intersected against the finite declined-cell set.
#[must_use]
fn axis_bounds(a: Option<u32>, b: Option<u32>, full_lo: u32, full_hi: u32) -> (u32, u32) {
    match (a, b) {
        (Some(x), Some(y)) => (x.min(y), x.max(y)),
        _ => (full_lo, full_hi),
    }
}

/// Walk `expr` collecting every precedent target (cells + ranges).
fn walk_precedents(
    expr: &Expr,
    cur: SheetId,
    sheet_map: &BTreeMap<String, SheetId>,
    out: &mut Vec<PrecTarget>,
) {
    match &expr.kind {
        ExprKind::Ref(r) => {
            if let Some(t) = resolve_ref(r, cur, sheet_map) {
                out.push(t);
            }
        }
        ExprKind::Binary {
            op: xl_ast::BinaryOp::Range,
            lhs,
            rhs,
        } => {
            if let Some(t) = resolve_range(lhs, rhs, cur, sheet_map) {
                out.push(t);
            } else {
                walk_precedents(lhs, cur, sheet_map, out);
                walk_precedents(rhs, cur, sheet_map, out);
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            walk_precedents(lhs, cur, sheet_map, out);
            walk_precedents(rhs, cur, sheet_map, out);
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::Postfix { expr, .. }
        | ExprKind::Paren(expr)
        | ExprKind::ImplicitIntersection(expr) => {
            walk_precedents(expr, cur, sheet_map, out);
        }
        ExprKind::Call { args, .. } => {
            for a in args {
                walk_precedents(a, cur, sheet_map, out);
            }
        }
        ExprKind::Array(rows) => {
            for row in rows {
                for e in row {
                    walk_precedents(e, cur, sheet_map, out);
                }
            }
        }
        // A defined `Name` may resolve to a range, but resolving it needs the
        // name's own formula; deferred (its cascade, if any, lands in
        // `other_unattributed` — explicit, never guessed).
        ExprKind::Name(_)
        | ExprKind::Number(_)
        | ExprKind::Text(_)
        | ExprKind::Bool(_)
        | ExprKind::Error(_)
        | ExprKind::Missing
        | ExprKind::Unsupported { .. } => {}
    }
}

/// Resolve a formula's precedents down to the subset that are themselves
/// declined cells (the only precedents a cascade can flow through).
#[must_use]
pub(crate) fn resolve_declined_precedents(
    expr: &Expr,
    cur: SheetId,
    sheet_map: &BTreeMap<String, SheetId>,
    declined_index: &BTreeMap<SheetId, BTreeSet<(u32, u32)>>,
) -> Vec<CellId> {
    let mut targets = Vec::new();
    walk_precedents(expr, cur, sheet_map, &mut targets);
    let mut found: BTreeSet<CellId> = BTreeSet::new();
    for t in targets {
        match t {
            PrecTarget::Cell(cid) => {
                if declined_index
                    .get(&cid.sheet)
                    .is_some_and(|s| s.contains(&(cid.row, cid.col)))
                {
                    found.insert(cid);
                }
            }
            PrecTarget::Rect {
                sheet,
                r0,
                r1,
                c0,
                c1,
            } => {
                if let Some(cells) = declined_index.get(&sheet) {
                    for &(row, col) in cells.range((r0, 0)..=(r1, u32::MAX)) {
                        if col >= c0 && col <= c1 {
                            found.insert(CellId::new(sheet, row, col));
                        }
                    }
                }
            }
        }
    }
    found.into_iter().collect()
}

/// One formula cell's identity, text, and declined status — the pure input to
/// [`attribute_cells`]. `declined_kind` is `Some` iff the cell's recalculated
/// value is a recalc sentinel.
#[derive(Clone, Debug)]
pub struct CellInfo {
    pub sheet: SheetId,
    pub row: u32,
    pub col: u32,
    /// Formula text, A1-style with a leading `=` (or the bodyless placeholder).
    pub formula: String,
    pub declined_kind: Option<ErrorKind>,
    /// The cell's OWN `<f>` kind when it is a **bodyless** follow-on (its
    /// `text` was `None`); `None` for a cell that carries its own formula body.
    /// Drives the shared/array follow-on split (see
    /// [`DeclineClass::SharedFollowonUnexpanded`] /
    /// [`DeclineClass::ArrayFollowonUnmaterialized`]).
    pub bodyless_kind: Option<FormulaKind>,
    /// The cell's `si` (shared-group index) when it is a bodyless
    /// [`FormulaKind::Shared`] follow-on — the key into the workbook's
    /// `(sheet, si)` master map used to resolve+translate its real formula. A
    /// missing `si` (or a missing / unparseable master) is what makes a shared
    /// follow-on genuinely `SharedFollowonUnexpanded`; a present, parseable
    /// master re-homes the follow-on to its TRUE cause. `None` for any
    /// non-shared / body-carrying cell.
    pub shared_index: Option<u32>,
}

/// Per-declined-cell precomputed attribution facts.
struct Prep {
    own_set: CauseSet,
    direct_pick: Option<Cause>,
    unimpl_names: BTreeSet<String>,
    precs: Vec<CellId>,
    parsed_ok: bool,
    bodyless: bool,
    /// The cell's own `<f>` kind when bodyless — drives the array follow-on
    /// class (shared follow-ons are now split by master-parseability, below).
    bodyless_kind: Option<FormulaKind>,
    /// The cell's own formula contains a parsed structured/table reference.
    has_structured_ref: bool,
    /// A bodyless shared follow-on whose group master **parses**: its effective
    /// AST is the translated master, so its causes are scanned/traced from that
    /// AST and it is attributed to its TRUE cause (never
    /// `SharedFollowonUnexpanded`).
    shared_would_expand: bool,
    /// A bodyless shared follow-on that **cannot** be expanded — no `si`, no
    /// master cell for `(sheet, si)`, or a master whose body fails to parse.
    /// This — and only this — is the genuine `SharedFollowonUnexpanded` residual.
    shared_unexpandable: bool,
}

/// The decline attribution of one workbook. Per-class counts sum to
/// [`WorkbookDeclineResult::total_declined`].
#[derive(Clone, Debug, Default)]
pub struct WorkbookDeclineResult {
    pub total_declined: usize,
    /// Counts indexed by [`DeclineClass::idx`].
    pub per_class: [usize; 10],
    /// Declined cells (direct + cascade) implicated by each unimplemented
    /// function name (a cell naming several counts under each).
    pub unimplemented_fn_cells: BTreeMap<String, usize>,
    /// `other_unattributed` breakdown: a bodyless **normal** follow-on (a
    /// shared/array/data-table follow-on is its own class now, not counted here).
    pub other_bodyless: usize,
    /// `other_unattributed` breakdown: a parsed structured/table reference
    /// construct (`Table1[Col]`) with no other cause.
    pub other_structured_ref: usize,
    /// `other_unattributed` breakdown: formulas that did not parse.
    pub other_parse_error: usize,
    /// `other_unattributed` breakdown: parsed, but no cause and no traceable
    /// cascade root (a reference-form construct, `#REF!`-rooted, or unclear).
    pub other_construct_or_unclear: usize,
    /// `other_unattributed` breakdown: a **would-expand** shared follow-on
    /// (master parses; expanded cleanly) that still declines with no statically
    /// attributable direct or cascade cause — i.e. a *runtime* refusal (e.g. the
    /// OXP-189 locale-collation `#UNSUPPORTED!`). These are NOT a shared-formula
    /// expansion gap; they are explicitly parked here, not in
    /// `shared_followon_unexpanded`.
    pub other_shared_expanded: usize,
    /// Declined cells whose OWN [`CauseSet`] carried **≥2** distinct causes
    /// before the tiebreak picked a single winner. Bounds how much the
    /// `external > unimplemented_fn` tiebreak could shift the external share.
    pub multi_cause_cells: usize,
    /// The `other_shared_expanded` sub-bucket, cell by cell, in `CellId` order:
    /// every would-expand shared follow-on counted in
    /// [`other_shared_expanded`](Self::other_shared_expanded). Consumed by the
    /// L2 refusal-site decomposition ([`crate::l2site`]) so the two tools
    /// reconcile on the identical cell set by construction.
    pub shared_expanded_cells: Vec<CellId>,
    /// Per-declined-cell classification, in `CellId` order (for tests/debug).
    pub classified: Vec<(CellId, DeclineClass)>,
    /// Sheet display names indexed by [`SheetId`] (`sheet_names[sid.0]`), so a
    /// consumer of [`classified`] can render each [`CellId`]'s workbook-relative
    /// coordinate as `sheet!A1`. Populated by [`attribute_workbook`] (which has
    /// the workbook in scope); left empty by [`attribute_cells`] alone, whose
    /// SheetId-only inputs carry no names (the unit tests exercise that path and
    /// do not need names). Never affects any count — pure output metadata.
    pub sheet_names: Vec<String>,
}

impl WorkbookDeclineResult {
    /// Count for one class.
    #[must_use]
    pub fn count(&self, class: DeclineClass) -> usize {
        self.per_class[class.idx()]
    }
}

/// Attribute one workbook's declined cells (pure — the engine-free core).
///
/// `sheet_map` maps ASCII-lowercased sheet display names to their [`SheetId`];
/// `defined_names_uc` is the set of ASCII-uppercased workbook defined names;
/// `shared_masters` maps `(sheet, si)` to the resolved [`SharedMaster`] for each
/// shared-formula group. A bodyless shared follow-on with a **parseable** master
/// is re-expanded (its master [`translate`]d to the follow-on position) and
/// attributed to its TRUE cause via the same scan/trace path as any other cell;
/// only a follow-on with no `si`, a missing master, or an **unparseable** master
/// is the genuine [`DeclineClass::SharedFollowonUnexpanded`] residual.
#[must_use]
pub fn attribute_cells(
    cells: &[CellInfo],
    sheet_map: &BTreeMap<String, SheetId>,
    defined_names_uc: &BTreeSet<String>,
    shared_masters: &BTreeMap<(SheetId, u32), SharedMaster>,
) -> WorkbookDeclineResult {
    // Declined cells, in CellId order, with kind + text + bodyless kind + si.
    #[derive(Clone, Copy)]
    struct DeclinedCell<'a> {
        formula: &'a str,
        kind: ErrorKind,
        bodyless_kind: Option<FormulaKind>,
        shared_index: Option<u32>,
    }
    let mut declined: BTreeMap<CellId, DeclinedCell<'_>> = BTreeMap::new();
    let mut declined_index: BTreeMap<SheetId, BTreeSet<(u32, u32)>> = BTreeMap::new();
    for c in cells {
        if let Some(kind) = c.declined_kind {
            let cid = CellId::new(c.sheet, c.row, c.col);
            declined.insert(
                cid,
                DeclinedCell {
                    formula: c.formula.as_str(),
                    kind,
                    bodyless_kind: c.bodyless_kind,
                    shared_index: c.shared_index,
                },
            );
            declined_index
                .entry(c.sheet)
                .or_default()
                .insert((c.row, c.col));
        }
    }

    // Phase A: precompute each declined cell's direct causes + declined
    // precedents, working from its EFFECTIVE AST — for a bodyless shared
    // follow-on with a parseable master that is the master translated to this
    // cell's position (what the engine actually expanded and evaluated).
    let mut prep: BTreeMap<CellId, Prep> = BTreeMap::new();
    for (&cid, dc) in &declined {
        let bodyless = dc.formula == BODYLESS_PLACEHOLDER;
        let is_shared_followon = bodyless && dc.bodyless_kind == Some(FormulaKind::Shared);

        let mut shared_would_expand = false;
        let mut shared_unexpandable = false;
        let ast: Option<Expr> = if is_shared_followon {
            // Resolve the group master for (sheet, si) and translate it here.
            match dc
                .shared_index
                .and_then(|si| shared_masters.get(&(cid.sheet, si)))
            {
                Some(master) => match &master.expr {
                    Some(master_expr) => {
                        let drow = i64::from(cid.row) - i64::from(master.row);
                        let dcol = i64::from(cid.col) - i64::from(master.col);
                        shared_would_expand = true;
                        Some(translate(master_expr, drow, dcol))
                    }
                    // Master present but its body does not parse — the genuine,
                    // ~97-cell shared-residual (OXP-211 non-ASCII names).
                    None => {
                        shared_unexpandable = true;
                        None
                    }
                },
                // No `si`, or no master cell for (sheet, si) — orphan/malformed;
                // the group cannot be expanded.
                None => {
                    shared_unexpandable = true;
                    None
                }
            }
        } else {
            parse(dc.formula).ok()
        };

        let mut own_set = CauseSet::default();
        let mut unimpl_names = BTreeSet::new();
        let mut has_structured_ref = false;
        if dc.kind == ErrorKind::Blocked {
            own_set.blocked_io = true;
        }
        match &ast {
            Some(expr) => scan_direct(
                expr,
                defined_names_uc,
                &mut own_set,
                &mut unimpl_names,
                &mut has_structured_ref,
            ),
            None => {
                // Unparseable: the only signal we can still read is a raw-text
                // external index (the unquoted `[1]…` form). Never fires for the
                // bodyless placeholder (no bracket token there).
                if has_external_bracket(dc.formula) {
                    own_set.external = true;
                }
            }
        }
        let precs = ast
            .as_ref()
            .map(|e| resolve_declined_precedents(e, cid.sheet, sheet_map, &declined_index))
            .unwrap_or_default();

        prep.insert(
            cid,
            Prep {
                own_set,
                direct_pick: own_set.pick(),
                unimpl_names,
                precs,
                parsed_ok: ast.is_some(),
                bodyless,
                bodyless_kind: dc.bodyless_kind,
                has_structured_ref,
                shared_would_expand,
                shared_unexpandable,
            },
        );
    }

    // Phase B: classify each declined cell.
    let mut result = WorkbookDeclineResult {
        total_declined: declined.len(),
        ..Default::default()
    };
    for (&cid, p) in &prep {
        // Ordering: a genuine own direct cause wins; else a cascade cause; else
        // the bodyless follow-on split (keyed on the cell's own `<f>` kind); else
        // other_unattributed. A bodyless cell has no formula body, so it can
        // never carry an own direct cause — but the ordering is kept explicit so
        // a direct/cascade cause is always attributed ahead of the kind split.
        let (class, tally_names): (DeclineClass, BTreeSet<String>) = match p.direct_pick {
            Some(cause) => (direct_class(cause), p.unimpl_names.clone()),
            None => {
                let (cause, names) = trace_cascade(cid, &prep);
                match cause {
                    Some(Cause::External) => (DeclineClass::ExternalCascade, BTreeSet::new()),
                    Some(Cause::UnimplementedFn) => (DeclineClass::UnimplementedFnCascade, names),
                    Some(Cause::Volatile) => (DeclineClass::VolatileAntitarget, BTreeSet::new()),
                    Some(Cause::BlockedIo) => (DeclineClass::BlockedIo, BTreeSet::new()),
                    Some(Cause::VbaUdf) => (DeclineClass::VbaUdfAddin, BTreeSet::new()),
                    // No direct cause and no traceable cascade. A shared
                    // follow-on lands in SharedFollowonUnexpanded ONLY when it
                    // genuinely could not expand (missing / unparseable master /
                    // no si); a would-expand follow-on with no static cause has
                    // been re-expanded and is a runtime refusal → other. Array /
                    // data-table follow-ons are unexpanded by design.
                    None => {
                        if p.shared_unexpandable {
                            (DeclineClass::SharedFollowonUnexpanded, BTreeSet::new())
                        } else if matches!(
                            p.bodyless_kind,
                            Some(FormulaKind::Array | FormulaKind::DataTable)
                        ) {
                            (DeclineClass::ArrayFollowonUnmaterialized, BTreeSet::new())
                        } else {
                            (DeclineClass::OtherUnattributed, BTreeSet::new())
                        }
                    }
                }
            }
        };

        // R7: count multi-cause cells (own CauseSet had ≥2 causes before the
        // tiebreak), regardless of which class ultimately won.
        if p.own_set.distinct_count() >= 2 {
            result.multi_cause_cells += 1;
        }

        result.per_class[class.idx()] += 1;
        if matches!(
            class,
            DeclineClass::UnimplementedFnDirect | DeclineClass::UnimplementedFnCascade
        ) {
            for n in &tally_names {
                *result.unimplemented_fn_cells.entry(n.clone()).or_default() += 1;
            }
        }
        if class == DeclineClass::OtherUnattributed {
            if p.has_structured_ref {
                result.other_structured_ref += 1;
            } else if p.shared_would_expand {
                // A shared follow-on that expanded cleanly (master parsed) yet
                // still declines — a runtime refusal, NOT a shared-expansion gap.
                result.other_shared_expanded += 1;
                result.shared_expanded_cells.push(cid);
            } else if p.bodyless {
                result.other_bodyless += 1;
            } else if !p.parsed_ok {
                result.other_parse_error += 1;
            } else {
                result.other_construct_or_unclear += 1;
            }
        }
        result.classified.push((cid, class));
    }

    // Mutual-exclusivity + completeness invariant: exactly one class per
    // declined cell, so the ten counts sum to the declined total.
    debug_assert_eq!(
        result.per_class.iter().sum::<usize>(),
        result.total_declined,
        "decline classes must partition the declined cells"
    );
    result
}

/// Map a direct cause to its `*_direct` (or single) class.
#[must_use]
fn direct_class(cause: Cause) -> DeclineClass {
    match cause {
        Cause::External => DeclineClass::ExternalDirect,
        Cause::UnimplementedFn => DeclineClass::UnimplementedFnDirect,
        Cause::Volatile => DeclineClass::VolatileAntitarget,
        Cause::BlockedIo => DeclineClass::BlockedIo,
        Cause::VbaUdf => DeclineClass::VbaUdfAddin,
    }
}

/// Trace a cascade: BFS over declined precedents, unioning every reachable
/// declined cell's direct causes, and picking the winner. Also returns the
/// unimplemented-function names contributed by reachable roots whose own direct
/// cause is `UnimplementedFn` (for the tally). Cycle-safe via a visited set.
#[must_use]
fn trace_cascade(
    start: CellId,
    prep: &BTreeMap<CellId, Prep>,
) -> (Option<Cause>, BTreeSet<String>) {
    let mut set = CauseSet::default();
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut seen: BTreeSet<CellId> = BTreeSet::new();
    let mut stack: Vec<CellId> = prep
        .get(&start)
        .map(|p| p.precs.clone())
        .unwrap_or_default();
    while let Some(c) = stack.pop() {
        if !seen.insert(c) {
            continue;
        }
        if let Some(p) = prep.get(&c) {
            set = set.or(p.own_set);
            if p.direct_pick == Some(Cause::UnimplementedFn) {
                names.extend(p.unimpl_names.iter().cloned());
            }
            for &q in &p.precs {
                if !seen.contains(&q) {
                    stack.push(q);
                }
            }
        }
    }
    (set.pick(), names)
}

/// Load, recalc, and attribute the declined cells of one workbook.
///
/// Mirrors [`crate::report::run_workbook`]'s load/snapshot/recalc/classify
/// pipeline exactly — same `CachedValueSource` oracle, same [`classify`] funnel
/// — so a cell counts as *declined* iff the funnel scores it
/// [`CellStatus::EngineUnsupported`]. `NoOracle` wins first there, so a
/// sentinel-valued cell with **no** oracle value is not declined; a computed
/// engine-internal lambda *is*. This is what makes the tool's `total_declined`
/// equal the funnel's `EngineUnsupported` count (R1 funnel reconciliation).
///
/// The raw post-recalc engine value is still consulted so `declined_kind`
/// carries the real sentinel [`ErrorKind`] when the engine produced one (the
/// blocked-by-value branch in [`attribute_cells`] depends on it); a non-sentinel
/// `EngineUnsupported` (a computed lambda) is recorded as the generic
/// [`ErrorKind::Unsupported`].
pub fn attribute_workbook(path: &std::path::Path) -> Result<WorkbookDeclineResult, RunError> {
    let workbook = xl_io::open(path).map_err(RunError::Load)?;

    let defined_names_uc: BTreeSet<String> = workbook
        .defined_names
        .iter()
        .map(|d| d.name.to_ascii_uppercase())
        .collect();

    // Snapshot every formula cell (sheet id + name, coords, text, own `<f>`
    // kind) and each cell's cached oracle value, before `Engine::load` consumes
    // the workbook and `recalc()` overwrites the cached values. The oracle map
    // is keyed by sheet display name exactly as `report::run_workbook` keys it,
    // so the same `CachedValueSource` decides `NoOracle`.
    let mut sheet_map: BTreeMap<String, SheetId> = BTreeMap::new();
    struct Snap {
        sheet: SheetId,
        name: String,
        row: u32,
        col: u32,
        formula: String,
        bodyless_kind: Option<FormulaKind>,
        shared_index: Option<u32>,
    }
    let mut snaps: Vec<Snap> = Vec::new();
    let mut cached_map: BTreeMap<(String, u32, u32), Value> = BTreeMap::new();
    // Shared-formula group masters, keyed `(sheet, si)`. Built here (not by the
    // engine, whose `collect_masters` is `pub(crate)`) by the same clean-room
    // rule the engine uses: a master is a `t="shared"` cell carrying BOTH a body
    // and an `si`; `sheet.cells` iterates in `(row, col)` order so a later insert
    // wins, exactly as `xl-engine`'s `BTreeMap::insert`. The body is parsed once;
    // an unparseable master is stored with `expr = None` so its follow-ons stay
    // the genuine `SharedFollowonUnexpanded` residual.
    let mut shared_masters: BTreeMap<(SheetId, u32), SharedMaster> = BTreeMap::new();
    for (idx, sheet) in workbook.sheets.iter().enumerate() {
        let sid = SheetId(idx as u32);
        sheet_map.insert(sheet.name.to_ascii_lowercase(), sid);
        for (&(row, col), cell) in &sheet.cells {
            let Some(raw) = &cell.formula else { continue };
            let is_shared = raw.kind == FormulaKind::Shared;
            let (formula, bodyless_kind, shared_index) = match &raw.text {
                Some(text) => {
                    // A shared cell carrying a body + si is a group MASTER.
                    if is_shared && let Some(si) = raw.shared_index {
                        shared_masters.insert(
                            (sid, si),
                            SharedMaster {
                                row,
                                col,
                                expr: xl_ast::parse(text).ok(),
                            },
                        );
                    }
                    (format!("={text}"), None, None)
                }
                // Bodyless follow-on: carry its `<f>` kind and (for shared) its
                // `si` so `attribute_cells` can resolve+translate its master.
                None => (
                    BODYLESS_PLACEHOLDER.to_string(),
                    Some(raw.kind),
                    is_shared.then_some(raw.shared_index).flatten(),
                ),
            };
            cached_map.insert((sheet.name.clone(), row, col), cell.value.clone());
            snaps.push(Snap {
                sheet: sid,
                name: sheet.name.clone(),
                row,
                col,
                formula,
                bodyless_kind,
                shared_index,
            });
        }
    }
    let oracle = CachedValueSource::new(cached_map);

    // Sheet display names in SheetId order (`sheet_names[sid.0]`), captured
    // before `Engine::load` consumes the workbook — lets the `--dump-cells`
    // consumer render each classified `CellId` as `sheet!A1`.
    let sheet_names: Vec<String> = workbook.sheets.iter().map(|s| s.name.clone()).collect();

    let mut engine = Engine::load(workbook);
    engine.recalc();

    let cells: Vec<CellInfo> = snaps
        .into_iter()
        .map(|s| {
            let computed = engine
                .value(s.sheet, s.row, s.col)
                .cloned()
                .unwrap_or(Value::Blank);
            let oracle_record = oracle.lookup(&s.name, s.row, s.col);
            let status = classify(&computed, &oracle_record, DiffConfig::default());
            // R1: declined iff the funnel says EngineUnsupported. Carry the raw
            // sentinel kind so blocked-by-value still works; a computed lambda
            // (EngineUnsupported without a sentinel value) is generic Unsupported.
            let declined_kind =
                matches!(status, CellStatus::EngineUnsupported).then(|| match &computed {
                    Value::Error(k) if k.is_recalc_sentinel() => *k,
                    _ => ErrorKind::Unsupported,
                });
            CellInfo {
                sheet: s.sheet,
                row: s.row,
                col: s.col,
                formula: s.formula,
                declined_kind,
                bodyless_kind: s.bodyless_kind,
                shared_index: s.shared_index,
            }
        })
        .collect();

    let mut result = attribute_cells(&cells, &sheet_map, &defined_names_uc, &shared_masters);
    result.sheet_names = sheet_names;
    Ok(result)
}

/// The corpus-wide decline-attribution accumulator. Fold one
/// [`WorkbookDeclineResult`] per workbook, then [`render`](DeclineTally::render).
#[derive(Clone, Debug, Default)]
pub struct DeclineTally {
    pub total_declined: usize,
    pub per_class: [usize; 10],
    pub unimplemented_fn_cells: BTreeMap<String, usize>,
    pub other_bodyless: usize,
    pub other_structured_ref: usize,
    pub other_parse_error: usize,
    pub other_construct_or_unclear: usize,
    pub other_shared_expanded: usize,
    pub multi_cause_cells: usize,
    pub workbooks: usize,
    pub load_failures: usize,
}

impl DeclineTally {
    /// Fold one workbook's result into the running totals.
    pub fn fold(&mut self, r: &WorkbookDeclineResult) {
        self.workbooks += 1;
        self.total_declined += r.total_declined;
        for i in 0..self.per_class.len() {
            self.per_class[i] += r.per_class[i];
        }
        for (name, n) in &r.unimplemented_fn_cells {
            *self.unimplemented_fn_cells.entry(name.clone()).or_default() += n;
        }
        self.other_bodyless += r.other_bodyless;
        self.other_structured_ref += r.other_structured_ref;
        self.other_parse_error += r.other_parse_error;
        self.other_construct_or_unclear += r.other_construct_or_unclear;
        self.other_shared_expanded += r.other_shared_expanded;
        self.multi_cause_cells += r.multi_cause_cells;
    }

    /// Record a workbook that failed to load (excluded from all counts).
    pub fn note_load_failure(&mut self) {
        self.load_failures += 1;
    }

    /// The unimplemented-function tally, ordered by declined-cell count desc,
    /// then name asc.
    #[must_use]
    pub fn top_unimplemented_fns(&self) -> Vec<(String, usize)> {
        let mut v: Vec<(String, usize)> = self
            .unimplemented_fn_cells
            .iter()
            .map(|(k, n)| (k.clone(), *n))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// Render the human-readable summary. `top_n` caps the unimplemented-fn list.
    #[must_use]
    pub fn render(&self, top_n: usize) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(s, "== Decline attribution ==");
        let _ = writeln!(
            s,
            "Every declined cell (#UNSUPPORTED!/#BLOCKED!/#RESOURCE!) traced to exactly ONE"
        );
        let _ = writeln!(
            s,
            "root cause; the ten classes partition the declined total. Tiebreak priority:"
        );
        let _ = writeln!(
            s,
            "external > unimplemented_fn > volatile > blocked_io > vba_udf.\n"
        );
        let _ = writeln!(
            s,
            "workbooks: {} attributed, {} load failure(s) (excluded)",
            self.workbooks, self.load_failures
        );
        let _ = writeln!(s, "total declined cells: {}\n", self.total_declined);

        let pct = |n: usize| -> f64 {
            if self.total_declined == 0 {
                0.0
            } else {
                n as f64 / self.total_declined as f64 * 100.0
            }
        };
        for class in DeclineClass::ALL {
            let n = self.per_class[class.idx()];
            let _ = writeln!(s, "  {:<26} {:>10}  ({:6.2}%)", class.tag(), n, pct(n));
        }
        let sum: usize = self.per_class.iter().sum();
        let _ = writeln!(
            s,
            "  {:<26} {:>10}  ({})",
            "----- sum",
            sum,
            if sum == self.total_declined {
                "OK: equals declined total"
            } else {
                "MISMATCH vs declined total!"
            }
        );

        let _ = writeln!(
            s,
            "\nother_unattributed breakdown: bodyless-followon={}  structured-ref={}  parse-error={}  construct/unclear={}  shared-expanded-runtime={}",
            self.other_bodyless,
            self.other_structured_ref,
            self.other_parse_error,
            self.other_construct_or_unclear,
            self.other_shared_expanded
        );
        let _ = writeln!(
            s,
            "multi-cause declined cells (own CauseSet had >=2 causes before tiebreak): {}",
            self.multi_cause_cells
        );

        let ranking = self.top_unimplemented_fns();
        let unimpl_total = self.per_class[DeclineClass::UnimplementedFnDirect.idx()]
            + self.per_class[DeclineClass::UnimplementedFnCascade.idx()];
        let _ = writeln!(
            s,
            "\n== top unimplemented functions (declined cells naming each; may overlap) — top {} ==",
            top_n
        );
        let _ = writeln!(
            s,
            "   ({} declined cells in unimplemented_fn_direct + _cascade)",
            unimpl_total
        );
        if ranking.is_empty() {
            let _ = writeln!(s, "   (none)");
        }
        for (name, n) in ranking.iter().take(top_n) {
            let _ = writeln!(s, "   {name:<24} {n:>10}");
        }
        s
    }

    /// A single machine-parseable summary line for status-log capture.
    #[must_use]
    pub fn summary_line(&self) -> String {
        let c = |cl: DeclineClass| self.per_class[cl.idx()];
        format!(
            "[DECLINE-ATTR] total={} ext_direct={} ext_cascade={} unimpl_direct={} unimpl_cascade={} volatile={} blocked_io={} vba_udf={} shared_followon={} array_followon={} other={} other_shared_expanded={} multi_cause={} (wbs={}, load_fail={})",
            self.total_declined,
            c(DeclineClass::ExternalDirect),
            c(DeclineClass::ExternalCascade),
            c(DeclineClass::UnimplementedFnDirect),
            c(DeclineClass::UnimplementedFnCascade),
            c(DeclineClass::VolatileAntitarget),
            c(DeclineClass::BlockedIo),
            c(DeclineClass::VbaUdfAddin),
            c(DeclineClass::SharedFollowonUnexpanded),
            c(DeclineClass::ArrayFollowonUnmaterialized),
            c(DeclineClass::OtherUnattributed),
            self.other_shared_expanded,
            self.multi_cause_cells,
            self.workbooks,
            self.load_failures,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet_map() -> BTreeMap<String, SheetId> {
        // Two sheets so an unresolved sheet name is genuinely absent.
        let mut m = BTreeMap::new();
        m.insert("sheet1".to_string(), SheetId(0));
        m.insert("sheet2".to_string(), SheetId(1));
        m
    }

    /// No shared-formula masters (the common case for these fixtures).
    fn no_masters() -> BTreeMap<(SheetId, u32), SharedMaster> {
        BTreeMap::new()
    }

    fn cell(row: u32, col: u32, formula: &str, declined: bool) -> CellInfo {
        CellInfo {
            sheet: SheetId(0),
            row,
            col,
            formula: formula.to_string(),
            declined_kind: declined.then_some(ErrorKind::Unsupported),
            bodyless_kind: None,
            shared_index: None,
        }
    }

    /// A declined **bodyless** follow-on cell of a given own `<f>` kind (its
    /// formula is the bodyless placeholder, no body of its own). No `si`, so a
    /// `Shared` follow-on here is the genuinely-unexpandable (orphan) path.
    fn bodyless_cell(row: u32, col: u32, kind: FormulaKind) -> CellInfo {
        CellInfo {
            sheet: SheetId(0),
            row,
            col,
            formula: BODYLESS_PLACEHOLDER.to_string(),
            declined_kind: Some(ErrorKind::Unsupported),
            bodyless_kind: Some(kind),
            shared_index: None,
        }
    }

    /// A declined bodyless **shared** follow-on carrying an `si` — its master is
    /// resolved from the `shared_masters` map passed to [`attribute_cells`].
    fn shared_followon(row: u32, col: u32, si: u32) -> CellInfo {
        CellInfo {
            sheet: SheetId(0),
            row,
            col,
            formula: BODYLESS_PLACEHOLDER.to_string(),
            declined_kind: Some(ErrorKind::Unsupported),
            bodyless_kind: Some(FormulaKind::Shared),
            shared_index: Some(si),
        }
    }

    /// A shared master resolved at 0-based `(row, col)` with body `text`
    /// (`None` if the body should be treated as unparseable).
    fn master(row: u32, col: u32, text: Option<&str>) -> SharedMaster {
        SharedMaster {
            row,
            col,
            expr: text.and_then(|t| parse(t).ok()),
        }
    }

    fn class_of(r: &WorkbookDeclineResult, row: u32, col: u32) -> DeclineClass {
        let cid = CellId::new(SheetId(0), row, col);
        r.classified
            .iter()
            .find(|(c, _)| *c == cid)
            .map(|(_, cl)| *cl)
            .expect("cell classified")
    }

    #[test]
    fn direct_causes_and_cascades_partition_the_declined_set() {
        // A grid exercising every one of the ten classes exactly once, with both
        // a direct cause and a cascade rooted in it, plus the sum-equals-total
        // invariant.
        let defined: BTreeSet<String> = ["MYUDF".to_string()].into_iter().collect();
        let cells = vec![
            // unimplemented_fn: FOOBAR is unregistered (and not a UDF).
            cell(0, 0, "=FOOBAR(A2)", true), // A1  -> unimplemented_fn_direct
            cell(1, 0, "=A1+1", true),       // A2  -> unimplemented_fn_cascade (reads A1)
            // external: quoted external workbook reference parses to a Ref whose
            // sheet name carries the [1] index.
            cell(0, 1, "='[1]Book'!A1", true), // B1  -> external_direct
            cell(1, 1, "=B1*2", true),         // B2  -> external_cascade (reads B1)
            // volatile anti-target.
            cell(0, 2, "=TODAY()", true), // C1  -> volatile_antitarget
            // blocked I/O by function name.
            cell(1, 2, "=WEBSERVICE(\"http://x\")", true), // C2  -> blocked_io
            // UDF invocation (defined name used as a function).
            cell(2, 2, "=MYUDF(A1)", true), // C3  -> vba_udf_addin
            // reference-form construct with no traceable cause: 3-D reference.
            cell(0, 3, "=SUM(Sheet1:Sheet2!A1)", true), // D1 -> other_unattributed
            // bodyless follow-ons, split on their own `<f>` kind.
            bodyless_cell(0, 4, FormulaKind::Shared), // E1 -> shared_followon_unexpanded
            bodyless_cell(1, 4, FormulaKind::Array),  // E2 -> array_followon_unmaterialized
        ];
        let r = attribute_cells(&cells, &sheet_map(), &defined, &no_masters());

        assert_eq!(r.total_declined, 10);
        assert_eq!(class_of(&r, 0, 0), DeclineClass::UnimplementedFnDirect);
        assert_eq!(class_of(&r, 1, 0), DeclineClass::UnimplementedFnCascade);
        assert_eq!(class_of(&r, 0, 1), DeclineClass::ExternalDirect);
        assert_eq!(class_of(&r, 1, 1), DeclineClass::ExternalCascade);
        assert_eq!(class_of(&r, 0, 2), DeclineClass::VolatileAntitarget);
        assert_eq!(class_of(&r, 1, 2), DeclineClass::BlockedIo);
        assert_eq!(class_of(&r, 2, 2), DeclineClass::VbaUdfAddin);
        assert_eq!(class_of(&r, 0, 3), DeclineClass::OtherUnattributed);
        assert_eq!(class_of(&r, 0, 4), DeclineClass::SharedFollowonUnexpanded);
        assert_eq!(
            class_of(&r, 1, 4),
            DeclineClass::ArrayFollowonUnmaterialized
        );

        // Completeness + mutual exclusivity: the ten counts sum to the total.
        assert_eq!(r.per_class.iter().sum::<usize>(), r.total_declined);
        for class in DeclineClass::ALL {
            assert_eq!(r.count(class), 1, "{} should have one cell", class.tag());
        }

        // The unimplemented-fn tally sees FOOBAR from both the direct cell and
        // the cascade root.
        assert_eq!(r.unimplemented_fn_cells.get("FOOBAR"), Some(&2));
        // D1's 3-D reference is a parsed reference-form construct; the bodyless
        // follow-ons are their own classes, not other_bodyless.
        assert_eq!(r.other_construct_or_unclear, 1);
        assert_eq!(r.other_bodyless, 0);
    }

    // ── Shared-formula follow-on re-attribution (master-parseability) ────────

    #[test]
    fn translate_mirrors_engine_relative_absolute_and_offgrid() {
        // The ported translate must match `xl-engine/src/shared.rs` exactly:
        // relative axes shift, absolute (`$`) axes stay, off-grid → `#REF!`.
        let e = parse("SUM(A1,$B$2,C$3)").unwrap();
        assert_eq!(translate(&e, 1, 0).to_string(), "SUM(A2,$B$2,C$3)");
        assert_eq!(translate(&e, 0, 1).to_string(), "SUM(B1,$B$2,D$3)");
        // Absolute-only never moves.
        assert_eq!(translate(&parse("$A$1").unwrap(), 5, 7).to_string(), "$A$1");
        // Off-grid underflow → the engine's interim `#REF!` node.
        let up = translate(&parse("A1").unwrap(), -1, 0);
        assert!(matches!(up.kind, ExprKind::Error(ErrorKind::Ref)));
    }

    #[test]
    fn shared_followon_with_parseable_external_master_is_external_direct() {
        // Master's body directly references an external workbook; the qualifier
        // survives translation, so every follow-on is external_direct (NOT
        // shared_followon_unexpanded).
        let cells = vec![shared_followon(3, 0, 0)];
        let mut m = no_masters();
        m.insert((SheetId(0), 0), master(0, 0, Some("'[1]Ext'!A1*B1")));
        let r = attribute_cells(&cells, &sheet_map(), &BTreeSet::new(), &m);
        assert_eq!(class_of(&r, 3, 0), DeclineClass::ExternalDirect);
        assert_eq!(r.count(DeclineClass::SharedFollowonUnexpanded), 0);
        assert_eq!(r.per_class.iter().sum::<usize>(), r.total_declined);
    }

    #[test]
    fn shared_followon_with_unimplemented_master_tallies_the_fn() {
        // Master calls an unregistered function; the follow-on inherits it as
        // unimplemented_fn_direct and the fn is tallied.
        let cells = vec![shared_followon(2, 5, 7)];
        let mut m = no_masters();
        m.insert((SheetId(0), 7), master(1, 5, Some("FOOBAR(A1)")));
        let r = attribute_cells(&cells, &sheet_map(), &BTreeSet::new(), &m);
        assert_eq!(class_of(&r, 2, 5), DeclineClass::UnimplementedFnDirect);
        assert_eq!(r.unimplemented_fn_cells.get("FOOBAR"), Some(&1));
        assert_eq!(r.count(DeclineClass::SharedFollowonUnexpanded), 0);
    }

    #[test]
    fn shared_followon_cascades_into_external_via_translated_ref() {
        // Two declined external cells in column A; a shared group in column B
        // whose relative master `=A1` reads column A. The follow-on B2's master
        // TRANSLATES to `=A2`, so it cascades into the (declined) external A2 —
        // proving the translated precedent, not the master's, drives the cascade.
        let cells = vec![
            cell(0, 0, "='[9]X'!A1", true), // A1 external_direct
            cell(1, 0, "='[9]X'!A1", true), // A2 external_direct
            cell(0, 1, "=A1", true),        // B1 master body: reads A1 -> ext cascade
            shared_followon(1, 1, 0),       // B2 follow-on si=0 -> reads A2 -> ext cascade
        ];
        let mut m = no_masters();
        m.insert((SheetId(0), 0), master(0, 1, Some("A1")));
        let r = attribute_cells(&cells, &sheet_map(), &BTreeSet::new(), &m);
        assert_eq!(class_of(&r, 0, 1), DeclineClass::ExternalCascade);
        assert_eq!(class_of(&r, 1, 1), DeclineClass::ExternalCascade);
        assert_eq!(r.count(DeclineClass::SharedFollowonUnexpanded), 0);
        assert_eq!(r.per_class.iter().sum::<usize>(), r.total_declined);
    }

    #[test]
    fn shared_followon_clean_master_no_cause_is_other_shared_expanded() {
        // Master parses and has no static cause; its translated precedent lands
        // on a NON-declined cell. The follow-on expanded cleanly yet still
        // declines (a runtime refusal) -> other_unattributed / shared-expanded,
        // NEVER shared_followon_unexpanded and NOT other_bodyless.
        let cells = vec![shared_followon(1, 1, 0)];
        let mut m = no_masters();
        m.insert((SheetId(0), 0), master(0, 1, Some("A1+1")));
        let r = attribute_cells(&cells, &sheet_map(), &BTreeSet::new(), &m);
        assert_eq!(class_of(&r, 1, 1), DeclineClass::OtherUnattributed);
        assert_eq!(r.other_shared_expanded, 1);
        assert_eq!(r.other_bodyless, 0);
        assert_eq!(r.count(DeclineClass::SharedFollowonUnexpanded), 0);
    }

    #[test]
    fn shared_followon_unparseable_master_stays_unexpanded() {
        // Master present but its body does not parse (expr = None) — the genuine
        // shared residual (the OXP-211 non-ASCII-name case).
        let cells = vec![shared_followon(4, 2, 3)];
        let mut m = no_masters();
        m.insert((SheetId(0), 3), master(0, 2, None)); // unparseable body
        let r = attribute_cells(&cells, &sheet_map(), &BTreeSet::new(), &m);
        assert_eq!(class_of(&r, 4, 2), DeclineClass::SharedFollowonUnexpanded);
        assert_eq!(r.other_shared_expanded, 0);
    }

    #[test]
    fn shared_followon_missing_master_is_unexpanded_orphan() {
        // An `si` with no master cell in the map — an orphan; cannot expand.
        let cells = vec![shared_followon(1, 0, 0)];
        let r = attribute_cells(&cells, &sheet_map(), &BTreeSet::new(), &no_masters());
        assert_eq!(class_of(&r, 1, 0), DeclineClass::SharedFollowonUnexpanded);
    }

    #[test]
    fn cascade_tiebreak_prefers_external_over_unimplemented() {
        // A cell reading two declined roots with different causes takes the
        // higher-priority one (external > unimplemented_fn).
        let cells = vec![
            cell(0, 0, "='[2]Ext'!A1", true), // A1 external_direct
            cell(1, 0, "=BOGUSFN(1)", true),  // A2 unimplemented_fn_direct
            cell(2, 0, "=A1+A2", true),       // A3 reads both -> external_cascade
        ];
        let r = attribute_cells(&cells, &sheet_map(), &BTreeSet::new(), &no_masters());
        assert_eq!(class_of(&r, 2, 0), DeclineClass::ExternalCascade);
        assert_eq!(r.per_class.iter().sum::<usize>(), r.total_declined);
    }

    #[test]
    fn blocked_by_value_and_range_cascade_and_other() {
        // #BLOCKED! result value alone -> blocked_io, even with a neutral
        // formula; a SUM over a range containing a declined root cascades; a
        // bare unparseable / bodyless cell is other_unattributed.
        let mut blocked = cell(0, 0, "=A2", true);
        blocked.declined_kind = Some(ErrorKind::Blocked);
        let cells = vec![
            blocked,                                // A1 -> blocked_io (by value)
            cell(1, 0, "=RUBBISH(9)", true),        // A2 -> unimplemented_fn_direct
            cell(0, 1, "=SUM(A1:A10)", true),       // B1 -> cascade over a range (reads A2)
            cell(0, 2, BODYLESS_PLACEHOLDER, true), // C1 -> other_unattributed (bodyless)
        ];
        let r = attribute_cells(&cells, &sheet_map(), &BTreeSet::new(), &no_masters());
        assert_eq!(class_of(&r, 0, 0), DeclineClass::BlockedIo);
        assert_eq!(class_of(&r, 1, 0), DeclineClass::UnimplementedFnDirect);
        // B1 reads A1 (blocked_io) and A2 (unimplemented) via the range; the
        // tiebreak (unimplemented > blocked_io) picks unimplemented.
        assert_eq!(class_of(&r, 0, 1), DeclineClass::UnimplementedFnCascade);
        assert_eq!(class_of(&r, 0, 2), DeclineClass::OtherUnattributed);
        assert_eq!(r.other_bodyless, 1);
        assert_eq!(r.per_class.iter().sum::<usize>(), r.total_declined);
    }

    #[test]
    fn non_declined_cells_are_never_classified() {
        // A non-declined precedent must not create a cascade edge.
        let cells = vec![
            cell(0, 0, "=1+1", false), // A1 fine
            cell(1, 0, "=A1+1", true), // A2 declined but reads only a fine cell
        ];
        let r = attribute_cells(&cells, &sheet_map(), &BTreeSet::new(), &no_masters());
        assert_eq!(r.total_declined, 1);
        // A2 has no direct cause and no declined precedent -> other_unattributed.
        assert_eq!(class_of(&r, 1, 0), DeclineClass::OtherUnattributed);
        assert_eq!(r.per_class.iter().sum::<usize>(), 1);
    }

    #[test]
    fn external_bracket_scanner() {
        assert!(has_external_bracket("[1]Sheet1!A1"));
        assert!(has_external_bracket("'[12]Book'!$A$1"));
        assert!(!has_external_bracket("Table1[Column]"));
        assert!(!has_external_bracket("[@Amount]"));
        assert!(!has_external_bracket("A1+B2"));
        // R3: an R1C1 offset and a digit-named table column both carry a `[`
        // immediately after an identifier char — not a workbook index.
        assert!(!has_external_bracket("R[1]C[1]"));
        assert!(!has_external_bracket("Table1[2024]"));
        // R3: a `[digits]` inside a string literal is not an external index.
        assert!(!has_external_bracket("=SUM(\"[1]x\")"));
    }

    #[test]
    fn fn_category_routing() {
        let defined: BTreeSet<String> = ["MYUDF".to_string()].into_iter().collect();
        let empty = BTreeSet::new();
        assert_eq!(fn_category("WEBSERVICE", &defined), FnCategory::BlockedIo);
        assert_eq!(fn_category("TODAY", &defined), FnCategory::Volatile);
        assert_eq!(fn_category("_XLL.FOO", &defined), FnCategory::VbaUdf);
        // R5: named-lambda parameter application is supported, never a missing fn.
        assert_eq!(fn_category("_XLPM.X", &empty), FnCategory::Supported);
        assert_eq!(fn_category("MYUDF", &defined), FnCategory::VbaUdf);
        assert_eq!(fn_category("OFFSET", &defined), FnCategory::Supported);
        assert_eq!(fn_category("SUM", &defined), FnCategory::Supported);
        assert_eq!(
            fn_category("NOTAREALFUNC", &defined),
            FnCategory::Unimplemented
        );
    }

    #[test]
    fn special_forms_are_supported_not_unimplemented() {
        // LET/LAMBDA and the higher-order lambda functions are engine special
        // forms (xl-engine/src/eval.rs::eval_special_form), evaluated before the
        // registry — a bare `xl_fn::lookup` misses them, but they are SUPPORTED,
        // so must not be counted as unimplemented functions.
        let d = BTreeSet::new();
        for name in [
            "LET",
            "LAMBDA",
            "MAP",
            "REDUCE",
            "SCAN",
            "BYROW",
            "BYCOL",
            "MAKEARRAY",
        ] {
            assert_eq!(
                fn_category(name, &d),
                FnCategory::Supported,
                "{name} is a supported special form"
            );
        }
        // A cell using a special form that still declines (an unsupported
        // broadcasting/lambda edge) is attributed to other_unattributed, not
        // unimplemented_fn.
        let cells = vec![cell(0, 0, "=_xlfn.LET(_xlpm.x,2,_xlpm.x*3)", true)];
        let r = attribute_cells(&cells, &sheet_map(), &BTreeSet::new(), &no_masters());
        assert_eq!(class_of(&r, 0, 0), DeclineClass::OtherUnattributed);
        assert_eq!(r.count(DeclineClass::UnimplementedFnDirect), 0);
    }

    #[test]
    fn structured_ref_is_other_not_unimplemented_fn() {
        // R2/R6: a parsed table/structured reference (`Table1[Amount]` parses to
        // an `Unsupported` node) is a construct refusal — other_unattributed
        // under other_structured_ref — never an unimplemented function, and never
        // external.
        let cells = vec![cell(0, 0, "=Table1[Amount]", true)];
        let r = attribute_cells(&cells, &sheet_map(), &BTreeSet::new(), &no_masters());
        assert_eq!(class_of(&r, 0, 0), DeclineClass::OtherUnattributed);
        assert_eq!(r.other_structured_ref, 1);
        assert_eq!(r.other_construct_or_unclear, 0);
        assert_eq!(r.count(DeclineClass::UnimplementedFnDirect), 0);
        assert_eq!(r.count(DeclineClass::ExternalDirect), 0);
        assert!(r.unimplemented_fn_cells.is_empty());
        assert_eq!(r.per_class.iter().sum::<usize>(), r.total_declined);
    }

    #[test]
    fn multi_cause_cell_counted_once_and_tiebroken() {
        // R7: a single formula naming two independent causes (blocked I/O + an
        // unimplemented function) is counted once in multi_cause_cells; the
        // tiebreak still resolves to a single winning class (unimplemented_fn >
        // blocked_io).
        let cells = vec![cell(0, 0, "=WEBSERVICE(\"x\")+FOOBAR(1)", true)];
        let r = attribute_cells(&cells, &sheet_map(), &BTreeSet::new(), &no_masters());
        assert_eq!(r.multi_cause_cells, 1);
        assert_eq!(class_of(&r, 0, 0), DeclineClass::UnimplementedFnDirect);
        assert_eq!(r.per_class.iter().sum::<usize>(), r.total_declined);
    }
}
