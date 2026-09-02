//! Shared command-line front end for `recalc verify` (Verify v1).
//!
//! Both binaries — the standalone `recalc` CLI (`recalc-cli`) and the
//! internal harness binary (`recalc-bench`) — parse the same arguments and
//! run the same contract through this module, so the exit codes and report
//! shape cannot drift between them. Hand-rolled argument parsing (no `clap`:
//! the surface is small and the workspace carries no external CLI dependency).
//!
//! # Exit codes (`docs/specs/recalc-verify-v1.md` §3)
//! - `0` PASS, `1` FAIL, `2` FALLBACK — the verification decision;
//! - `64` USAGE — invalid arguments, malformed policy, or report-output I/O
//!   failure; no decision is claimed.

use std::path::{Path, PathBuf};

use crate::verify::{Decision, VerifyRun, load_policy, verify_workbook, verify_workbook_supplied};

/// Parsed `recalc verify` arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VerifyArgs {
    /// The candidate workbook.
    pub input: PathBuf,
    /// Legacy harness HTML report (ignored by the Verify v1 path).
    pub html: Option<PathBuf>,
    /// Where to write the JSON report.
    pub json: Option<PathBuf>,
    /// Policy file; defaults apply when absent.
    pub policy: Option<PathBuf>,
    /// Locally recalculated baseline workbook.
    pub baseline: Option<PathBuf>,
    /// Caller-supplied Excel result workbook (requires `excel_build`).
    pub excel_result: Option<PathBuf>,
    /// Identified Excel build label for `excel_result`.
    pub excel_build: Option<String>,
    /// Suppress the human summary (the report is still written).
    pub quiet: bool,
}

/// Fetches the value for `flag` at `args[i]`, rejecting a missing value or
/// one that looks like another flag — `recalc verify book.xlsx --json
/// --quiet` must be a hard error, not a silently-created file named
/// `--quiet` with the quiet flag dropped.
pub fn flag_value(args: &[String], i: usize, flag: &str) -> Result<PathBuf, String> {
    match args.get(i) {
        None => Err(format!("{flag} requires a value")),
        Some(v) if v.starts_with("--") => Err(format!(
            "{flag} requires a value, but got the flag-like argument {v:?} \
             (a value may not start with `--`)"
        )),
        Some(v) => Ok(PathBuf::from(v)),
    }
}

/// Parse the arguments after the `verify` subcommand. Later occurrences of a
/// flag win; flag values may not start with `--`.
pub fn parse_verify_args(args: &[String]) -> Result<VerifyArgs, String> {
    let mut parsed = VerifyArgs::default();
    let mut input = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--html" => {
                i += 1;
                parsed.html = Some(flag_value(args, i, "--html")?);
            }
            "--json" => {
                i += 1;
                parsed.json = Some(flag_value(args, i, "--json")?);
            }
            "--policy" => {
                i += 1;
                parsed.policy = Some(flag_value(args, i, "--policy")?);
            }
            "--baseline" => {
                i += 1;
                parsed.baseline = Some(flag_value(args, i, "--baseline")?);
            }
            "--excel-result" => {
                i += 1;
                parsed.excel_result = Some(flag_value(args, i, "--excel-result")?);
            }
            "--excel-build" => {
                i += 1;
                parsed.excel_build = Some(
                    flag_value(args, i, "--excel-build")?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            "--quiet" => parsed.quiet = true,
            other if input.is_none() && !other.starts_with("--") => {
                input = Some(PathBuf::from(other));
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
        i += 1;
    }
    parsed.input = input.ok_or("missing <book.xlsx> argument")?;
    Ok(parsed)
}

/// Write a machine report without ever leaving a partially truncated target.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.to_path_buf();
    let suffix = format!(".tmp-{}", std::process::id());
    let name = tmp
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("report.json");
    tmp.set_file_name(format!("{name}{suffix}"));
    {
        let mut file = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(tmp, path)
}

/// Run the Verify v1 contract for already-parsed arguments and return the
/// process exit code. Errors go to stderr prefixed with `{program} verify:`;
/// the human summary goes to stdout unless `quiet`.
#[must_use]
pub fn run_v1(parsed: &VerifyArgs, program: &str) -> u8 {
    if parsed.excel_result.is_some() != parsed.excel_build.is_some() {
        eprintln!("{program} verify: --excel-result and --excel-build must be supplied together");
        return 64;
    }
    if parsed.baseline.is_some() && parsed.excel_result.is_some() {
        eprintln!(
            "{program} verify: choose one comparison source: --baseline or --excel-result, not both"
        );
        return 64;
    }
    if let Err(e) = load_policy(parsed.policy.as_deref()) {
        eprintln!("{program} verify: invalid policy: {e}");
        return 64;
    }
    let result = if let (Some(excel_result), Some(excel_build)) =
        (&parsed.excel_result, &parsed.excel_build)
    {
        verify_workbook_supplied(
            &parsed.input,
            parsed.policy.as_deref(),
            excel_result,
            excel_build,
        )
    } else {
        verify_workbook(
            &parsed.input,
            parsed.policy.as_deref(),
            parsed.baseline.as_deref(),
        )
    };
    let run: VerifyRun = match result {
        Ok(run) => run,
        Err(e) => {
            eprintln!("{program} verify: {e}");
            return Decision::Fallback.exit_code();
        }
    };
    if let Some(json_path) = &parsed.json
        && let Err(e) = write_atomic(json_path, run.json.as_bytes())
    {
        eprintln!(
            "{program} verify: failed to write JSON report to {}: {e}",
            json_path.display()
        );
        return 64;
    }
    if !parsed.quiet {
        print!("{}", run.human_report(&parsed.input));
        match &parsed.json {
            Some(json_path) => println!("  report: {}", json_path.display()),
            None => println!("  add --json report.json to keep the machine-readable report"),
        }
    }
    run.decision.exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parser_accepts_every_documented_flag() {
        let parsed = parse_verify_args(&args(&[
            "book.xlsx",
            "--policy",
            "p.toml",
            "--baseline",
            "b.xlsx",
            "--excel-result",
            "e.xlsx",
            "--excel-build",
            "16.0.1",
            "--json",
            "r.json",
            "--quiet",
        ]))
        .unwrap();
        assert_eq!(parsed.input, PathBuf::from("book.xlsx"));
        assert_eq!(parsed.policy, Some(PathBuf::from("p.toml")));
        assert_eq!(parsed.baseline, Some(PathBuf::from("b.xlsx")));
        assert_eq!(parsed.excel_result, Some(PathBuf::from("e.xlsx")));
        assert_eq!(parsed.excel_build.as_deref(), Some("16.0.1"));
        assert_eq!(parsed.json, Some(PathBuf::from("r.json")));
        assert!(parsed.quiet);
    }

    #[test]
    fn parser_rejects_missing_input_and_flag_like_values() {
        assert!(
            parse_verify_args(&args(&["--quiet"]))
                .unwrap_err()
                .contains("missing <book.xlsx>")
        );
        assert!(
            parse_verify_args(&args(&["b.xlsx", "--json", "--quiet"]))
                .unwrap_err()
                .contains("--json requires a value")
        );
        assert!(
            parse_verify_args(&args(&["b.xlsx", "--nope"]))
                .unwrap_err()
                .contains("unrecognized argument")
        );
    }

    #[test]
    fn run_v1_returns_usage_for_half_supplied_excel_pair() {
        let parsed = VerifyArgs {
            input: PathBuf::from("tests/fixtures/clean_values.xlsx"),
            excel_result: Some(PathBuf::from("tests/fixtures/clean_values.xlsx")),
            quiet: true,
            ..VerifyArgs::default()
        };
        assert_eq!(run_v1(&parsed, "recalc"), 64);
    }

    #[test]
    fn run_v1_rejects_two_comparison_sources() {
        let parsed = VerifyArgs {
            input: PathBuf::from("tests/fixtures/clean_values.xlsx"),
            baseline: Some(PathBuf::from("tests/fixtures/clean_values.xlsx")),
            excel_result: Some(PathBuf::from("tests/fixtures/clean_values.xlsx")),
            excel_build: Some("16.0.1".to_string()),
            quiet: true,
            ..VerifyArgs::default()
        };
        assert_eq!(run_v1(&parsed, "recalc"), 64);
    }

    #[test]
    fn run_v1_decides_without_a_json_path() {
        let clean = VerifyArgs {
            input: PathBuf::from("tests/fixtures/clean_values.xlsx"),
            quiet: true,
            ..VerifyArgs::default()
        };
        assert_eq!(run_v1(&clean, "recalc"), 0);
        let poisoned = VerifyArgs {
            input: PathBuf::from("tests/fixtures/cached_values.xlsx"),
            quiet: true,
            ..VerifyArgs::default()
        };
        assert_eq!(run_v1(&poisoned, "recalc"), 1);
        let missing = VerifyArgs {
            input: PathBuf::from("tests/fixtures/does-not-exist.xlsx"),
            quiet: true,
            ..VerifyArgs::default()
        };
        assert_eq!(run_v1(&missing, "recalc"), 2);
    }
}
