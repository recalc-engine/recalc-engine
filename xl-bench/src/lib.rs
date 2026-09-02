//! `xl-bench` — the Recalc conformance harness ("ExcelBench"), v0.
//!
//! # The core insight (why this crate needs no oracle corpus to be useful)
//! Every real `.xlsx`/`.xlsm` already carries Excel's own last-computed
//! value for each formula cell — the cached `<v>` element `xl-io` parses
//! into [`xl_io::Cell::value`]. That is a poor-man's oracle baked into every
//! workbook the harness will ever open, with zero extra infrastructure: load
//! the workbook via `xl-io` (cached values + raw formulas), snapshot the
//! cached values, run `xl-engine`'s recalc, and diff computed vs. cached per
//! formula cell. See [`report::run_workbook`] for the pipeline and
//! [`sidecar`] for why this is exposed behind a trait rather than hard-coded.
//!
//! The **real** oracle — a `.sidecar.parquet` dump from a pinned, licensed
//! Excel build (`docs/sidecar-format.md`) — needs a `parquet`/`arrow`-family
//! dependency that is **not yet approved** (the Recalc design rules's zero-dependency
//! rule). Rather than block this task on that approval,
//! [`sidecar::SidecarSource`] is the seam a `ParquetSidecarSource` will fill
//! in later; today only [`sidecar::CachedValueSource`] exists.
//!
//! `tools/gridgen`-generated probe workbooks have **no** cached values at
//! all (gridgen deliberately emits formulas with no `<v>` — the farm
//! computes them later); every formula cell in such a workbook reports
//! [`diff::CellStatus::NoOracle`], never a fabricated pass. See
//! [`sidecar::CachedValueSource`]'s docs for exactly how that's detected.
//!
//! # Crate map
//! - [`diff`] — pure cell-level classification logic
//!   ([`diff::CellStatus`], [`diff::classify`]). No I/O.
//! - [`sidecar`] — oracle sources ([`sidecar::SidecarSource`],
//!   [`sidecar::CachedValueSource`]).
//! - [`report`] — wires `xl-io`/`xl-engine` together into a
//!   [`report::WorkbookReport`] ([`report::run_workbook`]).
//! - [`json`] — hand-written JSON serialization (no `serde` — see that
//!   module's docs for why).
//! - [`html`] — self-contained single-file HTML fidelity reports.
//! - [`corpus`] — `recalc verify-dir`'s recursive multi-file runner.
//! - [`decline`] — `recalc decline-attribution`'s root-cause classification of
//!   every declined (`#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`) cell.
//! - [`shared_residual`] — `recalc shared-residual`'s instrumentation of *why*
//!   each bodyless shared-formula follow-on still declines, keyed to the master
//!   formula text that failed to parse (Lane A triage).
//! - [`l2site`] — `recalc l2-decomp`'s refusal-site decomposition of the
//!   `other_shared_expanded` sub-bucket (would-expand shared follow-ons that
//!   refuse at *runtime* — the W-B doc's L2 lane).
//! - [`mismatch`] — `recalc mismatch-mine`'s corpus-wide decomposition of the
//!   genuine-fidelity-failure set (every `Mismatch` cell) by function
//!   vocabulary, expected→actual type transition, and named pattern.
//! - [`addr`] — A1-style address formatting.
//!
//! The `recalc` CLI binary (`src/bin/recalc.rs`) is a thin wrapper over
//! this library's public API.
//!
//! # Licensing note
//! This crate's harness **code** is Apache-2.0, same as the rest of the
//! workspace. The oracle corpus and sidecar artifacts it will eventually
//! consume are **PROPRIETARY** and never committed — see `README.md` in
//! this directory. Test fixtures under `tests/fixtures/` are tiny, synthetic,
//! and hand-authored (not derived from any real workbook), specifically so
//! this rule is never in tension with having tests.

#![forbid(unsafe_code)]

pub mod addr;
pub mod cellhash;
pub mod corpus;
pub mod decline;
pub mod diff;
pub mod hash;
pub mod html;
pub mod json;
pub mod l2site;
pub mod mismatch;
pub mod report;
pub mod shared_residual;
pub mod sidecar;
pub mod tier0;
pub mod verify;
pub mod verify_cli;
