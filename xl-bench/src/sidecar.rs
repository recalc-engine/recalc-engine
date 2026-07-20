//! Oracle sources for the diff harness: where the "expected" value for a
//! formula cell comes from.
//!
//! The real oracle (per `docs/sidecar-format.md`) is a `.sidecar.parquet`
//! file dumped from a pinned, licensed Excel build on the farm. Reading
//! parquet needs a `parquet`/`arrow`-family dependency, and per
//! the Recalc design rules's zero-dependency rule that requires a fresh, human-approved
//! dependency entry — **not present yet**. Rather than block the whole
//! harness on that approval, this module defines [`SidecarSource`] as the
//! seam a parquet-backed implementation will fill in later, and ships the
//! one oracle that needs no new dependency at all: every real `.xlsx`/`.xlsm`
//! already carries Excel's own last-computed value in each formula cell's
//! `<v>` (`xl_io::Cell::value`) — a poor-man's oracle baked into every file
//! the harness will ever open. See the crate-level docs for the "core
//! insight" this harness is built on.
//!
//! # Future work (not in scope here)
//! A `ParquetSidecarSource` implementing this same trait, backed by
//! `docs/sidecar-format.md`'s row schema, is the intended real oracle for
//! the Excel-farm corpus. It arrives in a later task once a parquet-family
//! dependency is proposed and approved (see the Recalc design rules's "Dependency
//! approvals" log) — nothing here should need to change shape when it
//! lands; callers are already written against [`SidecarSource::lookup`],
//! not directly against the cached-value struct.

use std::collections::BTreeMap;

use xl_value::Value;

/// One oracle's answer for a single formula cell.
#[derive(Clone, Debug, PartialEq)]
pub enum OracleRecord {
    /// The oracle recorded this value for the cell.
    Value(Value),
    /// The oracle has nothing for this cell — e.g. a `tools/gridgen`
    /// probe workbook (no cached `<v>` at all, since gridgen's cells only
    /// ever carry the formula, computed by the farm later) or a cell outside
    /// a parquet sidecar's dumped rows. Callers must report
    /// [`crate::diff::CellStatus::NoOracle`] for this, never a fabricated
    /// pass — this is the harness's answer to "never silently wrong" for
    /// workbooks that simply have no oracle data yet.
    NoOracle,
}

/// A source of oracle (expected) values for a workbook's formula cells,
/// addressed by `(sheet name, 0-based row, 0-based col)` — the same
/// coordinate convention as `xl_io::Sheet::cells`.
///
/// Implementations: [`CachedValueSource`] (this module, no new dependency).
/// A parquet-backed `ParquetSidecarSource` is future work — see the module
/// docs.
pub trait SidecarSource {
    /// Looks up the oracle's recorded value for one cell.
    fn lookup(&self, sheet: &str, row: u32, col: u32) -> OracleRecord;
}

/// The v0 oracle: Excel's own cached `<v>` values, as parsed by `xl-io`,
/// snapshotted **before** Recalc's engine overwrites them during `recalc()`.
///
/// # Why `Value::Blank` means "no oracle"
/// `xl-io` resolves a `<c>` with an `<f>` but no sibling `<v>` element to
/// `Value::Blank` (see `xl_io::sheet_xml`'s `resolve_value`) — this is
/// exactly what `tools/gridgen`-generated probe workbooks look like (they
/// deliberately emit no cached value; the farm computes it later). A
/// genuine Excel-authored formula cell **always** has a `<v>` after the
/// workbook was last saved after calculation (even a formula that evaluates
/// to an empty string gets a cached `<v t="str"></v>`, which parses to
/// `Value::Text("")`, not `Value::Blank`) — so a formula cell whose cached
/// value is `Value::Blank` is a reliable signal that this source has no
/// data for the cell, not that Excel "computed blank". This harness treats
/// that signal as [`OracleRecord::NoOracle`] rather than fabricating a pass
/// against a value nobody actually computed.
pub struct CachedValueSource {
    cached: BTreeMap<(String, u32, u32), Value>,
}

impl CachedValueSource {
    /// Builds a source from `(sheet name, row, col) -> cached value` pairs.
    /// `sheet` names are matched case-sensitively against whatever the
    /// caller passes to [`SidecarSource::lookup`] — callers are expected to
    /// pass through `xl_io::Sheet::name` verbatim on both sides.
    #[must_use]
    pub fn new(cached: BTreeMap<(String, u32, u32), Value>) -> CachedValueSource {
        CachedValueSource { cached }
    }
}

impl SidecarSource for CachedValueSource {
    fn lookup(&self, sheet: &str, row: u32, col: u32) -> OracleRecord {
        match self.cached.get(&(sheet.to_string(), row, col)) {
            Some(Value::Blank) | None => OracleRecord::NoOracle,
            // BC-6 (RFC-0012): a lambda is never a scorable oracle value (Excel
            // writes `#CALC!` for a bare lambda cell, not a lambda). Treat it as
            // no-oracle rather than flowing it downstream into `diff::classify`
            // — this keeps the oracle side and the compute side in agreement
            // (classify refuses a computed lambda too), never a silent score.
            Some(Value::Lambda(_)) => OracleRecord::NoOracle,
            Some(v) => OracleRecord::Value(v.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_cached_value_is_no_oracle() {
        let mut cached = BTreeMap::new();
        cached.insert(("Sheet1".to_string(), 0, 0), Value::Blank);
        let src = CachedValueSource::new(cached);
        assert_eq!(src.lookup("Sheet1", 0, 0), OracleRecord::NoOracle);
    }

    #[test]
    fn missing_entry_is_no_oracle() {
        let src = CachedValueSource::new(BTreeMap::new());
        assert_eq!(src.lookup("Sheet1", 0, 0), OracleRecord::NoOracle);
    }

    #[test]
    fn present_value_round_trips() {
        let mut cached = BTreeMap::new();
        cached.insert(("Sheet1".to_string(), 0, 0), Value::number(5.0));
        let src = CachedValueSource::new(cached);
        assert_eq!(
            src.lookup("Sheet1", 0, 0),
            OracleRecord::Value(Value::number(5.0))
        );
    }

    #[test]
    fn cached_lambda_is_no_oracle() {
        // BC-6 (RFC-0012): a lambda is never a scorable oracle value — it maps
        // to `NoOracle` (agreeing with `diff::classify`, which refuses a
        // computed lambda too), never flowing downstream as a false score.
        let mut cached = BTreeMap::new();
        cached.insert(("Sheet1".to_string(), 0, 0), Value::test_lambda());
        let src = CachedValueSource::new(cached);
        assert_eq!(src.lookup("Sheet1", 0, 0), OracleRecord::NoOracle);
    }
}
