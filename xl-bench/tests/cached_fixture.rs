//! Integration test: the vendored `cached_values.xlsx` fixture
//! (`tests/fixtures/`, see its `README.md` for provenance) exercises every
//! [`xl_bench::diff::CellStatus`] variant this harness produces from a real
//! `.xlsx` package — exact match, a deliberately poisoned mismatch, an
//! unsupported-function explicit gap, and a no-cached-value cell — plus
//! JSON/HTML report generation and the CLI's exit-code contract end-to-end.

use std::path::PathBuf;

use xl_bench::diff::{CellStatus, DiffConfig};
use xl_bench::html::workbook_report_to_html;
use xl_bench::json::workbook_report_to_json;
use xl_bench::report::run_workbook;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cached_values.xlsx")
}

#[test]
fn every_classification_case_is_reachable() {
    let report = run_workbook(&fixture_path(), DiffConfig::default()).expect("fixture loads");

    // A1/A2 are plain literals — never counted as formula cells.
    assert_eq!(report.summary.total_formula_cells, 4);

    let status = |cell_ref: &str| {
        report
            .cells
            .iter()
            .find(|c| c.cell_ref == cell_ref)
            .unwrap_or_else(|| panic!("no cell record for {cell_ref}"))
            .status
            .clone()
    };

    assert_eq!(status("A3"), CellStatus::Exact);
    assert!(matches!(status("A4"), CellStatus::Mismatch { .. }));
    if let CellStatus::Mismatch { expected, actual } = status("A4") {
        assert_eq!(expected, xl_value::Value::number(999.0));
        assert_eq!(actual, xl_value::Value::number(5.0));
    }
    assert_eq!(status("A5"), CellStatus::EngineUnsupported);
    assert_eq!(status("A6"), CellStatus::NoOracle);

    assert_eq!(report.summary.exact, 1);
    assert_eq!(report.summary.mismatch, 1);
    assert_eq!(report.summary.engine_unsupported, 1);
    assert_eq!(report.summary.no_oracle, 1);

    // Lenient fidelity excludes Unsupported+NoOracle: denom = 4-1-1=2, num=1.
    assert!((report.summary.fidelity_pct().unwrap() - 50.0).abs() < 1e-9);
    // Strict fidelity only excludes NoOracle: denom = 4-1=3, num=1.
    assert!((report.summary.strict_fidelity_pct().unwrap() - (100.0 / 3.0)).abs() < 1e-9);
    assert!(report.summary.has_mismatch());
}

#[test]
fn json_report_contains_every_status_kind() {
    let report = run_workbook(&fixture_path(), DiffConfig::default()).expect("fixture loads");
    let json = workbook_report_to_json(&report);

    for needle in [
        "\"kind\":\"exact\"",
        "\"kind\":\"mismatch\"",
        "\"kind\":\"engine_unsupported\"",
        "\"kind\":\"no_oracle\"",
    ] {
        assert!(json.contains(needle), "missing {needle} in:\n{json}");
    }
    // The poisoned oracle value and the engine's correct value both appear.
    assert!(json.contains("999"));
    assert!(json.contains("\"mismatch\": 1") || json.contains("\"mismatch\":1"));
}

#[test]
fn html_report_renders_summary_and_mismatch_row() {
    let report = run_workbook(&fixture_path(), DiffConfig::default()).expect("fixture loads");
    let html = workbook_report_to_html(&report);

    assert!(html.contains("<html>"));
    assert!(html.contains("A4"));
    assert!(html.contains("999"));
    assert!(!html.contains("<script"));
}

#[test]
fn cli_verify_exits_1_on_mismatch_and_writes_reports() {
    let bin = env!("CARGO_BIN_EXE_recalc");
    let tmp = std::env::temp_dir().join(format!(
        "xl-bench-cli-test-{}-{}",
        std::process::id(),
        "fixture"
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let html_out = tmp.join("report.html");
    let json_out = tmp.join("report.json");

    let output = std::process::Command::new(bin)
        .arg("verify")
        .arg(fixture_path())
        .arg("--html")
        .arg(&html_out)
        .arg("--json")
        .arg(&json_out)
        .arg("--quiet")
        .output()
        .expect("recalc binary runs");

    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(html_out.exists());
    assert!(json_out.exists());

    std::fs::remove_dir_all(&tmp).unwrap();
}

/// Exit code 2: a file that doesn't parse as a workbook at all (not even a
/// zip archive) — the CLI's "load/parse failure" contract.
#[test]
fn cli_verify_exits_2_on_load_failure() {
    let bin = env!("CARGO_BIN_EXE_recalc");
    let tmp = std::env::temp_dir().join(format!(
        "xl-bench-cli-test-{}-{}",
        std::process::id(),
        "not-a-workbook"
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let bogus = tmp.join("not_a_workbook.xlsx");
    std::fs::write(&bogus, b"this is not a zip file").unwrap();

    let output = std::process::Command::new(bin)
        .arg("verify")
        .arg(&bogus)
        .arg("--quiet")
        .output()
        .expect("recalc binary runs");

    assert_eq!(output.status.code(), Some(2));

    std::fs::remove_dir_all(&tmp).unwrap();
}

/// Exit code 0: the mismatch-free `clean_values.xlsx` fixture (see
/// `tests/fixtures/README.md`) — one formula cell whose cached value the
/// engine reproduces exactly, so nothing is WRONG.
#[test]
fn cli_verify_exits_0_when_nothing_is_wrong() {
    let clean = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/clean_values.xlsx");

    let bin = env!("CARGO_BIN_EXE_recalc");
    let output = std::process::Command::new(bin)
        .arg("verify")
        .arg(&clean)
        .arg("--quiet")
        .output()
        .expect("recalc binary runs");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_verify_policy_mode_emits_v1_report_and_requires_json() {
    let bin = env!("CARGO_BIN_EXE_recalc");
    let policy =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/verify-policy.toml");
    let tmp = std::env::temp_dir().join(format!("xl-bench-v1-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("report.json");
    let output = std::process::Command::new(bin)
        .args(["verify"])
        .arg(fixture_path())
        .args(["--policy"])
        .arg(policy)
        .args(["--json"])
        .arg(&out)
        .arg("--quiet")
        .output()
        .expect("recalc binary runs");
    assert_eq!(output.status.code(), Some(1));
    let json = std::fs::read_to_string(&out).unwrap();
    assert!(json.contains("\"schema_version\":\"recalc.verify.report/v1\""));
    assert!(json.contains("\"candidate_sha256\":\""));
    std::fs::remove_dir_all(&tmp).unwrap();
}

#[test]
fn cli_verify_accepts_identified_excel_result() {
    let bin = env!("CARGO_BIN_EXE_recalc");
    let policy =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/verify-policy.toml");
    let tmp = std::env::temp_dir().join(format!("xl-bench-excel-v1-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("report.json");
    let output = std::process::Command::new(bin)
        .args(["verify"])
        .arg(fixture_path())
        .args(["--excel-result"])
        .arg(fixture_path())
        .args(["--excel-build", "16.0.12345.20000", "--policy"])
        .arg(policy)
        .args(["--json"])
        .arg(&out)
        .arg("--quiet")
        .output()
        .expect("recalc binary runs");
    assert_eq!(output.status.code(), Some(1));
    let json = std::fs::read_to_string(&out).unwrap();
    assert!(json.contains("supplied_excel_result_sha256"));
    assert!(json.contains("16.0.12345.20000"));
    std::fs::remove_dir_all(&tmp).unwrap();
}

/// Exit code 64 (`USAGE`, `docs/specs/recalc-verify-v1.md` §3): invalid
/// arguments claim no verification decision, so they must not reuse the
/// `FALLBACK` code 2 that a load failure legitimately returns.
#[test]
fn cli_verify_exits_64_on_invalid_arguments() {
    let bin = env!("CARGO_BIN_EXE_recalc");
    let output = std::process::Command::new(bin)
        .arg("verify")
        .arg("--quiet")
        .output()
        .expect("recalc binary runs");
    assert_eq!(output.status.code(), Some(64));
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing <book.xlsx>"));
}
