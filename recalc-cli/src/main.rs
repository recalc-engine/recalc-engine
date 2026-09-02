//! `recalc` — the standalone Recalc Verify command.
//!
//! One subcommand, `verify`, implementing the Verify v1 contract
//! (`docs/specs/recalc-verify-v1.md`): recalculate a workbook locally, label
//! every comparison by the evidence behind it, and return a stable exit code
//! a script or agent can act on. The internal conformance harness is a
//! separate binary (`recalc-bench`) and is deliberately not exposed here.
//!
//! # Exit codes
//! - `0` PASS — the policy is satisfied.
//! - `1` FAIL — a definite mismatch, formula error, or failed assertion.
//! - `2` FALLBACK — no safe decision (unsupported construct, missing
//!   evidence, load failure); route the workbook to a fallback.
//! - `64` USAGE — invalid arguments or policy; no decision was made.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use xl_bench::verify_cli::{parse_verify_args, run_v1};

const PROGRAM: &str = "recalc";

fn version_line() -> String {
    format!(
        "{PROGRAM} {} ({})",
        env!("CARGO_PKG_VERSION"),
        xl_bench::report::engine_meta().git_hash
    )
}

fn print_usage() {
    eprintln!(
        "Usage:\n  \
         recalc verify <book.xlsx> [--policy policy.toml] [--baseline baseline.xlsx]\n  \
                                [--excel-result result.xlsx --excel-build LABEL]\n  \
                                [--json report.json] [--quiet]\n  \
         recalc --version\n  \
         recalc --help\n\n\
         Exit codes: 0 PASS, 1 FAIL, 2 FALLBACK, 64 usage error.\n\
         The report written by --json follows recalc.verify.report/v1.\n\
         Flag values may not start with `--`; if a flag is repeated, the last\n\
         occurrence wins.\n"
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("verify") => match parse_verify_args(&args[1..]) {
            // `--html` is the harness's legacy report; the Verify v1 surface
            // has no HTML output, so accepting it silently would be a lie.
            Ok(parsed) if parsed.html.is_some() => {
                eprintln!("{PROGRAM} verify: unrecognized argument: --html\n");
                print_usage();
                ExitCode::from(64)
            }
            Ok(parsed) => ExitCode::from(run_v1(&parsed, PROGRAM)),
            Err(e) => {
                eprintln!("{PROGRAM} verify: {e}\n");
                print_usage();
                ExitCode::from(64)
            }
        },
        Some("--version" | "-V" | "version") => {
            println!("{}", version_line());
            ExitCode::from(0)
        }
        Some("--help" | "-h" | "help") => {
            print_usage();
            ExitCode::from(0)
        }
        Some(other) => {
            eprintln!("{PROGRAM}: unknown subcommand {other:?}\n");
            print_usage();
            ExitCode::from(64)
        }
        None => {
            print_usage();
            ExitCode::from(64)
        }
    }
}
