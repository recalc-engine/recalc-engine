//! `recalc verify-dir`: recursively runs the diff harness over every
//! `.xlsx`/`.xlsm` under a directory, tolerating individual bad files, and
//! produces per-file reports plus one aggregate index.
//!
//! This is the entry point the plan's M0 gate ("HTML report on 100 real
//! workbooks") and the eventual Enron-corpus run both go through
//! (`implementation-plan.md` §12). **Robustness is the point**: one
//! corrupt/unsupported file must never abort the whole run — a load
//! failure becomes a [`crate::json::CorpusEntryResult::LoadFailure`] row,
//! not a panic or an early return.

use std::path::{Path, PathBuf};

use crate::diff::DiffConfig;
use crate::html::{corpus_index_to_html, workbook_report_to_html};
use crate::json::{CorpusEntryResult, corpus_index_to_json, workbook_report_to_json};
use crate::report::{WorkbookIndexEntry, run_workbook};

/// Recursively finds every `.xlsx`/`.xlsm` file under `dir` (extension match
/// is ASCII case-insensitive), in a deterministic (sorted) order.
#[must_use]
pub fn discover_workbooks(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Symlink loops are a known hazard of unbounded recursive walks;
        // `Path::is_dir` follows symlinks, but real xlsx corpora (the
        // Enron-style drop this feeds) are plain directory trees, so this
        // is accepted as a v0 limitation rather than added complexity to
        // guard against a threat this corpus doesn't have. Untrusted zip
        // *contents* are already hardened in `xl-io`; this only walks the
        // filesystem.
        if path.is_dir() {
            walk(&path, out);
        } else if is_workbook_ext(&path) {
            out.push(path);
        }
    }
}

fn is_workbook_ext(path: &Path) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => ext.eq_ignore_ascii_case("xlsx") || ext.eq_ignore_ascii_case("xlsm"),
        None => false,
    }
}

/// FNV-1a 64-bit hash — the standard offset-basis/prime constants. Used
/// only to make report basenames collision-safe ([`report_basename`]); not
/// a cryptographic hash and not part of any output contract. Hand-rolled
/// (~5 lines) because no hashing dependency is approved and `std`'s
/// `DefaultHasher` explicitly does not promise a stable algorithm across
/// Rust releases — report filenames should not silently change on a
/// toolchain bump.
#[must_use]
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Turns a workbook path into a filesystem-safe, **collision-free** basename
/// for its per-file HTML report, e.g. `corpus/sub dir/book.xlsx` →
/// `corpus_sub_dir_book.xlsx.a1b2c3d4.html`. Only used for the on-disk
/// report filename; the report's own `workbook` field always carries the
/// real path.
///
/// The 8-hex suffix is an FNV-1a hash of the *original* path string,
/// because the readable prefix alone is lossy: flattening separators to `_`
/// maps `foo/bar.xlsx` and `foo_bar.xlsx` to the same prefix, and without
/// the hash the second report would silently overwrite the first.
#[must_use]
fn report_basename(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut name = String::new();
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
            name.push(c);
        } else {
            name.push('_');
        }
    }
    let h = fnv1a_64(raw.as_bytes());
    // XOR-fold to 32 bits so both halves of the 64-bit hash contribute.
    let hash32 = ((h >> 32) ^ (h & 0xffff_ffff)) as u32;
    name.push_str(&format!(".{hash32:08x}.html"));
    name
}

/// The result of a `verify-dir` run: every attempted file's outcome, plus
/// whether any per-file HTML was written.
pub struct CorpusRun {
    pub entries: Vec<CorpusEntryResult>,
}

impl CorpusRun {
    /// `true` if any file failed to load (distinct from a mismatch) — used
    /// to choose exit code 2 vs. 1, see `bin/recalc.rs`.
    #[must_use]
    pub fn has_load_failure(&self) -> bool {
        self.entries
            .iter()
            .any(|e| matches!(e, CorpusEntryResult::LoadFailure { .. }))
    }

    /// `true` if any successfully-loaded workbook has a genuine mismatch.
    #[must_use]
    pub fn has_mismatch(&self) -> bool {
        self.entries.iter().any(|e| match e {
            CorpusEntryResult::Ok(r) => r.summary.has_mismatch(),
            CorpusEntryResult::LoadFailure { .. } => false,
        })
    }
}

/// Runs every workbook under `dir` through the harness. When `html_dir` is
/// `Some`, writes one HTML report per successfully-loaded workbook plus an
/// `index.html`/`index.json` aggregating all of them (including load
/// failures); the directory is created if it doesn't exist. Never returns
/// early on a per-file failure — only a failure to create/write `html_dir`
/// itself is propagated as `Err`.
///
/// # Memory (corpus-scale by design)
/// Each workbook's full [`crate::report::WorkbookReport`] — including its
/// per-cell records — is dropped as soon as its per-file HTML report is
/// written; only a summary-only [`WorkbookIndexEntry`] is retained for the
/// aggregate index. Memory across the run is therefore proportional to the
/// number of *files*, not the number of formula cells.
///
/// # Progress
/// `progress` is invoked once per attempted file, immediately after that
/// file completes (loaded-and-diffed or load-failed), with
/// `(files_done_so_far, total_files, &entry)` — this is how the CLI streams
/// per-file result lines during a long corpus run instead of staying silent
/// until the end. Pass `|_, _, _| {}` when no streaming output is wanted.
pub fn verify_dir(
    dir: &Path,
    cfg: DiffConfig,
    html_dir: Option<&Path>,
    mut progress: impl FnMut(usize, usize, &CorpusEntryResult),
) -> std::io::Result<CorpusRun> {
    if let Some(out) = html_dir {
        std::fs::create_dir_all(out)?;
    }

    let paths = discover_workbooks(dir);
    let total = paths.len();
    let mut entries = Vec::with_capacity(total);
    let mut html_links = std::collections::BTreeMap::new();

    for (done, path) in paths.into_iter().enumerate() {
        let entry = match run_workbook(&path, cfg) {
            Ok(report) => {
                if let Some(out) = html_dir {
                    let basename = report_basename(&path);
                    let html = workbook_report_to_html(&report);
                    std::fs::write(out.join(&basename), html)?;
                    html_links.insert(report.workbook_path.clone(), basename);
                }
                // The full report (with its per-cell Vec) is dropped here;
                // only the summary-only entry survives the loop iteration.
                CorpusEntryResult::Ok(WorkbookIndexEntry::from(&report))
            }
            Err(e) => CorpusEntryResult::LoadFailure {
                path: path.display().to_string(),
                message: e.to_string(),
            },
        };
        progress(done + 1, total, &entry);
        entries.push(entry);
    }

    if let Some(out) = html_dir {
        let index = corpus_index_to_html(&entries, &html_links);
        std::fs::write(out.join("index.html"), index)?;
        let index_json = corpus_index_to_json(&entries);
        std::fs::write(out.join("index.json"), index_json)?;
    }

    Ok(CorpusRun { entries })
}

/// Convenience for callers that only want the JSON view without writing
/// files (e.g. tests).
#[must_use]
pub fn to_json(run: &CorpusRun) -> String {
    corpus_index_to_json(&run.entries)
}

#[must_use]
pub fn to_json_report(report: &crate::report::WorkbookReport) -> String {
    workbook_report_to_json(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_basename_is_filesystem_safe() {
        let name = report_basename(Path::new("corpus/sub dir/book.xlsx"));
        assert!(!name.contains('/'));
        assert!(!name.contains(' '));
        assert!(name.starts_with("corpus_sub_dir_book.xlsx."));
        assert!(name.ends_with(".html"));
    }

    #[test]
    fn report_basename_does_not_collide_across_distinct_paths() {
        // Flattening separators to `_` makes these two paths' readable
        // prefixes identical; the hash suffix must keep them distinct
        // (previously the second report silently overwrote the first).
        let a = report_basename(Path::new("foo/bar.xlsx"));
        let b = report_basename(Path::new("foo_bar.xlsx"));
        assert_ne!(a, b, "distinct paths must map to distinct report names");

        // And the same path is stable across calls.
        assert_eq!(a, report_basename(Path::new("foo/bar.xlsx")));
    }

    #[test]
    fn discover_workbooks_finds_xlsx_and_xlsm_only() {
        let tmp =
            std::env::temp_dir().join(format!("xl-bench-discover-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("a.xlsx"), b"x").unwrap();
        std::fs::write(tmp.join("sub").join("b.XLSM"), b"x").unwrap();
        std::fs::write(tmp.join("ignore.txt"), b"x").unwrap();
        let found = discover_workbooks(&tmp);
        assert_eq!(found.len(), 2);
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
