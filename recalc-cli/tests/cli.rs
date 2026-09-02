//! End-to-end tests of the standalone `recalc` binary: version output and the
//! four contract exit codes, driven through the shipped example workbooks.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_recalc")
}

fn example(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(name)
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("recalc-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn version_names_the_binary_version_and_revision() {
    let out = Command::new(bin()).arg("--version").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.starts_with(&format!("recalc {} (", env!("CARGO_PKG_VERSION"))),
        "got: {text}"
    );
    assert!(text.trim_end().ends_with(')'), "got: {text}");
}

#[test]
fn demo_workbook_passes_with_exit_0_and_a_valid_report() {
    let report = tmp("demo.json");
    let out = Command::new(bin())
        .args(["verify"])
        .arg(example("demo.xlsx"))
        .arg("--policy")
        .arg(example("recalc-policy.toml"))
        .arg("--json")
        .arg(&report)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("PASS  "), "stdout: {stdout}");
    let json = std::fs::read_to_string(&report).unwrap();
    assert!(json.contains("\"schema_version\":\"recalc.verify.report/v1\""));
    assert!(json.contains("\"decision\":\"pass\""));
}

#[test]
fn stale_workbook_fails_with_exit_1_and_names_the_cells() {
    let report = tmp("stale.json");
    let out = Command::new(bin())
        .args(["verify"])
        .arg(example("demo-stale.xlsx"))
        .arg("--policy")
        .arg(example("recalc-policy.toml"))
        .arg("--json")
        .arg(&report)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("FAIL  "), "stdout: {stdout}");
    assert!(stdout.contains("mismatch"), "stdout: {stdout}");
    let json = std::fs::read_to_string(&report).unwrap();
    assert!(json.contains("\"decision\":\"fail\""));
    assert!(json.contains("differs_stored"));
}

#[test]
fn quiet_suppresses_the_human_summary_only() {
    let report = tmp("quiet.json");
    let out = Command::new(bin())
        .args(["verify"])
        .arg(example("demo.xlsx"))
        .arg("--json")
        .arg(&report)
        .arg("--quiet")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
    assert!(report.exists());
}

#[test]
fn not_a_workbook_is_fallback_exit_2() {
    let out = Command::new(bin())
        .args(["verify"])
        .arg(example("README.md"))
        .arg("--quiet")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn usage_errors_exit_64() {
    for argv in [
        vec!["verify"],
        vec!["verify", "--quiet"],
        vec!["verify", "book.xlsx", "--json"],
        vec!["frobnicate"],
        vec![],
    ] {
        let out = Command::new(bin()).args(&argv).output().unwrap();
        assert_eq!(out.status.code(), Some(64), "argv: {argv:?}");
    }
    let out = Command::new(bin()).arg("--help").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
}
