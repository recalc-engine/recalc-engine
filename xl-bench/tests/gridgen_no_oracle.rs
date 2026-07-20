//! Integration test: a `tools/gridgen`-generated probe workbook has **no**
//! cached values at all (gridgen deliberately writes `<f>` with no sibling
//! `<v>` — see `tools/gridgen/xlsxbuild.py`'s `CellFormula` handling); every
//! one of its formula cells must report [`xl_bench::diff::CellStatus::NoOracle`],
//! never a fabricated `Exact`/`Mismatch` against a value nobody actually
//! computed. This is the harness's answer to
//! `implementation-plan.md` §0 ("never silently wrong") for the exact
//! scenario the task description calls out.
//!
//! Shells out to `python3 tools/gridgen/gridgen.py` (stdlib-only, no
//! network) to generate a small `SUM` grid into a temp directory. If
//! `python3` is not on `PATH` in whatever environment runs this test suite,
//! the test self-skips **loudly** (a `SKIPPED:` line on stdout, visible
//! with `--nocapture` and in CI logs) rather than failing red — this
//! harness itself has no Python dependency; gridgen is a separate, optional
//! corpus-generation tool. CI declares python3 explicitly in the test job
//! (`.github/workflows/ci.yml`) so the skip never silently hides this
//! coverage there.

use std::path::PathBuf;
use std::process::Command;

use xl_bench::diff::{CellStatus, DiffConfig};
use xl_bench::report::run_workbook;

fn repo_root() -> PathBuf {
    // xl-bench/ -> repo root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xl-bench has a parent directory")
        .to_path_buf()
}

#[test]
fn gridgen_workbook_reports_no_oracle_everywhere() {
    let root = repo_root();
    let gridgen = root.join("tools/gridgen/gridgen.py");
    if !gridgen.exists() {
        println!(
            "SKIPPED: gridgen_workbook_reports_no_oracle_everywhere — {gridgen:?} not found \
             (gridgen coverage NOT exercised in this run)"
        );
        return;
    }

    let out_dir = std::env::temp_dir().join(format!(
        "xl-bench-gridgen-test-{}-{}",
        std::process::id(),
        "sum"
    ));
    let _ = std::fs::remove_dir_all(&out_dir);

    let status = Command::new("python3")
        .arg(&gridgen)
        .arg("--fn")
        .arg("SUM")
        .arg("--out")
        .arg(&out_dir)
        .arg("--max-rows-per-workbook")
        .arg("50")
        .status();

    let status = match status {
        Ok(s) => s,
        Err(e) => {
            println!(
                "SKIPPED: gridgen_workbook_reports_no_oracle_everywhere — could not run \
                 python3 ({e}); gridgen coverage NOT exercised in this run"
            );
            return;
        }
    };
    assert!(status.success(), "gridgen.py failed");

    let mut xlsx_files: Vec<PathBuf> = std::fs::read_dir(&out_dir)
        .expect("gridgen wrote its output directory")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("xlsx"))
        .collect();
    xlsx_files.sort();
    assert!(!xlsx_files.is_empty(), "gridgen produced no .xlsx files");

    let mut total_formula_cells = 0usize;
    for path in &xlsx_files {
        let report = run_workbook(path, DiffConfig::default())
            .unwrap_or_else(|e| panic!("failed to load gridgen workbook {path:?}: {e}"));
        assert!(
            report.summary.total_formula_cells > 0,
            "gridgen workbook {path:?} has no formula cells"
        );
        for cell in &report.cells {
            assert_eq!(
                cell.status,
                CellStatus::NoOracle,
                "gridgen cell {}!{} should be NoOracle (no cached <v>), got {:?}",
                cell.sheet,
                cell.cell_ref,
                cell.status
            );
        }
        total_formula_cells += report.summary.total_formula_cells;
        assert_eq!(report.summary.no_oracle, report.summary.total_formula_cells);
        assert_eq!(report.summary.exact, 0);
        assert_eq!(report.summary.mismatch, 0);
        assert!(report.summary.fidelity_pct().is_none());
        assert!(report.summary.strict_fidelity_pct().is_none());
    }
    assert!(total_formula_cells > 0);

    let _ = std::fs::remove_dir_all(&out_dir);
}
