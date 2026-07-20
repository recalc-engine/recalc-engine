//! `recalc` — the `xl-bench` CLI: `recalc verify <book.xlsx>` and
//! `recalc verify-dir <dir>`.
//!
//! Hand-rolled argument parsing (no `clap` — not on the approved-dependency
//! list; the surface here is small enough that a parser dependency would be
//! more machinery than the problem needs).
//!
//! # Exit codes (`verify`)
//! - `0` — every formula cell is Exact/UlpDiff/NoOracle/EngineUnsupported —
//!   nothing is *wrong*, even if some cells are unscored or explicitly
//!   unsupported.
//! - `1` — at least one cell is a genuine [`xl_bench::diff::CellStatus::Mismatch`].
//! - `2` — the workbook failed to load/parse, or argument/IO errors.
//!
//! # Exit codes (`verify-dir`)
//! Mirrors `verify` across the whole corpus: `2` if any file failed to
//! load, else `1` if any loaded file has a mismatch, else `0`. A load
//! failure is treated as the more severe outcome (a mismatch was at least
//! *measured*; a load failure means the harness measured nothing for that
//! file at all) — this ordering is a judgment call, documented here rather
//! than left implicit.

#![forbid(unsafe_code)]

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use xl_bench::addr::a1_ref;
use xl_bench::cellhash::{Fnv128, dump_workbook};
use xl_bench::corpus::{discover_workbooks, verify_dir};
use xl_bench::decline::{DeclineTally, attribute_workbook};
use xl_bench::diff::DiffConfig;
use xl_bench::html::workbook_report_to_html;
use xl_bench::json::{CorpusEntryResult, workbook_report_to_json};
use xl_bench::mismatch::mine_dir;
use xl_bench::report::run_workbook;
use xl_bench::shared_residual::{
    SharedResidualTally, attribute_workbook as attribute_shared_residual,
};
use xl_bench::tier0::Tier0Cut;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("verify") => cmd_verify(&args[1..]),
        Some("verify-dir") => cmd_verify_dir(&args[1..]),
        Some("tier0") => cmd_tier0(&args[1..]),
        Some("decline-attribution") => cmd_decline_attribution(&args[1..]),
        Some("shared-residual") => cmd_shared_residual(&args[1..]),
        Some("mismatch-mine") => cmd_mismatch_mine(&args[1..]),
        Some("cell-hash") => cmd_cell_hash(&args[1..]),
        Some("--help" | "-h") => {
            print_usage();
            ExitCode::from(0)
        }
        Some(other) => {
            eprintln!("recalc: unknown subcommand {other:?}\n");
            print_usage();
            ExitCode::from(2)
        }
        None => {
            print_usage();
            ExitCode::from(2)
        }
    }
}

fn print_usage() {
    eprintln!(
        "Usage:\n  \
         recalc verify <book.xlsx> [--html out.html] [--json out.json] [--quiet]\n  \
         recalc verify-dir <dir> [--html-dir out/] [--tol=15sig] [--quiet]\n  \
         recalc tier0 <dir> [--top N] [--dump-mismatch FN1,FN2] [--dump-unsupported FN1,FN2] [--dump-n N] [--tol=15sig] [--quiet]   (INTERNAL Tier-0 fidelity cut)\n  \
         recalc decline-attribution <dir> [--top N] [--dump-cells FILE] [--quiet]   (root-cause classify every #UNSUPPORTED!/#BLOCKED!/#RESOURCE! cell; --dump-cells streams one workbook\\tsheet\\tA1\\tclass line per declined cell)\n  \
         recalc shared-residual <dir> [--top N] [--max-text N] [--quiet]   (dedup unparseable shared-formula MASTER texts blocking follow-on expansion — Lane A triage)\n  \
         recalc mismatch-mine <dir> [--tol=15sig] [--dump FILE] [--top N] [--fn-detail N] [--sample-n N] [--max-text N] [--quiet]   (decompose every Mismatch cell by function / type-transition / pattern; --dump streams per-cell TSV forensics)\n  \
         recalc cell-hash <dir> [--dump FILE] [--self-check] [--quiet]   (serial-vs-parallel sweep: bit-exact per-cell recalc fingerprint; build twice and diff — docs/parallel-sweep.md)\n  \
         (--tol=15sig: the ratified 15-sig-fig float scoring tolerance / M1-gate headline; default is the strict bit-exact floor)\n\n\
         Flag values may not start with `--` (a following flag is never\n\
         consumed as a value). If a flag is given more than once, the last\n\
         occurrence wins.\n"
    );
}

/// Fetches the value for `flag` at `args[i]`, rejecting a missing value or
/// one that looks like another flag — `recalc verify book.xlsx --html
/// --quiet` must be a hard error, not a silently-created file named
/// `--quiet` with the quiet flag dropped.
fn flag_value(args: &[String], i: usize, flag: &str) -> Result<PathBuf, String> {
    match args.get(i) {
        None => Err(format!("{flag} requires a value")),
        Some(v) if v.starts_with("--") => Err(format!(
            "{flag} requires a value, but got the flag-like argument {v:?} \
             (a value may not start with `--`)"
        )),
        Some(v) => Ok(PathBuf::from(v)),
    }
}

fn fmt_pct(p: Option<f64>) -> String {
    match p {
        Some(x) => format!("{x:.2}%"),
        None => "n/a".to_string(),
    }
}

struct VerifyArgs {
    input: PathBuf,
    html: Option<PathBuf>,
    json: Option<PathBuf>,
    quiet: bool,
}

fn parse_verify_args(args: &[String]) -> Result<VerifyArgs, String> {
    let mut input = None;
    let mut html = None;
    let mut json = None;
    let mut quiet = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--html" => {
                i += 1;
                html = Some(flag_value(args, i, "--html")?);
            }
            "--json" => {
                i += 1;
                json = Some(flag_value(args, i, "--json")?);
            }
            "--quiet" => quiet = true,
            other if input.is_none() && !other.starts_with("--") => {
                input = Some(PathBuf::from(other));
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
        i += 1;
    }
    let input = input.ok_or("missing <book.xlsx> argument")?;
    Ok(VerifyArgs {
        input,
        html,
        json,
        quiet,
    })
}

fn cmd_verify(args: &[String]) -> ExitCode {
    let parsed = match parse_verify_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("recalc verify: {e}");
            print_usage();
            return ExitCode::from(2);
        }
    };

    let report = match run_workbook(&parsed.input, DiffConfig::default()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("recalc verify: {e}");
            return ExitCode::from(2);
        }
    };

    if let Some(path) = &parsed.html
        && let Err(e) = std::fs::write(path, workbook_report_to_html(&report))
    {
        eprintln!(
            "recalc verify: failed to write HTML report to {}: {e}",
            path.display()
        );
        return ExitCode::from(2);
    }
    if let Some(path) = &parsed.json
        && let Err(e) = std::fs::write(path, workbook_report_to_json(&report))
    {
        eprintln!(
            "recalc verify: failed to write JSON report to {}: {e}",
            path.display()
        );
        return ExitCode::from(2);
    }

    if !parsed.quiet {
        let s = &report.summary;
        println!(
            "{}: {} formula cells — exact {}, ulp_diff {}, mismatch {}, unsupported {}, no_oracle {} — fidelity {} / strict {}",
            report.workbook_path,
            s.total_formula_cells,
            s.exact,
            s.ulp_diff,
            s.mismatch,
            s.engine_unsupported,
            s.no_oracle,
            fmt_pct(s.fidelity_pct()),
            fmt_pct(s.strict_fidelity_pct()),
        );
    }

    if report.summary.has_mismatch() {
        ExitCode::from(1)
    } else {
        ExitCode::from(0)
    }
}

struct VerifyDirArgs {
    dir: PathBuf,
    html_dir: Option<PathBuf>,
    quiet: bool,
    /// `--tol=15sig` (alias `--fuzzy`) sets `DiffConfig::fuzzy_15sig` — the
    /// ratified 15-significant-figure float scoring tolerance (TOLERANCES.md,
    /// the contract review checkpoint 2026-07-13). Off by default: the bare command reports the
    /// bit-exact conservative floor, so a plain `verify-dir` is unchanged.
    fuzzy: bool,
}

fn parse_verify_dir_args(args: &[String]) -> Result<VerifyDirArgs, String> {
    let mut dir = None;
    let mut html_dir = None;
    let mut quiet = false;
    let mut fuzzy = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--html-dir" => {
                i += 1;
                html_dir = Some(flag_value(args, i, "--html-dir")?);
            }
            "--quiet" => quiet = true,
            // The ratified 15-sig-fig float scoring tolerance (TOLERANCES.md);
            // `--fuzzy` is a back-compat alias. Default stays bit-exact.
            "--tol=15sig" | "--fuzzy" => fuzzy = true,
            other if dir.is_none() && !other.starts_with("--") => {
                dir = Some(PathBuf::from(other));
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
        i += 1;
    }
    let dir = dir.ok_or("missing <dir> argument")?;
    Ok(VerifyDirArgs {
        dir,
        html_dir,
        quiet,
        fuzzy,
    })
}

fn cmd_verify_dir(args: &[String]) -> ExitCode {
    let parsed = match parse_verify_dir_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("recalc verify-dir: {e}");
            print_usage();
            return ExitCode::from(2);
        }
    };

    // Stream a result line as each workbook completes (a corpus run over
    // thousands of files must not sit silent until the end), plus an
    // "N/M processed" heartbeat every 50 files. `--quiet` suppresses all of
    // it via a no-op callback.
    let quiet = parsed.quiet;
    let progress = move |done: usize, total: usize, entry: &CorpusEntryResult| {
        if quiet {
            return;
        }
        match entry {
            CorpusEntryResult::Ok(e) => {
                let s = &e.summary;
                println!(
                    "{}: {} cells — fidelity {} / strict {}",
                    e.workbook_path,
                    s.total_formula_cells,
                    fmt_pct(s.fidelity_pct()),
                    fmt_pct(s.strict_fidelity_pct()),
                );
            }
            CorpusEntryResult::LoadFailure { path, message } => {
                println!("{path}: LOAD FAILURE: {message}");
            }
        }
        if done.is_multiple_of(50) || done == total {
            println!("-- {done}/{total} processed");
        }
    };

    // Stamp the scoring mode so a corpus re-score is self-identifying (dual-
    // disclosure requirement — TOLERANCES.md), mirroring `tier0`.
    println!(
        "[SCORING MODE] {}",
        if parsed.fuzzy {
            "15-sig-fig float tolerance (TOLERANCES.md 'Global numeric comparison'; OXP-182) — the documented headline"
        } else {
            "bit-exact (strict conservative floor; pass --tol=15sig for the 15-sig-fig headline)"
        }
    );
    let diff_cfg = DiffConfig {
        fuzzy_15sig: parsed.fuzzy,
        ..DiffConfig::default()
    };
    let run = match verify_dir(&parsed.dir, diff_cfg, parsed.html_dir.as_deref(), progress) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("recalc verify-dir: {e}");
            return ExitCode::from(2);
        }
    };

    // Corpus-wide aggregate, folded from the per-workbook summaries. Printed
    // unconditionally (even under `--quiet`) so a re-score can run silent and
    // still report the single number that matters. Strict and lenient use the
    // same numerator/denominator definitions as `WorkbookSummary`, applied to
    // the corpus totals: strict = (exact+ulp)/(total-no_oracle); lenient =
    // (exact+ulp)/(total-unsupported-no_oracle).
    let (mut exact, mut ulp, mut mismatch, mut unsupported, mut no_oracle, mut total) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    for e in &run.entries {
        if let CorpusEntryResult::Ok(r) = e {
            let s = &r.summary;
            exact += s.exact;
            ulp += s.ulp_diff;
            mismatch += s.mismatch;
            unsupported += s.engine_unsupported;
            no_oracle += s.no_oracle;
            total += s.total_formula_cells;
        }
    }
    let strict_denom = total.saturating_sub(no_oracle);
    let lenient_denom = strict_denom.saturating_sub(unsupported);
    let pct = |num: usize, den: usize| {
        if den == 0 {
            "n/a".to_string()
        } else {
            format!("{:.3}%", 100.0 * num as f64 / den as f64)
        }
    };
    println!(
        "\nCORPUS TOTAL: {total} formula cells — exact {exact}, ulp {ulp}, mismatch {mismatch}, \
         unsupported {unsupported}, no_oracle {no_oracle}\n\
         CORPUS FIDELITY: strict {} (={}/{}) / lenient {} (={}/{})",
        pct(exact + ulp, strict_denom),
        exact + ulp,
        strict_denom,
        pct(exact + ulp, lenient_denom),
        exact + ulp,
        lenient_denom,
    );

    if !parsed.quiet {
        let load_failures = run
            .entries
            .iter()
            .filter(|e| matches!(e, CorpusEntryResult::LoadFailure { .. }))
            .count();
        let mismatched = run
            .entries
            .iter()
            .filter(|e| match e {
                CorpusEntryResult::Ok(r) => r.summary.has_mismatch(),
                CorpusEntryResult::LoadFailure { .. } => false,
            })
            .count();
        println!(
            "{} file(s): {} load failure(s), {} with mismatch(es)",
            run.entries.len(),
            load_failures,
            mismatched
        );
    }

    if run.has_load_failure() {
        ExitCode::from(2)
    } else if run.has_mismatch() {
        ExitCode::from(1)
    } else {
        ExitCode::from(0)
    }
}

struct Tier0Args {
    dir: PathBuf,
    top: usize,
    quiet: bool,
    dump_mismatch: Vec<String>,
    dump_unsupported: Vec<String>,
    dump_n: usize,
    /// `--tol=15sig` (alias `--fuzzy`) sets `DiffConfig::fuzzy_15sig` — the
    /// ratified 15-significant-figure float scoring tolerance (TOLERANCES.md,
    /// the contract review checkpoint 2026-07-13). Off by default: the bare command reports
    /// the strict (bit-exact) conservative floor.
    fuzzy: bool,
}

fn parse_tier0_args(args: &[String]) -> Result<Tier0Args, String> {
    let mut dir = None;
    let mut top = 30usize;
    let mut quiet = false;
    let mut dump_mismatch = Vec::new();
    let mut dump_unsupported = Vec::new();
    let mut dump_n = 40usize;
    let mut fuzzy = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--top" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or("--top requires a value")?
                    .parse::<usize>()
                    .map_err(|_| "--top requires a non-negative integer".to_string())?;
                top = v;
            }
            "--dump-mismatch" => {
                i += 1;
                let v = args.get(i).ok_or("--dump-mismatch requires a value")?;
                if v.starts_with("--") {
                    return Err("--dump-mismatch requires a comma-separated function list".into());
                }
                dump_mismatch = v
                    .split(',')
                    .map(|s| s.trim().to_ascii_uppercase())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "--dump-unsupported" => {
                i += 1;
                let v = args.get(i).ok_or("--dump-unsupported requires a value")?;
                if v.starts_with("--") {
                    return Err(
                        "--dump-unsupported requires a comma-separated function list".into(),
                    );
                }
                dump_unsupported = v
                    .split(',')
                    .map(|s| s.trim().to_ascii_uppercase())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "--dump-n" => {
                i += 1;
                dump_n = args
                    .get(i)
                    .ok_or("--dump-n requires a value")?
                    .parse::<usize>()
                    .map_err(|_| "--dump-n requires a non-negative integer".to_string())?;
            }
            "--quiet" => quiet = true,
            // The ratified 15-sig-fig float scoring tolerance (TOLERANCES.md).
            // `--fuzzy` is a back-compat alias.
            "--tol=15sig" | "--fuzzy" => fuzzy = true,
            other if dir.is_none() && !other.starts_with("--") => {
                dir = Some(PathBuf::from(other));
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
        i += 1;
    }
    let dir = dir.ok_or("missing <dir> argument")?;
    Ok(Tier0Args {
        dir,
        top,
        quiet,
        dump_mismatch,
        dump_unsupported,
        dump_n,
        fuzzy,
    })
}

/// `recalc tier0 <dir>` — the INTERNAL Tier-0 fidelity cut (see
/// [`xl_bench::tier0`]). Walks the corpus, scores each workbook via the same
/// `run_workbook` path as `verify`, and folds the per-cell records into the
/// Tier-0 slices. Prints the `[INTERNAL/T0]` report plus a machine-parseable
/// summary line. Always exits `0` on a completed run (this is a measurement,
/// not a pass/fail gate).
fn cmd_tier0(args: &[String]) -> ExitCode {
    let parsed = match parse_tier0_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("recalc tier0: {e}");
            print_usage();
            return ExitCode::from(2);
        }
    };

    let books = discover_workbooks(&parsed.dir);
    if books.is_empty() {
        eprintln!(
            "recalc tier0: no .xlsx/.xlsm workbooks under {}",
            parsed.dir.display()
        );
        return ExitCode::from(2);
    }

    let total = books.len();
    let mut cut = Tier0Cut {
        sample_fns: parsed.dump_mismatch.iter().cloned().collect(),
        unsup_sample_fns: parsed.dump_unsupported.iter().cloned().collect(),
        sample_cap: parsed.dump_n,
        ..Default::default()
    };
    let diff_cfg = DiffConfig {
        fuzzy_15sig: parsed.fuzzy,
        ..DiffConfig::default()
    };
    // Stamp the scoring mode so every printed number is self-identifying and
    // reproducible (dual-disclosure requirement — TOLERANCES.md).
    println!(
        "[SCORING MODE] {}",
        if parsed.fuzzy {
            "15-sig-fig float tolerance (TOLERANCES.md 'Global numeric comparison'; OXP-182) — the documented / M1-gate headline"
        } else {
            "bit-exact (strict conservative floor; pass --tol=15sig for the 15-sig-fig headline)"
        }
    );
    let mut load_failures = 0usize;
    for (idx, path) in books.iter().enumerate() {
        match run_workbook(path, diff_cfg) {
            Ok(report) => cut.fold_workbook(&report.cells, &path.display().to_string()),
            Err(e) => {
                load_failures += 1;
                if !parsed.quiet {
                    println!("{}: LOAD FAILURE: {e}", path.display());
                }
            }
        }
        let done = idx + 1;
        if !parsed.quiet && (done.is_multiple_of(50) || done == total) {
            println!("-- {done}/{total} processed");
        }
    }

    // The report + summary line print unconditionally (even under --quiet) so a
    // silent run still emits the numbers that matter.
    print!("{}", cut.render(parsed.top));
    println!("load failures (excluded from all cuts): {load_failures}");
    println!("{}", cut.summary_line());
    ExitCode::from(0)
}

struct DeclineArgs {
    dir: PathBuf,
    top: usize,
    /// Optional per-declined-cell TSV dump: one
    /// `workbook<TAB>sheet<TAB>A1<TAB>class` line per declined cell (the forensic
    /// artifact behind the external-ref decomposition — keep it out of the repo;
    /// it is derived from the proprietary oracle corpus).
    dump_cells: Option<PathBuf>,
    quiet: bool,
}

fn parse_decline_args(args: &[String]) -> Result<DeclineArgs, String> {
    let mut dir = None;
    let mut top = 15usize;
    let mut dump_cells = None;
    let mut quiet = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--top" => {
                i += 1;
                top = args
                    .get(i)
                    .ok_or("--top requires a value")?
                    .parse::<usize>()
                    .map_err(|_| "--top requires a non-negative integer".to_string())?;
            }
            "--dump-cells" => {
                i += 1;
                dump_cells = Some(flag_value(args, i, "--dump-cells")?);
            }
            "--quiet" => quiet = true,
            other if dir.is_none() && !other.starts_with("--") => {
                dir = Some(PathBuf::from(other));
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
        i += 1;
    }
    let dir = dir.ok_or("missing <dir> argument")?;
    Ok(DeclineArgs {
        dir,
        top,
        dump_cells,
        quiet,
    })
}

/// `recalc decline-attribution <dir>` — classify every declined
/// (`#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`) cell across the corpus into
/// exactly one of the eight root-cause classes (see [`xl_bench::decline`]).
/// Walks the corpus, recalcs each workbook via the same load/recalc path as
/// `verify`, and folds each workbook's attribution into the running tally.
/// Prints the report + machine-parseable summary line unconditionally (even
/// under `--quiet`). Always exits `0` on a completed run (this is a
/// measurement, not a pass/fail gate).
fn cmd_decline_attribution(args: &[String]) -> ExitCode {
    let parsed = match parse_decline_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("recalc decline-attribution: {e}");
            print_usage();
            return ExitCode::from(2);
        }
    };

    let books = discover_workbooks(&parsed.dir);
    if books.is_empty() {
        eprintln!(
            "recalc decline-attribution: no .xlsx/.xlsm workbooks under {}",
            parsed.dir.display()
        );
        return ExitCode::from(2);
    }

    // Optional per-declined-cell TSV dump (`--dump-cells`). One line per declined
    // cell: `workbook<TAB>sheet<TAB>A1<TAB>class`. Streams as we go so a 1M-cell
    // corpus never buffers in memory. Purely additive output — the tally, its
    // partition gate, and every printed count are byte-identical with or without
    // it (the classification is unchanged; we only render `result.classified`).
    let mut dump_writer = match &parsed.dump_cells {
        Some(p) => match File::create(p) {
            Ok(f) => Some(BufWriter::new(f)),
            Err(e) => {
                eprintln!(
                    "recalc decline-attribution: failed to create dump file {}: {e}",
                    p.display()
                );
                return ExitCode::from(2);
            }
        },
        None => None,
    };

    let total = books.len();
    let mut tally = DeclineTally::default();
    for (idx, path) in books.iter().enumerate() {
        match attribute_workbook(path) {
            Ok(result) => {
                if let Some(w) = dump_writer.as_mut() {
                    let wb = path.display();
                    for (cid, class) in &result.classified {
                        let sheet = result
                            .sheet_names
                            .get(cid.sheet.0 as usize)
                            .map(String::as_str)
                            .unwrap_or("?");
                        if let Err(e) = writeln!(
                            w,
                            "{wb}\t{sheet}\t{}\t{}",
                            a1_ref(cid.row, cid.col),
                            class.tag()
                        ) {
                            eprintln!("recalc decline-attribution: error writing dump file: {e}");
                            return ExitCode::from(2);
                        }
                    }
                }
                tally.fold(&result);
            }
            Err(e) => {
                tally.note_load_failure();
                if !parsed.quiet {
                    println!("{}: LOAD FAILURE: {e}", path.display());
                }
            }
        }
        let done = idx + 1;
        if !parsed.quiet && (done.is_multiple_of(50) || done == total) {
            println!("-- {done}/{total} processed");
        }
    }

    if let Some(mut w) = dump_writer
        && w.flush().is_err()
    {
        eprintln!(
            "recalc decline-attribution: error flushing the --dump-cells file (output may be incomplete)"
        );
        return ExitCode::from(2);
    }

    // The report + summary line print unconditionally (even under --quiet) so a
    // silent run still emits the numbers that matter.
    print!("{}", tally.render(parsed.top));
    println!("{}", tally.summary_line());

    // Real (release-safe) partition gate: the `debug_assert` inside
    // `attribute_cells` is compiled out of a release build, but this number
    // backs a PUBLISHED benchmark — so if the ten per-class counts do not sum to
    // the declined total, fail loudly (exit 2) rather than print a silently
    // non-partitioning table.
    let per_class_sum: usize = tally.per_class.iter().sum();
    if per_class_sum != tally.total_declined {
        eprintln!(
            "recalc decline-attribution: FATAL: per-class counts sum to {per_class_sum} but total \
             declined is {} — the classification is not a partition; refusing to report a \
             non-reconciling number",
            tally.total_declined
        );
        std::process::exit(2);
    }
    ExitCode::from(0)
}

struct SharedResidualArgs {
    dir: PathBuf,
    top: usize,
    /// Truncate each shown master formula to this many bytes (0 = full text).
    max_text: usize,
    quiet: bool,
}

fn parse_shared_residual_args(args: &[String]) -> Result<SharedResidualArgs, String> {
    let mut dir = None;
    let mut top = 200usize;
    let mut max_text = 240usize;
    let mut quiet = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--top" => {
                i += 1;
                top = args
                    .get(i)
                    .ok_or("--top requires a value")?
                    .parse::<usize>()
                    .map_err(|_| "--top requires a non-negative integer".to_string())?;
            }
            "--max-text" => {
                i += 1;
                max_text = args
                    .get(i)
                    .ok_or("--max-text requires a value")?
                    .parse::<usize>()
                    .map_err(|_| "--max-text requires a non-negative integer".to_string())?;
            }
            "--quiet" => quiet = true,
            other if dir.is_none() && !other.starts_with("--") => {
                dir = Some(PathBuf::from(other));
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
        i += 1;
    }
    let dir = dir.ok_or("missing <dir> argument")?;
    Ok(SharedResidualArgs {
        dir,
        top,
        max_text,
        quiet,
    })
}

/// `recalc shared-residual <dir>` — Lane A instrumentation. For every bodyless
/// `<f t="shared"/>` follow-on across the corpus, decide whether it stays
/// declined because its group master is *missing* (orphan) or *unparseable*,
/// and emit a deduplicated ranking of the failing master **formula texts** by
/// how many follow-on cells each blocks. Emits formula text only — never cell
/// values or workbook contents. Always exits `0` on a completed run (this is a
/// measurement, not a pass/fail gate).
fn cmd_shared_residual(args: &[String]) -> ExitCode {
    let parsed = match parse_shared_residual_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("recalc shared-residual: {e}");
            print_usage();
            return ExitCode::from(2);
        }
    };

    let books = discover_workbooks(&parsed.dir);
    if books.is_empty() {
        eprintln!(
            "recalc shared-residual: no .xlsx/.xlsm workbooks under {}",
            parsed.dir.display()
        );
        return ExitCode::from(2);
    }

    let total = books.len();
    let mut tally = SharedResidualTally::default();
    for (idx, path) in books.iter().enumerate() {
        match attribute_shared_residual(path) {
            Ok(result) => tally.fold(&result),
            Err(e) => {
                tally.note_load_failure();
                if !parsed.quiet {
                    println!("{}: LOAD FAILURE: {e}", path.display());
                }
            }
        }
        let done = idx + 1;
        if !parsed.quiet && (done.is_multiple_of(50) || done == total) {
            println!("-- {done}/{total} processed");
        }
    }

    // Report + summary line print unconditionally so a silent run still emits
    // the numbers that matter.
    print!("{}", tally.render(parsed.top, parsed.max_text));
    println!("{}", tally.summary_line());
    ExitCode::from(0)
}

struct MismatchMineArgs {
    dir: PathBuf,
    top: usize,
    fn_detail: usize,
    sample_n: usize,
    max_text: usize,
    dump: Option<PathBuf>,
    quiet: bool,
    /// `--tol=15sig` (alias `--fuzzy`): score under the ratified 15-significant-
    /// figure float tolerance (TOLERANCES.md). Off by default (bit-exact floor).
    /// For the headline mismatch set (the cells that survive at Excel's own
    /// storage resolution) pass `--tol=15sig`.
    fuzzy: bool,
}

fn parse_mismatch_mine_args(args: &[String]) -> Result<MismatchMineArgs, String> {
    let mut dir = None;
    let mut top = 30usize;
    let mut fn_detail = 12usize;
    let mut sample_n = 20usize;
    let mut max_text = 60usize;
    let mut dump = None;
    let mut quiet = false;
    let mut fuzzy = false;
    let mut i = 0;
    let parse_usize = |args: &[String], i: usize, flag: &str| -> Result<usize, String> {
        args.get(i)
            .ok_or(format!("{flag} requires a value"))?
            .parse::<usize>()
            .map_err(|_| format!("{flag} requires a non-negative integer"))
    };
    while i < args.len() {
        match args[i].as_str() {
            "--top" => {
                i += 1;
                top = parse_usize(args, i, "--top")?;
            }
            "--fn-detail" => {
                i += 1;
                fn_detail = parse_usize(args, i, "--fn-detail")?;
            }
            "--sample-n" => {
                i += 1;
                sample_n = parse_usize(args, i, "--sample-n")?;
            }
            "--max-text" => {
                i += 1;
                max_text = parse_usize(args, i, "--max-text")?;
            }
            "--dump" => {
                i += 1;
                dump = Some(flag_value(args, i, "--dump")?);
            }
            "--quiet" => quiet = true,
            "--tol=15sig" | "--fuzzy" => fuzzy = true,
            other if dir.is_none() && !other.starts_with("--") => {
                dir = Some(PathBuf::from(other));
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
        i += 1;
    }
    let dir = dir.ok_or("missing <dir> argument")?;
    Ok(MismatchMineArgs {
        dir,
        top,
        fn_detail,
        sample_n,
        max_text,
        dump,
        quiet,
        fuzzy,
    })
}

/// `recalc mismatch-mine <dir>` — corpus-wide decomposition of the genuine-
/// fidelity-failure set (see [`xl_bench::mismatch`]). Walks every workbook,
/// recalcs + diffs at the requested tolerance, and classifies every
/// `Mismatch` cell by function vocabulary, expected→actual type transition, and
/// named pattern. Prints the ranked report + machine summary line
/// unconditionally; `--dump FILE` additionally streams one TSV line per
/// mismatch cell (the forensic artifact — keep it out of the repo; it is
/// derived from the proprietary oracle corpus). Always exits `0` on a completed
/// run (a measurement, not a pass/fail gate).
fn cmd_mismatch_mine(args: &[String]) -> ExitCode {
    let parsed = match parse_mismatch_mine_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("recalc mismatch-mine: {e}");
            print_usage();
            return ExitCode::from(2);
        }
    };

    let mut dump_writer = match &parsed.dump {
        Some(p) => match File::create(p) {
            Ok(f) => Some(BufWriter::new(f)),
            Err(e) => {
                eprintln!(
                    "recalc mismatch-mine: failed to create dump file {}: {e}",
                    p.display()
                );
                return ExitCode::from(2);
            }
        },
        None => None,
    };

    // Stamp the scoring mode so every printed number is self-identifying and
    // reproducible (dual-disclosure requirement — TOLERANCES.md), mirroring
    // `tier0`/`verify-dir`.
    println!(
        "[SCORING MODE] {}",
        if parsed.fuzzy {
            "15-sig-fig float tolerance (TOLERANCES.md 'Global numeric comparison'; OXP-182) — the documented headline mismatch set"
        } else {
            "bit-exact (strict conservative floor; pass --tol=15sig for the 15-sig-fig headline set)"
        }
    );

    let cfg = DiffConfig {
        fuzzy_15sig: parsed.fuzzy,
        ..DiffConfig::default()
    };

    let quiet = parsed.quiet;
    let progress = |done: usize, total: usize, _rel: &str| {
        if !quiet && (done.is_multiple_of(50) || done == total) {
            println!("-- {done}/{total} processed");
        }
    };

    let tally = mine_dir(
        &parsed.dir,
        cfg,
        parsed.sample_n,
        parsed.max_text,
        &mut dump_writer,
        progress,
    );

    if let Some(mut w) = dump_writer
        && w.flush().is_err()
    {
        eprintln!(
            "recalc mismatch-mine: error flushing the --dump file (output may be incomplete)"
        );
        return ExitCode::from(2);
    }

    // Report + summary print unconditionally so a silent run still emits the
    // numbers that matter.
    print!("{}", tally.render(parsed.top, parsed.fn_detail));
    println!("{}", tally.summary_line());
    ExitCode::from(0)
}

struct CellHashArgs {
    dir: PathBuf,
    /// Optional full per-cell dump file (one `relpath\tsheet\tA1\ttoken` line per
    /// formula cell) — the authoritative cross-binary comparison artifact.
    dump: Option<PathBuf>,
    /// In-process parallel-vs-serial comparison (requires `--features parallel`).
    self_check: bool,
    quiet: bool,
}

fn parse_cell_hash_args(args: &[String]) -> Result<CellHashArgs, String> {
    let mut dir = None;
    let mut dump = None;
    let mut self_check = false;
    let mut quiet = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dump" => {
                i += 1;
                dump = Some(flag_value(args, i, "--dump")?);
            }
            "--self-check" => self_check = true,
            "--quiet" => quiet = true,
            other if dir.is_none() && !other.starts_with("--") => {
                dir = Some(PathBuf::from(other));
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
        i += 1;
    }
    let dir = dir.ok_or("missing <dir> argument")?;
    Ok(CellHashArgs {
        dir,
        dump,
        self_check,
        quiet,
    })
}

/// `recalc cell-hash <dir>` — Lane D serial-vs-parallel corpus sweep (the
/// deferred half of the rayon dependency policy, condition 4; see `docs/parallel-sweep.md`).
/// Recalculates every workbook with the running binary's executor (serial in a
/// default build; parallel-when-safe under `--features parallel`) and emits a
/// **canonical, bit-exact** per-cell fingerprint: a per-workbook 128-bit hash
/// line, a single corpus-level hash, and (with `--dump`) the full per-cell
/// stream. Build the CLI twice and diff the two outputs: byte-identical ⇒
/// per-cell serial/parallel bit-identity. `--self-check` (parallel build only)
/// additionally compares both executors **in-process** and reports how many
/// workbooks actually engaged the parallel path (non-vacuity). Exit `1` iff a
/// self-check divergence is found (a determinism bug — STOP), `2` on
/// argument/IO error, else `0`.
fn cmd_cell_hash(args: &[String]) -> ExitCode {
    let parsed = match parse_cell_hash_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("recalc cell-hash: {e}");
            print_usage();
            return ExitCode::from(2);
        }
    };

    // Stamp which executor this binary carries so the artifact self-identifies
    // (the whole point of the sweep is diffing an `on` run against an `off` run).
    let parallel_on = cfg!(feature = "parallel");
    println!(
        "[BUILD] parallel_feature={}",
        if parallel_on { "on" } else { "off" }
    );

    if parsed.self_check && !parallel_on {
        eprintln!(
            "recalc cell-hash: --self-check requires a `--features parallel` build \
             (it compares the parallel executor against the forced-serial one \
             in-process); rebuild with `cargo build -p xl-bench --features parallel`"
        );
        return ExitCode::from(2);
    }

    let books = discover_workbooks(&parsed.dir);
    if books.is_empty() {
        eprintln!(
            "recalc cell-hash: no .xlsx/.xlsm workbooks under {}",
            parsed.dir.display()
        );
        return ExitCode::from(2);
    }

    let mut dump_writer = match &parsed.dump {
        Some(p) => match File::create(p) {
            Ok(f) => Some(BufWriter::new(f)),
            Err(e) => {
                eprintln!(
                    "recalc cell-hash: failed to create dump file {}: {e}",
                    p.display()
                );
                return ExitCode::from(2);
            }
        },
        None => None,
    };

    let total = books.len();
    let mut corpus = Fnv128::new();
    let (mut books_ok, mut cells_total, mut load_failures) = (0usize, 0usize, 0usize);
    let mut dump_write_err = false;

    // Self-check accumulators — only exist in a parallel build.
    #[cfg(feature = "parallel")]
    let (mut sc_checked, mut sc_gate_open, mut sc_divergent_books, mut sc_divergent_cells) =
        (0usize, 0usize, 0usize, 0usize);
    #[cfg(feature = "parallel")]
    let mut sc_first: Option<String> = None;

    for (idx, path) in books.iter().enumerate() {
        let rel = path.display().to_string();
        match dump_workbook(path) {
            Ok(d) => {
                books_ok += 1;
                cells_total += d.n_cells();
                // Fold this workbook's identity + digest into the corpus digest.
                corpus.write(rel.as_bytes());
                corpus.write(&[0]);
                corpus.write(&d.hash.to_be_bytes());
                corpus.write(&[0x1e]);
                if !parsed.quiet {
                    println!("H {:032x} N {} {}", d.hash, d.n_cells(), rel);
                }
                if let Some(w) = dump_writer.as_mut() {
                    for (sheet, row, col, tok) in &d.cells {
                        if writeln!(w, "{rel}\t{sheet}\t{}\t{tok}", a1_ref(*row, *col)).is_err() {
                            dump_write_err = true;
                        }
                    }
                }
            }
            Err(e) => {
                load_failures += 1;
                // A load failure is folded distinctly so a workbook that loads in
                // one build but not the other flips the corpus digest.
                corpus.write(rel.as_bytes());
                corpus.write(b"\x00LOADFAIL\x1e");
                if !parsed.quiet {
                    println!("{rel}: LOAD FAILURE: {e}");
                }
            }
        }

        #[cfg(feature = "parallel")]
        if parsed.self_check {
            match xl_bench::cellhash::self_check_workbook(path) {
                Ok(sc) => {
                    sc_checked += 1;
                    if sc.gate_open {
                        sc_gate_open += 1;
                    }
                    if sc.divergent > 0 {
                        sc_divergent_books += 1;
                        sc_divergent_cells += sc.divergent;
                        if sc_first.is_none()
                            && let Some((sheet, cell, st, pt)) = sc.first_divergence
                        {
                            sc_first =
                                Some(format!("{rel} {sheet}!{cell} serial={st} parallel={pt}"));
                        }
                    }
                }
                Err(e) => {
                    // Load failures were already counted by the dump pass above;
                    // note only that the self-check could not run for this file.
                    if !parsed.quiet {
                        println!("{rel}: SELF-CHECK SKIPPED (load failure: {e})");
                    }
                }
            }
        }

        let done = idx + 1;
        if !parsed.quiet && (done.is_multiple_of(50) || done == total) {
            println!("-- {done}/{total} processed");
        }
    }

    if let Some(mut w) = dump_writer
        && w.flush().is_err()
    {
        dump_write_err = true;
    }
    if dump_write_err {
        eprintln!("recalc cell-hash: error writing the --dump file (output is incomplete)");
        return ExitCode::from(2);
    }

    // Corpus summary prints unconditionally (even under --quiet) — the corpus
    // hash is the single number the sweep compares between the two builds.
    println!(
        "\nCELL-HASH CORPUS: books_ok {books_ok} cells {cells_total} \
         load_failures {load_failures} corpus_hash {:032x}",
        corpus.finish()
    );

    #[cfg(feature = "parallel")]
    if parsed.self_check {
        println!(
            "SELF-CHECK: books_checked {sc_checked} parallel_gate_open {sc_gate_open} \
             divergent_books {sc_divergent_books} divergent_cells {sc_divergent_cells}"
        );
        if let Some(f) = &sc_first {
            println!("SELF-CHECK FIRST DIVERGENCE: {f}");
        }
        if sc_divergent_cells > 0 {
            eprintln!(
                "recalc cell-hash: FATAL: {sc_divergent_cells} cell(s) in {sc_divergent_books} \
                 workbook(s) diverge between the parallel and serial executors — a determinism \
                 bug (RFC-0014 / rayon condition 4). Do NOT ship the parallel feature."
            );
            return ExitCode::from(1);
        }
    }

    ExitCode::from(0)
}
