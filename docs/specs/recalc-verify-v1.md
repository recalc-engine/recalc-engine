# Recalc Verify v1 contract

**Status:** implementation specification; public CLI/report/policy surface
remains a founder checkpoint before a release. This document is normative for
the first implementation and is versioned independently from the calculation
engine. It follows the local-first verification product direction: independent
calculation, explicit evidence labels, and refusal over guessing.

## 1. Scope and promise

Recalc Verify evaluates a workbook locally after an agent or application edits
it. It recalculates with the selected Recalc runtime, records workbook and
formula features, evaluates declared comparison evidence and assertions, and
returns a machine-readable policy decision.

The verifier makes an independent calculation result available. It does not
claim that every attempted result is Excel-exact. A pinned-Excel claim is only
made when a supplied result identifies the Excel build and the receipt records
the comparison rule. Agreement with a workbook's cached value is explicitly a
stored-value comparison; caches can be stale and are not a pinned oracle.

The default data flow is local. The verifier does not upload workbooks,
formulas, values, diagnostics, or telemetry.

## 2. CLI

The canonical invocation is:

```sh
recalc verify OUTPUT.xlsx \
  --baseline INPUT.xlsx \
  --policy recalc-policy.toml \
  --json report.json
```

All paths are local filesystem paths. `OUTPUT.xlsx` is the candidate workbook
after an agent edit. `--baseline` is optional and provides a comparison
workbook; it is not itself an Excel oracle. `--excel-result` is optional and
accepts a customer- or farm-supplied workbook result whose `--excel-build`
string is mandatory. `--policy` is optional and otherwise uses the documented
safe defaults. `--json` is optional for a human run but is required in CI and
agent integrations. `--quiet` suppresses the human summary, not the report.

The CLI never overwrites an input workbook. It writes the report atomically to
the requested output path. If report writing fails, the process returns usage/
I/O exit code 64 and explains the failure on stderr.

## 3. Stable exit codes

The process returns exactly one of these codes:

| Code | Symbol | Meaning |
|---:|---|---|
| 0 | `PASS` | The policy is satisfied; no definite mismatch, formula error, or assertion failure remains, and no configured fallback condition occurred. |
| 1 | `FAIL` | A definite policy violation occurred: a comparison mismatch, formula error, or failed assertion. |
| 2 | `FALLBACK` | Recalc could not establish a safe decision: unsupported/blocked/resource-limited computation, unresolved external data, iterative/cyclic refusal, parse/load failure, missing required evidence, or an explicit policy fallback condition. |
| 64 | `USAGE` | Invalid arguments, malformed policy, incompatible report version, or report-output I/O failure. No verification decision is claimed. |

The decision precedence is `FAIL` > `FALLBACK` > `PASS` once the invocation is
valid. A definite failure is actionable even when another cell also requires a
fallback; the report retains every issue and the caller can choose to route the
whole workbook to Excel. A usage error is outside that decision state machine.

## 4. Policy file

The policy is UTF-8 TOML with the following v1 subset. Unknown keys and unknown
enum values are usage errors; duplicate scalar keys are usage errors. Strings
are quoted TOML basic strings. Arrays contain strings only.

```toml
policy_version = "recalc.verify.policy/v1"

# Safe defaults are shown explicitly. A fallback condition is never ignored.
on_unsupported = "fallback"       # fallback | fail
on_blocked = "fallback"            # fallback | fail
on_resource_limit = "fallback"     # fallback | fail
on_parse_error = "fallback"        # fallback | fail
on_formula_error = "fail"           # fail | fallback | allow
on_external_reference = "fallback" # fallback | fail | allow
on_vba_project = "fallback"        # fallback | fail | allow
require_comparison = false          # require at least one comparison source
require_excel_result = false        # require identified supplied Excel result
require_determinism = true          # missing seed/clock for volatile work is fallback
allow_stored_value_match = true     # cache agreement is evidence, never proof
allow_baseline_match = true

[[assertions]]
sheet = "Summary"
range = "B2"
operator = "not_error"             # not_error | equals_text | equals_number |
                                    # between_number | equals_bool | blank
value = ""                          # used by equals_* and between_number
upper = ""                          # used only by between_number

[[assertions]]
sheet = "Summary"
range = "B5:D5"                     # ranges are evaluated cell by cell
operator = "not_error"
```

Assertions are evaluated after calculation and before the final decision. A
range assertion applies to every cell in the range; one failing cell is one
assertion failure with a cell location. `equals_number` and `between_number`
use the policy's declared numeric comparison mode; they do not create an
Excel-fidelity claim. `value` is required for equality/bounds operators and
must parse as the declared type. `not_error` and `blank` omit `value`.

The implementation may add non-breaking optional keys in a later report
version, but it must not change the meaning of a v1 key. A policy's canonical
UTF-8 bytes (including normalized final newline) are hashed into the receipt.

## 5. Evidence and issue vocabulary

Every formula cell has one calculation outcome and zero or more independent
comparison outcomes. These labels are intentionally not the old benchmark
`exact` bucket:

### Calculation outcomes

- `recalc_computed` — the current runtime produced a value. This label makes no
  Excel-fidelity claim.
- `formula_error` — the computed value is an Excel error such as `#DIV/0!`;
  the error is reported even if a stored cache contains the same error.
- `unsupported` — Recalc declined a construct or function with
  `#UNSUPPORTED!`.
- `blocked` — sandbox policy declined external I/O with `#BLOCKED!`.
- `resource_limited` — a configured resource cap produced `#RESOURCE!` or a
  typed loader/resource failure.
- `parse_failed` — the workbook or formula could not be parsed.

### Comparison evidence

- `matches_stored` / `differs_stored` — comparison with the
  candidate workbook's cached `<v>` value. The receipt states that this cache
  is not a pinned Excel oracle.
- `matches_baseline` / `differs_baseline` — comparison with the selected
  baseline workbook under the declared baseline rule. It is not an Excel
  claim.
- `matches_supplied_excel` / `differs_supplied_excel` —
  comparison with a supplied result carrying an identified Excel build and
  source hash. This is the only v1 label that can support a pinned-Excel
  comparison statement.
- `assertion_passed` / `assertion_failed` — user invariant; never an Excel
  fidelity claim.
- `evidence_unavailable` — the requested comparison source lacks a value,
  matching cell, build metadata, or required shape.

The report records the calculation outcome before looking at evidence. A
missing cached value can therefore never hide `unsupported`, `blocked`,
`resource_limited`, or `formula_error` as `evidence_unavailable`.

## 6. Evaluation order

The verifier performs the following deterministic sequence:

1. Validate arguments and policy.
2. Read bytes and compute the candidate SHA-256 hash without changing them.
3. Load the workbook under `xl-io` hardening limits. Record typed parse,
   structure, and resource failures in the report when possible.
4. Inventory workbook flags, formulas, external references, VBA presence,
   dynamic-array/spill constructs, volatile functions, and iteration settings.
5. Recalculate using the runtime's deterministic context. Record diagnostics
   and calculation outcomes for every formula cell, including errors and
   sentinels even when no comparison value exists.
6. Evaluate stored-value, baseline, and supplied-Excel comparisons in stable
   sheet/row/column order.
7. Evaluate assertions in declaration order, then range cell order.
8. Apply policy to issues and evidence requirements using the precedence above.
9. Write the report and return the stable exit code.

No step consults the network or filesystem from a formula. The optional input
files are opened only by the host verifier.

## 7. Reproducibility receipt

Every report contains a receipt with:

- report schema and policy versions;
- Recalc package/runtime version and git revision;
- target triple, operating-system release, and evaluator mode;
- candidate, baseline, supplied-result, and policy SHA-256 hashes when present;
- declared seed and injectable clock values, or explicit `null`/`refused`;
- workbook date system, calculation mode, iteration settings, and feature flags;
- comparison rules, evidence source labels, identified Excel build/source when
  supplied, and per-source counts;
- stable ordered issue and cell records;
- final decision, exit-code symbol, and a self-check hash of the canonical
  report payload.

The receipt contains no workbook bytes. Paths are display-only metadata and are
not used as identity; hashes are the identity. Environment fields are limited
to reproducibility data and never include credentials or host filesystem dumps.

## 8. JSON schema and compatibility

The v1 schema is
[`recalc-verify-report-v1.schema.json`](recalc-verify-report-v1.schema.json).
The root `schema_version` is exactly `recalc.verify.report/v1`. Consumers must
branch on `decision`, `exit_code`, `calculation_outcome`, `evidence.label`, and
stable issue codes rather than human messages. A future incompatible change
uses `/v2`; v1 reports remain readable.

## 9. Safe defaults and known boundaries

The default policy falls back on unsupported, blocked, resource-limited,
parse-failed, unresolved external, VBA, and nondeterministic/iterative work.
It fails on formula errors, comparison mismatches, and assertion failures. It
does not silently enable seeded random or clock semantics that the engine does
not yet implement. A caller may explicitly choose `allow` for a known risk,
but the report still records it and the result is not promoted to Excel proof.

The v1 verifier does not write workbooks, execute VBA, fetch external links,
render sheets, or provide a hosted control plane. Python and Node integrations
consume the same JSON contract; they do not define a second policy language.
