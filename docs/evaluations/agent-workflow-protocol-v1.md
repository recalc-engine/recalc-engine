# Recalc Verify agent-workflow evaluation v1

This is a versioned, implementation-independent protocol for measuring
whether Verify changes an agent's spreadsheet outcomes. It is a test plan,
not evidence of a completed external evaluation. The evaluator must use a
licensed task set and record its task/corpus receipt before making a claim.

## Arms

For the same agent, model, prompt, task order, and workbook seed, run:

1. **No gate:** the agent edits and submits the workbook normally.
2. **Diagnostic retry:** run Verify; return its machine-readable diagnostics;
   permit one correction and retry.
3. **Hard gate + fallback:** run Verify; accept only `pass`, otherwise refuse
   the workbook or route it to the declared Excel fallback.

Randomization, model settings, task order, and retry budget are fixed in a
run manifest. The evaluator must retain raw per-task reports and hashes; do
not upload workbook contents to Recalc by default.

## Required metrics

- task success and formula-error rate;
- unsafe-output acceptance/refusal rate;
- corrections after a Verify diagnostic;
- `pass`, `fail`, and `fallback` proportions;
- fraction of tasks with no usable comparison evidence;
- added wall-clock latency and retry count;
- false refusals on tasks whose licensed oracle result is correct.

Report numerator, denominator, confidence method, and missing/invalid runs.
Do not claim causality from a single arm or from a benchmark score alone.

## Receipt

Each run records `protocol_version`, agent/model/runtime versions, task-set
hash, seed, arm, policy hash, Verify report hashes, decision counts, metric
denominators, and evaluator identity. The checked-in fixture
`docs/evaluations/agent-workflow-fixture-v1.json` exercises all three arms
with synthetic outcomes and is a schema/aggregation smoke test only.
