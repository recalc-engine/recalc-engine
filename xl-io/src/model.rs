//! Public data model: the parsed shape of a workbook.
//!
//! ## Provenance
//! Element/attribute names cited here are from ECMA-376 5th edition Part 1
//! ("Fundamentals and Markup Language Reference"), §18 "SpreadsheetML":
//! `workbook.xml` §18.2.27 (`CT_Workbook`), §18.2.19 (`CT_Sheet`), §18.2.15
//! (`CT_CalcPr`), §18.2.6 (`CT_DefinedNames`); shared strings §18.4
//! (`CT_Sst`/`CT_Rst`/`CT_RElt`); sheet data §18.3.1.4 (`CT_Row`), §18.3.1.4
//! (`CT_Cell`), §18.3.1.40 (`CT_CellFormula`); styles §18.8 (`CT_Stylesheet`,
//! `CT_NumFmts`, `CT_CellXfs`).

use std::collections::{BTreeMap, BTreeSet};

use xl_value::Value;

/// A fully-opened workbook: every sheet's cells plus workbook-level metadata
/// needed by later crates (`xl-graph` for calc settings, `xl-ast` for
/// defined-name formula text).
///
/// Obtained via [`crate::open`] / [`crate::open_with_caps`] /
/// [`crate::from_bytes`] / [`crate::from_bytes_with_caps`].
#[derive(Clone, Debug)]
pub struct Workbook {
    /// Sheets in workbook order (the order of `<sheet>` elements in
    /// `xl/workbook.xml`, i.e. tab order).
    pub sheets: Vec<Sheet>,
    /// 1900 vs 1904 date system (`workbookPr/@date1904`).
    pub date_system: DateSystem,
    /// Calculation settings (`<calcPr>`).
    pub calc_settings: CalcSettings,
    /// Workbook-level and sheet-scoped defined names, raw (unparsed formula
    /// text — parsing is `xl-ast`'s job).
    pub defined_names: Vec<DefinedName>,
    /// Package-level feature flags that don't fit elsewhere.
    pub flags: WorkbookFlags,
}

impl Workbook {
    /// Looks up a sheet by name, **case-insensitively** — Excel sheet names
    /// are case-insensitive-unique within a workbook.
    ///
    /// Folding uses Unicode **simple** (non-expanding, locale-independent)
    /// lowercase — see [`eq_ignore_unicode_case`] — matching Excel's en-US
    /// sheet-name matching as pinned by oracle probe **OXP-061**
    /// (`RUN-2026-07-11-oracle01`, `docs/oracle-experiments.md`): `"ä"`
    /// matches `"Ä"`, but the sharp-s does **not** expand (`"straße"` ≠
    /// `"STRASSE"`) and dotted capital `İ` (U+0130) does **not** fold to
    /// ASCII `"i"`. This is a locale-*independent* fold (not Turkish
    /// casing), so it stays within the "no non-en-US locale semantics" v1
    /// non-goal (`implementation-plan.md` §1) while reproducing every probed
    /// en-US case.
    #[must_use]
    pub fn sheet(&self, name: &str) -> Option<&Sheet> {
        self.sheets
            .iter()
            .find(|s| eq_ignore_unicode_case(&s.name, name))
    }

    /// Looks up a sheet by its zero-based tab-order index.
    #[must_use]
    pub fn sheet_at(&self, index: usize) -> Option<&Sheet> {
        self.sheets.get(index)
    }
}

/// Case-insensitive string equality under Unicode **simple** (non-expanding,
/// locale-independent) lowercase folding.
///
/// Excel's en-US sheet-name matching folds `"ä"`/`"Ä"` together yet does
/// **not** expand the sharp-s (`"straße"` ≠ `"STRASSE"`) nor fold the dotted
/// capital `İ` (U+0130) to ASCII `"i"` (oracle probe OXP-061,
/// `RUN-2026-07-11-oracle01`). Rust's [`char::to_lowercase`] is exactly this
/// locale-independent simple fold — full Unicode default lowercasing that
/// keeps ß as ß and maps İ to `i` + combining dot (which then ≠ plain `i`) —
/// so a per-`char` lowercased comparison reproduces every probed case. It
/// stays allocation-free by comparing the two lazily-lowercased `char`
/// streams element-by-element ([`Iterator::eq`]).
fn eq_ignore_unicode_case(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
}

/// One worksheet's cell data.
#[derive(Clone, Debug)]
pub struct Sheet {
    /// The sheet's display name (`<sheet name="...">`).
    pub name: String,
    /// Excel's own internal sheet id (`<sheet sheetId="...">`) — **not** the
    /// zero-based tab-order index; stable across sheet reordering in the
    /// original file but otherwise opaque.
    pub sheet_id: u32,
    /// This sheet's **0-based position in the workbook's `<sheets>`
    /// collection** (document order), counting every `<sheet>` entry —
    /// including the ones the loader skips (chartsheets, dialogsheets,
    /// macrosheets, and `veryHidden` no-part VBA sheets).
    ///
    /// This is the index space `definedName@localSheetId` scopes against
    /// (ECMA-376 §18.2.6; see [`DefinedName::sheet_scope`]). It equals the
    /// position in [`Workbook::sheets`] only when
    /// [`WorkbookFlags::skipped_sheets`] is zero — a skipped `<sheet>` entry
    /// shifts the two index spaces apart, so scoped-name resolution must key
    /// on this field, never on the loaded-vector position.
    pub sheets_index: u32,
    /// Cell storage, keyed by **0-based, inclusive** `(row, col)` — matching
    /// [`xl_value::RectRange`]'s convention — so `"A1"` is `(0, 0)`.
    ///
    /// A `BTreeMap` is intentionally the v1 storage: simple, correct,
    /// sorted-iteration-friendly, and adequate for the fixture/corpus sizes
    /// this task targets. `implementation-plan.md` §2 specifies a chunked
    /// column-major sparse store for performance; that is a **later**
    /// optimization task, not part of this crate's v1 contract — swapping
    /// the storage is expected to preserve the `Sheet`/`Cell` public shape.
    pub cells: BTreeMap<(u32, u32), Cell>,
    /// The **0-based** indices of rows carrying the OOXML `<row hidden="1">`
    /// attribute (§18.3.1.73 `CT_Row/@hidden`) — rows Excel is not displaying.
    ///
    /// OOXML's single `hidden` bit conflates two distinct causes: a row hidden
    /// *manually* (the Hide Rows command) and a row hidden by an active
    /// *AutoFilter*. The file does not distinguish them, so neither does this
    /// set; it is exactly "the rows Excel considers hidden". Its one consumer is
    /// `SUBTOTAL`'s `101`–`111` `function_num` forms, which exclude hidden rows
    /// (OXP-121, `RUN-2026-07-11-oracle01`) — Excel excludes *both* causes there,
    /// so the conflation is correct for that consumer. A `BTreeSet` mirrors the
    /// sparse, sorted-iteration house style of [`Sheet::cells`]; hidden rows are
    /// a small minority, so it stays cheap.
    pub hidden_rows: BTreeSet<u32>,
}

impl Sheet {
    /// Borrows the cell at 0-based `(row, col)`, or `None` if absent (an
    /// absent cell is a blank cell in Excel terms — the store only holds
    /// cells OOXML actually recorded, and Excel omits fully-blank cells).
    #[must_use]
    pub fn cell(&self, row: u32, col: u32) -> Option<&Cell> {
        self.cells.get(&(row, col))
    }

    /// Whether the given **0-based** row carries `<row hidden="1">` in the
    /// source file (manually hidden or filter-hidden — OOXML does not
    /// distinguish; see [`Sheet::hidden_rows`]).
    #[must_use]
    pub fn is_row_hidden(&self, row: u32) -> bool {
        self.hidden_rows.contains(&row)
    }
}

/// One cell's parsed contents.
#[derive(Clone, Debug)]
pub struct Cell {
    /// The cached value: what Excel last computed/stored for this cell.
    /// Recomputing this from `formula` (when present) is `xl-graph`'s job;
    /// this crate only surfaces what the file already contains.
    pub value: Value,
    /// The raw formula, if this cell holds one. `None` for a plain literal
    /// cell.
    pub formula: Option<RawFormula>,
    /// Number-format reference resolved through the cell's style (`s`
    /// attribute) → `cellXfs` → `numFmtId` chain in `xl/styles.xml`.
    pub num_fmt: NumFmtId,
}

/// A cell's raw, unparsed formula text plus the OOXML formula-sharing
/// metadata. **This crate does not parse formulas** — `xl-ast` owns that;
/// `text` is exactly the XML text content of `<f>`, unescaped.
#[derive(Clone, Debug)]
pub struct RawFormula {
    /// The formula text (without the leading `=`), or `None` for a
    /// **follow-on** shared-formula cell that only carries `<f t="shared"
    /// si="N"/>` with no body — its formula is `si`'s master cell's text,
    /// translated by relative offset (a job for the consumer that resolves
    /// shared-formula groups, not this crate).
    pub text: Option<String>,
    /// What kind of formula this is (`t` attribute on `<f>`).
    pub kind: FormulaKind,
    /// The `si` (shared-group index) attribute, present for
    /// [`FormulaKind::Shared`] cells.
    pub shared_index: Option<u32>,
    /// The `ref` attribute (the master cell's range for shared/array/data-table
    /// formulas), stored as raw A1 text — parsing is `xl-ast`'s job.
    pub range: Option<String>,
}

impl RawFormula {
    /// Whether this formula was **array-entered** — a legacy
    /// `Ctrl+Shift+Enter` CSE array formula (`<f t="array">`, ECMA-376
    /// §18.3.1.40 `ST_CellFormulaType` value `array`).
    ///
    /// Only `t="array"` counts. A `t="shared"` cell is an ordinary formula that
    /// merely shares its text with a group master, **not** array-entered; a
    /// `t="dataTable"` cell is a what-if data table, not a CSE array. Consumers
    /// use this to pick array vs scalar evaluation context: a **non**-array
    /// formula performs Excel's *legacy implicit intersection* on a range that
    /// reaches scalar context, whereas an array-entered one evaluates
    /// element-wise and must **not** be intersected (OXP-004/163,
    /// RUN-2026-07-11-oracle01).
    #[must_use]
    pub fn is_array_entered(&self) -> bool {
        matches!(self.kind, FormulaKind::Array)
    }
}

/// The `t` attribute of `<f>` (ECMA-376 §18.3.1.40 `ST_CellFormulaType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormulaKind {
    /// A plain, ungrouped formula (the default when `t` is absent).
    Normal,
    /// Part of a shared-formula group (`t="shared"`).
    Shared,
    /// An (legacy, `Ctrl+Shift+Enter`) array formula (`t="array"`).
    Array,
    /// A what-if data-table formula (`t="dataTable"`).
    DataTable,
}

/// A resolved number-format reference for a cell.
///
/// This crate resolves the `s` (style index) → `cellXfs` → `numFmtId` chain
/// but does **not** render formats — that is a distinct, later task. Built-in
/// format ids (0-163, ECMA-376 §18.8.30) have no entry in `xl/styles.xml`'s
/// `<numFmts>`, so `format_code` is `None` for them; only explicit custom
/// `<numFmt>` entries populate it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumFmtId {
    /// The numeric format id. `0` ("General") when the cell has no style, the
    /// style index is out of range, or the referenced `cellXfs` entry has no
    /// `numFmtId`.
    pub id: u32,
    /// The custom format code string, when `id` is a custom (explicitly
    /// declared) format. `None` for built-in ids or when `xl/styles.xml` is
    /// absent.
    pub format_code: Option<String>,
}

impl NumFmtId {
    /// The default "General" format with no custom code — used whenever a
    /// cell has no style, or `xl/styles.xml` is absent/doesn't resolve.
    #[must_use]
    pub fn general() -> NumFmtId {
        NumFmtId {
            id: 0,
            format_code: None,
        }
    }
}

/// The 1900/1904 date system (`workbookPr/@date1904`, ECMA-376 §18.2.28).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DateSystem {
    /// Serial day 1 = 1900-01-01 (with the well-known fictitious 1900-02-29
    /// leap day at serial 60). Excel's default on Windows.
    #[default]
    Excel1900,
    /// Serial day 0 = 1904-01-01. Legacy Mac default.
    Excel1904,
}

/// Calculation settings from `<calcPr>` (ECMA-376 §18.2.15 `CT_CalcPr`).
/// Absent `<calcPr>` yields Excel's documented defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct CalcSettings {
    /// `calcMode` attribute.
    pub calc_mode: CalcMode,
    /// `iterate` attribute: whether iterative calculation is enabled.
    pub iterate: bool,
    /// `iterateCount` attribute: maximum iterations.
    pub iterate_count: u32,
    /// `iterateDelta` attribute: maximum change to stop iterating.
    pub iterate_delta: f64,
    /// `fullCalcOnLoad` attribute: whether the file demands a full recalc
    /// before trusting any cached value.
    pub full_calc_on_load: bool,
}

impl Default for CalcSettings {
    fn default() -> CalcSettings {
        CalcSettings {
            calc_mode: CalcMode::Auto,
            iterate: false,
            iterate_count: 100,
            iterate_delta: 0.001,
            full_calc_on_load: false,
        }
    }
}

/// `calcMode` attribute of `<calcPr>` (`ST_CalcMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalcMode {
    /// `"auto"` — full automatic recalculation (the default).
    Auto,
    /// `"autoNoTable"` — automatic except data tables.
    AutoNoTable,
    /// `"manual"` — only recalculates on explicit request.
    Manual,
}

/// A defined name (`<definedName>`, ECMA-376 §18.2.5 `CT_DefinedName`),
/// stored raw: this crate does not parse the formula text.
#[derive(Clone, Debug, PartialEq)]
pub struct DefinedName {
    /// The name as declared.
    pub name: String,
    /// The raw formula/reference text (no leading `=`).
    pub formula: String,
    /// `Some(local_sheet_id)` for a sheet-scoped name (`localSheetId`
    /// attribute), `None` for a workbook-scoped name.
    ///
    /// Per ECMA-376 §18.2.6 the value is a **0-based index into the
    /// workbook's `<sheets>` collection** — the index space recorded in
    /// [`Sheet::sheets_index`], *not* the position in [`Workbook::sheets`]
    /// (those diverge when the loader skips non-worksheet `<sheet>`
    /// entries). A sheet-local name is scoped to that one sheet: within its
    /// sheet it shadows a workbook-scoped name of the same string; other
    /// sheets see the workbook-scoped one. Name strings are unique per
    /// scope, not per workbook.
    pub sheet_scope: Option<u32>,
}

/// Package-level flags that don't belong to any one part.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct WorkbookFlags {
    /// Whether `xl/vbaProject.bin` is present in the package (i.e. this is
    /// really an `.xlsm`/`.xlsb`-style macro-enabled package). Per
    /// `implementation-plan.md` §1/§8 this crate never executes or even
    /// reads the macro project's contents — presence is checked against the
    /// zip's part list only, never decompressed.
    pub has_vba_project: bool,
    /// Count of `<sheet>` entries in `xl/workbook.xml` that were **skipped**
    /// because they carry no parseable worksheet cell data. Two real-Excel
    /// cases (both from the Enron corpus) feed this counter:
    ///
    /// 1. An empty (`r:id=""`) or absent `r:id` attribute — no relationship
    ///    and no part at all, as Excel writes for `state="veryHidden"` VBA
    ///    code/module sheets (e.g. a sheet literally named `"Code"`).
    /// 2. A valid `r:id` resolving to a **non-worksheet** part: a
    ///    dialogsheet (`<dialogsheet>`), chartsheet (`<chartsheet>`), or
    ///    Excel 4.0 macrosheet (`<macrosheet>`), identified by its
    ///    relationship `Type`.
    ///
    /// Distinct from [`WorkbookFlags::has_vba_project`] (that's
    /// `xl/vbaProject.bin` presence); a nonzero count here is a related but
    /// separate signal that non-data sheets were present.
    pub skipped_sheets: u32,
}
