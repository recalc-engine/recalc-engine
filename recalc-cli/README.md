# recalc — Recalc Verify command-line tool

`recalc verify` recalculates an `.xlsx`/`.xlsm` workbook locally, compares
every formula cell with the evidence you choose, and returns a stable exit
code a script or agent can act on. It reads only the files you name, writes
only the report you ask for, makes no network calls, and uploads nothing.

Recalc is an open-source (Apache-2.0) spreadsheet calculation engine written
in Rust. Source, specification, and issues:
<https://github.com/recalc-engine/recalc-engine>.

## Install

Download the archive for your platform from the GitHub Releases page,
verify it, and unpack it. No runtime, no Rust toolchain, no installer.

```sh
# example: Linux x86_64 (see the release page for the other targets)
V=0.1.0
curl -LO https://github.com/recalc-engine/recalc-engine/releases/download/cli-v$V/recalc-v$V-x86_64-unknown-linux-musl.tar.gz
curl -LO https://github.com/recalc-engine/recalc-engine/releases/download/cli-v$V/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
tar -xzf recalc-v$V-x86_64-unknown-linux-musl.tar.gz
./recalc --version
```

Targets: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`
(static binaries; any glibc or musl distribution), `aarch64-apple-darwin`,
`x86_64-apple-darwin`, `x86_64-pc-windows-msvc` (`.zip`).

Every release carries build provenance. With the GitHub CLI installed:

```sh
gh attestation verify recalc-v$V-x86_64-unknown-linux-musl.tar.gz --repo recalc-engine/recalc-engine
```

Building from source is the contributor path:
`cargo run --release -p recalc-cli -- verify ...` from a clone of the
repository.

## First run

The archive contains `examples/` with a passing workbook, a stale one, and a
policy file (`examples/README.md` explains them):

```sh
./recalc verify examples/demo.xlsx --policy examples/recalc-policy.toml --json report.json
echo "exit=$?"     # 0
./recalc verify examples/demo-stale.xlsx --policy examples/recalc-policy.toml --json stale.json
echo "exit=$?"     # 1: two cells still carry values from before an input changed
```

## Usage

```
recalc verify <book.xlsx> [--policy policy.toml] [--baseline baseline.xlsx]
                          [--excel-result result.xlsx --excel-build LABEL]
                          [--json report.json] [--quiet]
recalc --version
recalc --help
```

| Exit code | Meaning |
|---|---|
| `0` PASS | The policy is satisfied. |
| `1` FAIL | A definite mismatch, formula error, or failed assertion. |
| `2` FALLBACK | No safe decision: an unsupported or blocked construct, missing evidence, or a file that does not load. Route the workbook to a fallback path. |
| `64` usage | Invalid arguments or policy; no decision was made. |

Comparison sources, one per run:

- **Stored values** (default): the cached values saved in the workbook.
  Agreement is labelled `matches_stored`. A stored value is what the last
  program that saved the file wrote; it is not an independent Excel result.
- **`--baseline`**: a second workbook you trust, recalculated locally.
  Labelled `matches_baseline`.
- **`--excel-result` + `--excel-build`**: a result workbook produced by an
  identified Excel build. Labelled `matches_supplied_excel`; the build label
  is recorded as you gave it, never inferred.

The JSON report follows the `recalc.verify.report/v1` schema: decision,
exit code, workbook and policy hashes, engine version and revision, per-cell
calculation outcome and evidence label, and an `issues` list with stable
codes. Schema and contract:
`docs/specs/recalc-verify-v1.md` in the repository.

## What it does not do

It does not render, chart, run macros, open other workbooks, or reach the
network. Constructs it cannot compute safely come back as explicit
refusals (`#UNSUPPORTED!`, `#BLOCKED!`) and a FALLBACK decision under the
default policy; it never substitutes a plausible value.

## License

Apache-2.0. See `LICENSE`. Microsoft and Excel are trademarks of the
Microsoft group of companies; Recalc is independent and not affiliated with
Microsoft.
