//! Wires `xl-io` (load + cached values) and `xl-engine` (recalc) into one
//! workbook-level diff run, producing the record/summary shapes the
//! JSON/HTML reporters serialize.
//!
//! # Pipeline (the "core insight" this harness is built on)
//! 1. Open the workbook with `xl-io` — this parses every formula's raw text
//!    *and* Excel's own cached `<v>` value for the cell (`xl_io::Cell`).
//! 2. Snapshot every formula cell's cached value into a
//!    [`crate::sidecar::CachedValueSource`] **before** handing the workbook
//!    to `xl-engine` (`Engine::load` takes `Workbook` by value and the
//!    engine's own value store is later overwritten by `recalc()` — the
//!    cached values must be captured first or they're gone).
//! 3. Load + `recalc()` the engine (single-threaded, deterministic —
//!    `xl-engine`'s own contract).
//! 4. For every formula cell, [`crate::diff::classify`] the engine's
//!    computed value against the cached-value oracle.

use std::collections::BTreeMap;
use std::path::Path;

use xl_io::{CalcMode, DateSystem};
use xl_value::Value;

use crate::addr::a1_ref;
use crate::diff::{CellStatus, DiffConfig, classify};
use crate::sidecar::{CachedValueSource, SidecarSource};

/// Everything that went wrong opening/parsing a workbook — maps 1:1 to the
/// CLI's exit code 2 ("load/parse failure").
#[derive(Debug)]
pub enum RunError {
    /// `xl-io` could not open or parse the package.
    Load(xl_io::IoError),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Load(e) => write!(f, "failed to load workbook: {e}"),
        }
    }
}

impl std::error::Error for RunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RunError::Load(e) => Some(e),
        }
    }
}

/// Engine identity recorded in every report, so a fidelity number can always
/// be traced back to the exact Recalc build that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineMeta {
    /// `xl-bench`'s own `CARGO_PKG_VERSION`. Every workspace crate currently
    /// shares one `[workspace.package] version` (`Cargo.toml`), so this is
    /// equivalent to `xl-engine`'s version too; if crates ever version
    /// independently, this field should be revisited to name `xl-engine`
    /// specifically (tracked here as a documented simplification, not a
    /// silent gap).
    pub version: String,
    /// Short git commit hash, when determinable — see [`engine_meta`] for
    /// the resolution order. `"unknown"` if neither source works (e.g. a
    /// source tarball with no `.git` and no env var set).
    pub git_hash: String,
}

/// Resolves [`EngineMeta`]: `version` from `CARGO_PKG_VERSION`; `git_hash`
/// from the `RECALC_GIT_HASH` environment variable if set (the documented
/// hook for CI/release builds to pin an exact hash without relying on a
/// `.git` directory being present at runtime), else by shelling out to
/// `git rev-parse --short HEAD` in the current directory, else `"unknown"`.
///
/// **Memoized** (`OnceLock`): the identity of the running engine cannot
/// change mid-process, and shelling out to `git` per workbook is measurable
/// overhead on a corpus-scale `verify-dir` run (~10ms × thousands of files),
/// so the resolution runs exactly once per process.
#[must_use]
pub fn engine_meta() -> EngineMeta {
    static META: std::sync::OnceLock<EngineMeta> = std::sync::OnceLock::new();
    META.get_or_init(|| {
        let version = env!("CARGO_PKG_VERSION").to_string();
        let git_hash = std::env::var("RECALC_GIT_HASH")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(git_hash_from_repo)
            .unwrap_or_else(|| "unknown".to_string());
        EngineMeta { version, git_hash }
    })
    .clone()
}

fn git_hash_from_repo() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8(output.stdout).ok()?;
    let hash = hash.trim();
    if hash.is_empty() {
        None
    } else {
        Some(hash.to_string())
    }
}

/// Package-level workbook feature flags surfaced in every report — read
/// straight off `xl-io`'s parsed [`xl_io::Workbook`], not recomputed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkbookFlagsReport {
    /// `xl/vbaProject.bin` is present (`.xlsm`-style macro package). Recalc
    /// never executes or reads its contents (`implementation-plan.md` §1).
    pub has_vba_project: bool,
    /// `true` for the 1904 date system, `false` for the (default) 1900
    /// system.
    pub date_system_1904: bool,
    /// The workbook's requested `calcMode` (`"auto"`, `"auto_no_table"`, or
    /// `"manual"`) — informational; the harness always fully recalculates
    /// regardless of what the file requests, mirroring `oracle/dump_workbook.py`'s
    /// documented "the dump script forces a full rebuild regardless" policy
    /// (`docs/sidecar-format.md`).
    pub calc_mode: &'static str,
}

fn calc_mode_str(mode: CalcMode) -> &'static str {
    match mode {
        CalcMode::Auto => "auto",
        CalcMode::AutoNoTable => "auto_no_table",
        CalcMode::Manual => "manual",
    }
}

/// One formula cell's diff record.
#[derive(Clone, Debug, PartialEq)]
pub struct CellRecord {
    /// Sheet display name.
    pub sheet: String,
    /// A1-style reference (e.g. `"B7"`).
    pub cell_ref: String,
    /// 0-based row (matches `xl_io::Sheet::cells`'s convention).
    pub row: u32,
    /// 0-based column.
    pub col: u32,
    /// The formula text, A1-style with a leading `=` (matching
    /// `docs/sidecar-format.md`'s `formula` column convention), or a
    /// placeholder for a shared/array follow-on cell with no formula body
    /// of its own (see `xl_io::RawFormula::text`'s docs).
    pub formula: String,
    /// The diff classification.
    pub status: CellStatus,
}

/// Workbook-level counts and the two fidelity percentages
/// (`implementation-plan.md` §0: "never let the explicit-gap bucket inflate
/// the headline number" — report both, never just one).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorkbookSummary {
    /// Total formula cells examined (every cell with an `<f>`, regardless of
    /// status).
    pub total_formula_cells: usize,
    pub exact: usize,
    pub ulp_diff: usize,
    pub mismatch: usize,
    pub engine_unsupported: usize,
    pub no_oracle: usize,
}

impl WorkbookSummary {
    /// Tallies a set of per-cell records into workbook-level counts.
    #[must_use]
    pub fn from_records(records: &[CellRecord]) -> WorkbookSummary {
        let mut s = WorkbookSummary {
            total_formula_cells: records.len(),
            exact: 0,
            ulp_diff: 0,
            mismatch: 0,
            engine_unsupported: 0,
            no_oracle: 0,
        };
        for r in records {
            match r.status {
                CellStatus::Exact => s.exact += 1,
                CellStatus::UlpDiff { .. } => s.ulp_diff += 1,
                CellStatus::Mismatch { .. } => s.mismatch += 1,
                CellStatus::EngineUnsupported => s.engine_unsupported += 1,
                CellStatus::NoOracle => s.no_oracle += 1,
            }
        }
        s
    }

    /// **Lenient fidelity %**: `(Exact + UlpDiff) / (total - Unsupported -
    /// NoOracle) * 100`. Excludes both explicit-gap buckets (Recalc's own
    /// admitted non-support, and cells this run had no oracle for at all)
    /// from the denominator — this is "of the cells we could actually
    /// judge and that Recalc attempted, how many matched". `None` if the
    /// denominator is zero (nothing judgeable in this workbook).
    #[must_use]
    pub fn fidelity_pct(&self) -> Option<f64> {
        let denom = self
            .total_formula_cells
            .saturating_sub(self.engine_unsupported)
            .saturating_sub(self.no_oracle);
        pct(self.exact + self.ulp_diff, denom)
    }

    /// **Strict fidelity %**: `(Exact + UlpDiff) / (total - NoOracle) *
    /// 100`. Counts `EngineUnsupported` cells as failures (they are not
    /// excluded from the denominator here) — this is "of the cells we had
    /// an oracle for, how many did Recalc actually get right, including the
    /// ones it flatly refused to compute". `NoOracle` cells are still
    /// excluded: there is nothing to be strict *about* without a real
    /// expected value. `None` if the denominator is zero.
    #[must_use]
    pub fn strict_fidelity_pct(&self) -> Option<f64> {
        let denom = self.total_formula_cells.saturating_sub(self.no_oracle);
        pct(self.exact + self.ulp_diff, denom)
    }

    /// `true` if any cell is a genuine [`CellStatus::Mismatch`] — the CLI's
    /// "exit 1" condition.
    #[must_use]
    pub fn has_mismatch(&self) -> bool {
        self.mismatch > 0
    }
}

fn pct(numerator: usize, denominator: usize) -> Option<f64> {
    if denominator == 0 {
        None
    } else {
        Some(100.0 * numerator as f64 / denominator as f64)
    }
}

/// A full diff run for one workbook.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkbookReport {
    /// The path the workbook was loaded from, as given by the caller
    /// (display form — not canonicalized).
    pub workbook_path: String,
    pub engine: EngineMeta,
    pub flags: WorkbookFlagsReport,
    /// Per-cell records, in sheet-tab-order then row/col order (matches
    /// `xl_io::Sheet::cells`'s `BTreeMap` iteration order).
    pub cells: Vec<CellRecord>,
    pub summary: WorkbookSummary,
}

/// The summary-only view of one workbook's run, retained by the corpus
/// runner for the aggregate index. Deliberately **excludes** the per-cell
/// records: on a corpus-scale `verify-dir` run (thousands of workbooks,
/// potentially hundreds of thousands of formula cells each), keeping every
/// [`WorkbookReport::cells`] vector alive for the whole run is unbounded
/// memory growth for data the index never reads — per-cell detail lives in
/// the per-file HTML/JSON reports written (and dropped) as each workbook
/// completes.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkbookIndexEntry {
    /// Same as [`WorkbookReport::workbook_path`].
    pub workbook_path: String,
    /// Same as [`WorkbookReport::summary`].
    pub summary: WorkbookSummary,
}

impl From<&WorkbookReport> for WorkbookIndexEntry {
    fn from(report: &WorkbookReport) -> WorkbookIndexEntry {
        WorkbookIndexEntry {
            workbook_path: report.workbook_path.clone(),
            summary: report.summary,
        }
    }
}

/// Loads `path`, recalculates it, and diffs every formula cell against
/// Excel's own cached values. See the module docs for the pipeline.
pub fn run_workbook(path: &Path, cfg: DiffConfig) -> Result<WorkbookReport, RunError> {
    let workbook = xl_io::open(path).map_err(RunError::Load)?;

    let has_vba_project = workbook.flags.has_vba_project;
    let date_system_1904 = matches!(workbook.date_system, DateSystem::Excel1904);
    let calc_mode = calc_mode_str(workbook.calc_settings.calc_mode);

    // Snapshot every formula cell's identity + cached value *before* handing
    // the workbook to the engine (Engine::load consumes it, and recalc()
    // overwrites the value store the cache came from).
    struct FormulaCell {
        sheet: String,
        row: u32,
        col: u32,
        formula: String,
        cached: Value,
    }
    let mut formula_cells: Vec<FormulaCell> = Vec::new();
    for sheet in &workbook.sheets {
        for (&(row, col), cell) in &sheet.cells {
            let Some(raw) = &cell.formula else {
                continue;
            };
            let formula = match &raw.text {
                Some(text) => format!("={text}"),
                None => "<shared/array follow-on, no formula body>".to_string(),
            };
            formula_cells.push(FormulaCell {
                sheet: sheet.name.clone(),
                row,
                col,
                formula,
                cached: cell.value.clone(),
            });
        }
    }

    let mut cached_map: BTreeMap<(String, u32, u32), Value> = BTreeMap::new();
    for fc in &formula_cells {
        cached_map.insert((fc.sheet.clone(), fc.row, fc.col), fc.cached.clone());
    }
    let oracle = CachedValueSource::new(cached_map);

    let mut engine = xl_engine::Engine::load(workbook);
    engine.recalc();

    let mut cells = Vec::with_capacity(formula_cells.len());
    for fc in formula_cells {
        let computed = engine
            .sheet_id(&fc.sheet)
            .and_then(|sid| engine.value(sid, fc.row, fc.col))
            .cloned()
            .unwrap_or(Value::Blank);
        let oracle_record = oracle.lookup(&fc.sheet, fc.row, fc.col);
        let status = classify(&computed, &oracle_record, cfg);
        cells.push(CellRecord {
            sheet: fc.sheet,
            cell_ref: a1_ref(fc.row, fc.col),
            row: fc.row,
            col: fc.col,
            formula: fc.formula,
            status,
        });
    }

    let summary = WorkbookSummary::from_records(&cells);

    Ok(WorkbookReport {
        workbook_path: path.display().to_string(),
        engine: engine_meta(),
        flags: WorkbookFlagsReport {
            has_vba_project,
            date_system_1904,
            calc_mode,
        },
        cells,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fidelity_pct_excludes_unsupported_and_no_oracle() {
        let s = WorkbookSummary {
            total_formula_cells: 10,
            exact: 4,
            ulp_diff: 1,
            mismatch: 2,
            engine_unsupported: 2,
            no_oracle: 1,
        };
        // denom = 10 - 2 - 1 = 7; numerator = 5
        assert!((s.fidelity_pct().unwrap() - (500.0 / 7.0)).abs() < 1e-9);
        // strict denom = 10 - 1 = 9; numerator = 5
        assert!((s.strict_fidelity_pct().unwrap() - (500.0 / 9.0)).abs() < 1e-9);
        assert!(s.strict_fidelity_pct().unwrap() < s.fidelity_pct().unwrap());
    }

    #[test]
    fn fidelity_pct_none_when_nothing_judgeable() {
        let s = WorkbookSummary {
            total_formula_cells: 5,
            exact: 0,
            ulp_diff: 0,
            mismatch: 0,
            engine_unsupported: 0,
            no_oracle: 5,
        };
        assert_eq!(s.fidelity_pct(), None);
        assert_eq!(s.strict_fidelity_pct(), None);
    }

    #[test]
    fn engine_meta_has_a_version() {
        let meta = engine_meta();
        assert!(!meta.version.is_empty());
        assert!(!meta.git_hash.is_empty());
    }
}
