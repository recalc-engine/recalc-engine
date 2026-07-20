//! Integration tests for the corpus runner (`recalc verify-dir` /
//! [`xl_bench::corpus::verify_dir`]): a temp directory holding a clean
//! workbook, a poisoned one (deliberate cached-value mismatch), and a
//! garbage non-zip `.xlsx` must produce a correct aggregate index
//! (`index.html`/`index.json`), correctly-wired per-file report links,
//! collision-safe per-file report names, streamed progress callbacks, and
//! the CLI's documented exit-code precedence (load failure `2` beats
//! mismatch `1`).

use std::path::{Path, PathBuf};

use xl_bench::corpus::verify_dir;
use xl_bench::diff::DiffConfig;
use xl_bench::json::CorpusEntryResult;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{name}"))
}

/// Builds the standard mixed corpus dir: clean + poisoned + garbage.
fn make_corpus_dir(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("xl-bench-verify-dir-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::copy(fixture("clean_values.xlsx"), dir.join("clean.xlsx")).unwrap();
    std::fs::copy(fixture("cached_values.xlsx"), dir.join("sub/poisoned.xlsx")).unwrap();
    std::fs::write(dir.join("garbage.xlsx"), b"this is not a zip archive").unwrap();
    dir
}

fn entry_for<'a>(entries: &'a [CorpusEntryResult], needle: &str) -> &'a CorpusEntryResult {
    entries
        .iter()
        .find(|e| match e {
            CorpusEntryResult::Ok(r) => r.workbook_path.contains(needle),
            CorpusEntryResult::LoadFailure { path, .. } => path.contains(needle),
        })
        .unwrap_or_else(|| panic!("no corpus entry matching {needle}"))
}

#[test]
fn mixed_corpus_produces_correct_entries_and_index_files() {
    let dir = make_corpus_dir("mixed");
    let html_dir = dir.join("out");

    let mut progress_calls: Vec<(usize, usize)> = Vec::new();
    let run = verify_dir(
        &dir,
        DiffConfig::default(),
        Some(&html_dir),
        |done, total, _| {
            progress_calls.push((done, total));
        },
    )
    .expect("verify_dir runs");

    // Three files attempted; progress streamed once per file, in order,
    // with a stable total.
    assert_eq!(run.entries.len(), 3);
    assert_eq!(progress_calls, vec![(1, 3), (2, 3), (3, 3)]);

    // Clean file: ok, no mismatch, 100% fidelity.
    match entry_for(&run.entries, "clean.xlsx") {
        CorpusEntryResult::Ok(r) => {
            assert!(!r.summary.has_mismatch());
            assert_eq!(r.summary.total_formula_cells, 1);
            assert!((r.summary.fidelity_pct().unwrap() - 100.0).abs() < 1e-9);
        }
        other => panic!("clean.xlsx should be Ok, got {other:?}"),
    }
    // Poisoned file: ok (loads fine) but has a mismatch.
    match entry_for(&run.entries, "poisoned.xlsx") {
        CorpusEntryResult::Ok(r) => assert!(r.summary.has_mismatch()),
        other => panic!("poisoned.xlsx should be Ok-with-mismatch, got {other:?}"),
    }
    // Garbage file: load failure, recorded — the run was not aborted.
    match entry_for(&run.entries, "garbage.xlsx") {
        CorpusEntryResult::LoadFailure { message, .. } => {
            assert!(!message.is_empty());
        }
        other => panic!("garbage.xlsx should be LoadFailure, got {other:?}"),
    }
    assert!(run.has_load_failure());
    assert!(run.has_mismatch());

    // Aggregate index files exist with the right rows.
    let index_html = std::fs::read_to_string(html_dir.join("index.html")).unwrap();
    assert!(index_html.contains("clean.xlsx"));
    assert!(index_html.contains("poisoned.xlsx"));
    assert!(index_html.contains("load failure"));
    assert!(index_html.contains(">mismatch<"));
    assert!(index_html.contains(">ok<"));
    assert!(index_html.contains("3 file(s)"));
    assert!(index_html.contains("1 load failure(s)"));

    let index_json = std::fs::read_to_string(html_dir.join("index.json")).unwrap();
    assert!(index_json.contains("\"status\":\"ok\""));
    assert!(index_json.contains("\"status\":\"load_failure\""));
    assert!(index_json.contains("\"mismatch\":1"));
    assert!(index_json.contains("\"mismatch\":0"));

    // html_links wiring: every href in the index points at a per-file
    // report that actually exists in the output directory.
    let mut href_count = 0;
    for piece in index_html.split("href=\"").skip(1) {
        let href = piece.split('"').next().unwrap();
        assert!(
            html_dir.join(href).exists(),
            "index.html links to missing per-file report {href}"
        );
        href_count += 1;
    }
    assert_eq!(href_count, 2, "one link per successfully-loaded workbook");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn per_file_report_names_do_not_collide_for_separator_ambiguous_paths() {
    // `foo/bar.xlsx` and `foo_bar.xlsx` flatten to the same readable prefix;
    // the hash suffix must keep their per-file reports distinct (previously
    // the second silently overwrote the first).
    let dir = std::env::temp_dir().join(format!(
        "xl-bench-verify-dir-{}-collision",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("foo")).unwrap();
    std::fs::copy(fixture("clean_values.xlsx"), dir.join("foo/bar.xlsx")).unwrap();
    std::fs::copy(fixture("clean_values.xlsx"), dir.join("foo_bar.xlsx")).unwrap();
    let html_dir = dir.join("out");

    let run = verify_dir(&dir, DiffConfig::default(), Some(&html_dir), |_, _, _| {})
        .expect("verify_dir runs");
    assert_eq!(run.entries.len(), 2);

    let reports: Vec<String> = std::fs::read_dir(&html_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "index.html" && n != "index.json")
        .collect();
    assert_eq!(
        reports.len(),
        2,
        "expected two distinct per-file reports, got {reports:?}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cli_verify_dir_exit_code_precedence_load_failure_beats_mismatch() {
    let bin = env!("CARGO_BIN_EXE_recalc");

    // Load failure present (plus a mismatch): exit 2 wins.
    let dir = make_corpus_dir("cli-precedence");
    let output = std::process::Command::new(bin)
        .arg("verify-dir")
        .arg(&dir)
        .arg("--quiet")
        .output()
        .expect("recalc binary runs");
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_dir_all(&dir).unwrap();

    // Mismatch only (no garbage file): exit 1.
    let dir = std::env::temp_dir().join(format!(
        "xl-bench-verify-dir-{}-cli-mismatch",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fixture("cached_values.xlsx"), dir.join("poisoned.xlsx")).unwrap();
    let output = std::process::Command::new(bin)
        .arg("verify-dir")
        .arg(&dir)
        .arg("--quiet")
        .output()
        .expect("recalc binary runs");
    assert_eq!(output.status.code(), Some(1));
    std::fs::remove_dir_all(&dir).unwrap();

    // Clean only: exit 0.
    let dir = std::env::temp_dir().join(format!(
        "xl-bench-verify-dir-{}-cli-clean",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fixture("clean_values.xlsx"), dir.join("clean.xlsx")).unwrap();
    let output = std::process::Command::new(bin)
        .arg("verify-dir")
        .arg(&dir)
        .arg("--quiet")
        .output()
        .expect("recalc binary runs");
    assert_eq!(output.status.code(), Some(0));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cli_verify_dir_streams_progress_unless_quiet() {
    let bin = env!("CARGO_BIN_EXE_recalc");
    let dir = make_corpus_dir("cli-progress");

    let output = std::process::Command::new(bin)
        .arg("verify-dir")
        .arg(&dir)
        .output()
        .expect("recalc binary runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("clean.xlsx"), "stdout: {stdout}");
    assert!(stdout.contains("LOAD FAILURE"), "stdout: {stdout}");
    assert!(stdout.contains("3/3 processed"), "stdout: {stdout}");

    let quiet_output = std::process::Command::new(bin)
        .arg("verify-dir")
        .arg(&dir)
        .arg("--quiet")
        .output()
        .expect("recalc binary runs");
    let quiet_stdout = String::from_utf8_lossy(&quiet_output.stdout);
    // `--quiet` suppresses the per-file streaming and the N/M heartbeat...
    assert!(
        !quiet_stdout.contains("clean.xlsx"),
        "--quiet must suppress per-file lines, got: {quiet_stdout}"
    );
    assert!(
        !quiet_stdout.contains("processed"),
        "--quiet must suppress the heartbeat, got: {quiet_stdout}"
    );
    // ...but still prints the single corpus-wide aggregate (the whole point of
    // a fast silent re-score is to get that one number).
    assert!(
        quiet_stdout.contains("CORPUS FIDELITY:"),
        "--quiet must still print the corpus aggregate, got: {quiet_stdout}"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cli_rejects_flag_like_values() {
    let bin = env!("CARGO_BIN_EXE_recalc");

    // `--html --quiet` must be exit 2 with a clear error, not a file named
    // `--quiet` and a silently-dropped quiet flag.
    let output = std::process::Command::new(bin)
        .arg("verify")
        .arg(fixture("clean_values.xlsx"))
        .arg("--html")
        .arg("--quiet")
        .output()
        .expect("recalc binary runs");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--html requires a value"),
        "stderr: {stderr}"
    );
    assert!(
        !Path::new("--quiet").exists(),
        "must not create a file named --quiet"
    );

    let output = std::process::Command::new(bin)
        .arg("verify-dir")
        .arg("some-dir")
        .arg("--html-dir")
        .arg("--quiet")
        .output()
        .expect("recalc binary runs");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--html-dir requires a value"),
        "stderr: {stderr}"
    );
}
