//! Recalc Verify v1 contract primitives.
//!
//! This module deliberately contains only dependency-free, typed policy
//! parsing and decision logic. The CLI pipeline is layered on top so the
//! machine-readable contract cannot accidentally inherit the legacy
//! cached-value report's lenient `NoOracle` semantics.
//!
//! Provenance: `docs/specs/recalc-verify-v1.md` and
//! `docs/specs/recalc-verify-report-v1.schema.json` (2026-09-02).

#![forbid(unsafe_code)]

use crate::diff::CellStatus;
use crate::hash::sha256_hex;
use crate::report::run_workbook;
use std::fmt;
use std::path::Path;

/// Stable Verify decision and process exit code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Evidence and assertions passed.
    Pass,
    /// A measured mismatch, formula error, or failed assertion occurred.
    Fail,
    /// The workbook or requested evidence could not be judged safely.
    Fallback,
}

/// Apply the contract's precedence rule (`FAIL` > `FALLBACK` > `PASS`).
#[must_use]
pub fn decide(fail: bool, fallback: bool) -> Decision {
    if fail {
        Decision::Fail
    } else if fallback {
        Decision::Fallback
    } else {
        Decision::Pass
    }
}

impl Decision {
    /// Contract exit code (`0`, `1`, or `2`).
    #[must_use]
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Fail => 1,
            Self::Fallback => 2,
        }
    }
}

/// Action taken when a safe-but-incomplete condition is encountered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyAction {
    /// Refuse the verification with exit code 2.
    Fallback,
    /// Turn the condition into a measured failure with exit code 1.
    Fail,
    /// Permit the condition without changing the decision (explicit opt-in).
    Allow,
}

impl PolicyAction {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "fallback" => Some(Self::Fallback),
            "fail" => Some(Self::Fail),
            "allow" => Some(Self::Allow),
            _ => None,
        }
    }
}

/// Strict, dependency-free subset of TOML accepted by Verify v1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub policy_version: String,
    pub on_unsupported: PolicyAction,
    pub on_blocked: PolicyAction,
    pub on_resource_limit: PolicyAction,
    pub on_parse_error: PolicyAction,
    pub on_formula_error: PolicyAction,
    pub on_external_reference: PolicyAction,
    pub on_vba_project: PolicyAction,
    pub require_comparison: bool,
    pub require_excel_result: bool,
    pub require_determinism: bool,
    pub allow_stored_value_match: bool,
    pub allow_baseline_match: bool,
    pub assertions: Vec<Assertion>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            policy_version: "recalc.verify.policy/v1".to_string(),
            on_unsupported: PolicyAction::Fallback,
            on_blocked: PolicyAction::Fallback,
            on_resource_limit: PolicyAction::Fallback,
            on_parse_error: PolicyAction::Fallback,
            on_formula_error: PolicyAction::Fail,
            on_external_reference: PolicyAction::Fallback,
            on_vba_project: PolicyAction::Fallback,
            require_comparison: false,
            require_excel_result: false,
            require_determinism: true,
            allow_stored_value_match: true,
            allow_baseline_match: true,
            assertions: Vec::new(),
        }
    }
}

/// A deterministic cell assertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assertion {
    pub sheet: String,
    pub range: String,
    pub operator: String,
    pub value: Option<String>,
    pub upper: Option<String>,
}

/// Policy parse failure. Unknown keys are rejected so a typo cannot silently
/// weaken a verification gate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyError(pub String);

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PolicyError {}

/// Read and parse a policy file, or return the canonical default policy.
pub fn load_policy(path: Option<&Path>) -> Result<(Policy, Vec<u8>), PolicyError> {
    let bytes = match path {
        Some(path) => std::fs::read(path)
            .map_err(|e| PolicyError(format!("failed to read policy {}: {e}", path.display())))?,
        None => canonical_default_policy().as_bytes().to_vec(),
    };
    let text =
        std::str::from_utf8(&bytes).map_err(|_| PolicyError("policy is not UTF-8".to_string()))?;
    Ok((parse_policy(text)?, bytes))
}

/// The exact bytes used when no policy file is supplied; this is hashed in a
/// receipt rather than represented by an ambiguous implicit default.
#[must_use]
pub fn canonical_default_policy() -> &'static str {
    "policy_version = \"recalc.verify.policy/v1\"\non_unsupported = \"fallback\"\non_blocked = \"fallback\"\non_resource_limit = \"fallback\"\non_parse_error = \"fallback\"\non_formula_error = \"fail\"\non_external_reference = \"fallback\"\non_vba_project = \"fallback\"\nrequire_comparison = false\nrequire_excel_result = false\nrequire_determinism = true\nallow_stored_value_match = true\nallow_baseline_match = true\n"
}

fn unquote(value: &str) -> Result<String, PolicyError> {
    let value = value.trim();
    if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
        return Err(PolicyError(format!(
            "expected quoted string, got {value:?}"
        )));
    }
    let inner = &value[1..value.len() - 1];
    if inner.contains('\\') {
        return Err(PolicyError(
            "policy strings may not contain escapes".to_string(),
        ));
    }
    Ok(inner.to_string())
}

/// Parse the intentionally small policy grammar documented by Verify v1.
pub fn parse_policy(text: &str) -> Result<Policy, PolicyError> {
    let mut policy = Policy::default();
    let mut seen = std::collections::BTreeSet::new();
    let mut current: Option<Assertion> = None;
    for (line_no, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line == "[[assertions]]" {
            if let Some(a) = current.take() {
                policy.assertions.push(a);
            }
            current = Some(Assertion {
                sheet: String::new(),
                range: String::new(),
                operator: String::new(),
                value: None,
                upper: None,
            });
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| PolicyError(format!("line {}: expected key = value", line_no + 1)))?;
        let key = key.trim();
        let value = value.trim();
        if let Some(assertion) = current.as_mut() {
            let duplicate = match key {
                "sheet" => !assertion.sheet.is_empty(),
                "range" => !assertion.range.is_empty(),
                "operator" => !assertion.operator.is_empty(),
                "value" => assertion.value.is_some(),
                "upper" => assertion.upper.is_some(),
                _ => false,
            };
            if duplicate {
                return Err(PolicyError(format!(
                    "line {}: duplicate assertion key {key:?}",
                    line_no + 1
                )));
            }
            match key {
                "sheet" => assertion.sheet = unquote(value)?,
                "range" => assertion.range = unquote(value)?,
                "operator" => assertion.operator = unquote(value)?,
                "value" => assertion.value = Some(unquote(value)?),
                "upper" => assertion.upper = Some(unquote(value)?),
                _ => {
                    return Err(PolicyError(format!(
                        "line {}: unknown assertion key {key:?}",
                        line_no + 1
                    )));
                }
            }
            continue;
        }
        if !seen.insert(key.to_string()) {
            return Err(PolicyError(format!(
                "line {}: duplicate key {key:?}",
                line_no + 1
            )));
        }
        match key {
            "policy_version" => {
                let version = unquote(value)?;
                if version != "recalc.verify.policy/v1" {
                    return Err(PolicyError(format!(
                        "line {}: unsupported policy_version",
                        line_no + 1
                    )));
                }
                policy.policy_version = version;
            }
            "on_unsupported"
            | "on_blocked"
            | "on_resource_limit"
            | "on_parse_error"
            | "on_formula_error"
            | "on_external_reference"
            | "on_vba_project" => {
                let action = PolicyAction::parse(&unquote(value)?)
                    .ok_or_else(|| PolicyError(format!("line {}: invalid action", line_no + 1)))?;
                match key {
                    "on_unsupported" => policy.on_unsupported = action,
                    "on_blocked" => policy.on_blocked = action,
                    "on_resource_limit" => policy.on_resource_limit = action,
                    "on_parse_error" => policy.on_parse_error = action,
                    "on_formula_error" => policy.on_formula_error = action,
                    "on_external_reference" => policy.on_external_reference = action,
                    "on_vba_project" => policy.on_vba_project = action,
                    _ => unreachable!(),
                }
            }
            "require_comparison"
            | "require_excel_result"
            | "require_determinism"
            | "allow_stored_value_match"
            | "allow_baseline_match" => {
                let parsed = match value {
                    "true" => true,
                    "false" => false,
                    _ => {
                        return Err(PolicyError(format!(
                            "line {}: expected true or false",
                            line_no + 1
                        )));
                    }
                };
                match key {
                    "require_comparison" => policy.require_comparison = parsed,
                    "require_excel_result" => policy.require_excel_result = parsed,
                    "require_determinism" => policy.require_determinism = parsed,
                    "allow_stored_value_match" => policy.allow_stored_value_match = parsed,
                    "allow_baseline_match" => policy.allow_baseline_match = parsed,
                    _ => unreachable!(),
                }
            }
            _ => {
                return Err(PolicyError(format!(
                    "line {}: unknown policy key {key:?}",
                    line_no + 1
                )));
            }
        }
    }
    if let Some(a) = current {
        policy.assertions.push(a);
    }
    for (i, a) in policy.assertions.iter().enumerate() {
        if a.sheet.is_empty() || a.range.is_empty() || a.operator.is_empty() {
            return Err(PolicyError(format!(
                "assertion {i} is missing sheet, range, or operator"
            )));
        }
        match a.operator.as_str() {
            "equals_number" | "equals_text" | "equals_bool" | "equals_error" | "not_error"
            | "between_number" | "blank" => {}
            _ => {
                return Err(PolicyError(format!(
                    "assertion {i} has unsupported operator"
                )));
            }
        }
        if matches!(
            a.operator.as_str(),
            "equals_number" | "equals_text" | "equals_bool" | "equals_error"
        ) && a.value.is_none()
        {
            return Err(PolicyError(format!("assertion {i} requires value")));
        }
        if a.operator == "between_number" && (a.value.is_none() || a.upper.is_none()) {
            return Err(PolicyError(format!(
                "assertion {i} requires value and upper"
            )));
        }
    }
    Ok(policy)
}

fn parse_a1_ref(text: &str) -> Option<(u32, u32)> {
    let split = text.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = text.split_at(split);
    if letters.is_empty() || digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut col = 0u32;
    for ch in letters.chars() {
        let upper = ch.to_ascii_uppercase();
        if !upper.is_ascii_uppercase() {
            return None;
        }
        col = col
            .checked_mul(26)?
            .checked_add((upper as u32) - ('A' as u32) + 1)?;
    }
    let row = digits.parse::<u32>().ok()?.checked_sub(1)?;
    Some((row, col.checked_sub(1)?))
}

fn assertion_cells(range: &str) -> Option<Vec<(u32, u32)>> {
    let (start, end) = range
        .split_once(':')
        .map_or((range, range), |(a, b)| (a, b));
    let (r1, c1) = parse_a1_ref(start)?;
    let (r2, c2) = parse_a1_ref(end)?;
    let mut cells = Vec::new();
    for row in r1.min(r2)..=r1.max(r2) {
        for col in c1.min(c2)..=c1.max(c2) {
            cells.push((row, col));
        }
    }
    Some(cells)
}

fn evaluate_assertions(
    path: &Path,
    assertions: &[Assertion],
) -> Result<Vec<(String, String, String)>, String> {
    if assertions.is_empty() {
        return Ok(Vec::new());
    }
    let workbook = xl_io::open(path).map_err(|e| e.to_string())?;
    let mut engine = xl_engine::Engine::load(workbook);
    engine.recalc();
    let mut failures = Vec::new();
    for assertion in assertions {
        let cells = assertion_cells(&assertion.range)
            .ok_or_else(|| format!("invalid assertion range {:?}", assertion.range))?;
        let sid = engine
            .sheet_id(&assertion.sheet)
            .ok_or_else(|| format!("unknown assertion sheet {:?}", assertion.sheet))?;
        for (row, col) in cells {
            let value = engine
                .value(sid, row, col)
                .cloned()
                .unwrap_or(xl_value::Value::Blank);
            let passed = match assertion.operator.as_str() {
                "not_error" => !matches!(value, xl_value::Value::Error(_)),
                "blank" => matches!(value, xl_value::Value::Blank),
                "equals_number" => assertion.value.as_deref().and_then(|s| s.parse::<f64>().ok()).is_some_and(|n| matches!(value, xl_value::Value::Number(v) if v == n)),
                "between_number" => assertion.value.as_deref().and_then(|s| s.parse::<f64>().ok()).zip(assertion.upper.as_deref().and_then(|s| s.parse::<f64>().ok())).is_some_and(|(lo, hi)| matches!(value, xl_value::Value::Number(v) if v >= lo && v <= hi)),
                "equals_text" => matches!((&value, assertion.value.as_deref()), (xl_value::Value::Text(v), Some(expected)) if v.as_str() == expected),
                "equals_bool" => assertion.value.as_deref().and_then(|s| s.parse::<bool>().ok()).is_some_and(|b| matches!(value, xl_value::Value::Bool(v) if v == b)),
                "equals_error" => matches!((&value, assertion.value.as_deref()), (xl_value::Value::Error(v), Some(expected)) if v.as_str() == expected),
                _ => false,
            };
            if !passed {
                failures.push((
                    assertion.sheet.clone(),
                    crate::addr::a1_ref(row, col),
                    assertion.operator.clone(),
                ));
            }
        }
    }
    Ok(failures)
}

/// Run the dependency-free cached-value Verify path and return a v1 JSON
/// report. Baseline and supplied-Excel comparisons are intentionally handled
/// by the CLI as separate evidence sources; this path never relabels cached
/// evidence as an Excel-oracle result.
pub fn run_cached_verify(
    path: &Path,
    policy_path: Option<&Path>,
) -> Result<(String, Decision), String> {
    run_verify_v1(path, policy_path, None)
}

/// Run Verify v1 with an optional locally recalculated baseline workbook.
pub fn run_verify_v1(
    path: &Path,
    policy_path: Option<&Path>,
    baseline_path: Option<&Path>,
) -> Result<(String, Decision), String> {
    run_verify_internal(path, policy_path, baseline_path, false)
}

fn computed_values(
    path: &Path,
) -> Result<std::collections::BTreeMap<(String, u32, u32), xl_value::Value>, String> {
    let workbook = xl_io::open(path).map_err(|e| e.to_string())?;
    let names: Vec<(String, u32, u32)> = workbook
        .sheets
        .iter()
        .flat_map(|sheet| {
            sheet.cells.iter().filter_map(|(&(row, col), cell)| {
                cell.formula
                    .as_ref()
                    .map(|_| (sheet.name.clone(), row, col))
            })
        })
        .collect();
    let mut engine = xl_engine::Engine::load(workbook);
    engine.recalc();
    let mut values = std::collections::BTreeMap::new();
    for (sheet, row, col) in names {
        if let Some(sid) = engine.sheet_id(&sheet) {
            values.insert(
                (sheet, row, col),
                engine
                    .value(sid, row, col)
                    .cloned()
                    .unwrap_or(xl_value::Value::Blank),
            );
        }
    }
    Ok(values)
}

fn cached_values(
    path: &Path,
) -> Result<std::collections::BTreeMap<(String, u32, u32), xl_value::Value>, String> {
    let workbook = xl_io::open(path).map_err(|e| e.to_string())?;
    let mut values = std::collections::BTreeMap::new();
    for sheet in workbook.sheets {
        let sheet_name = sheet.name.clone();
        for ((row, col), cell) in sheet.cells {
            if cell.formula.is_some() {
                values.insert((sheet_name.clone(), row, col), cell.value);
            }
        }
    }
    Ok(values)
}

fn values_equal(a: &xl_value::Value, b: &xl_value::Value) -> bool {
    match (a, b) {
        (xl_value::Value::Number(a), xl_value::Value::Number(b)) => a == b,
        (xl_value::Value::Text(a), xl_value::Value::Text(b)) => a.as_str() == b.as_str(),
        (xl_value::Value::Bool(a), xl_value::Value::Bool(b)) => a == b,
        (xl_value::Value::Error(a), xl_value::Value::Error(b)) => a == b,
        (xl_value::Value::Blank, xl_value::Value::Blank) => true,
        _ => false,
    }
}

fn has_external_reference(formula: &str) -> bool {
    let mut quoted = false;
    let mut chars = formula.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            if quoted && chars.peek() == Some(&'"') {
                chars.next();
                continue;
            }
            quoted = !quoted;
        } else if ch == '[' && !quoted {
            return true;
        }
    }
    false
}

fn has_volatile_function(formula: &str) -> bool {
    let upper = formula.to_ascii_uppercase();
    [
        "NOW(",
        "TODAY(",
        "RAND(",
        "RANDBETWEEN(",
        "RANDARRAY(",
        "OFFSET(",
        "INDIRECT(",
        "CELL(",
        "INFO(",
    ]
    .iter()
    .any(|name| upper.contains(name))
}

/// Internal Verify runner; `baseline_cached` distinguishes a local recalculated
/// baseline from a supplied Excel-result workbook's stored values.
fn run_verify_internal(
    path: &Path,
    policy_path: Option<&Path>,
    baseline_path: Option<&Path>,
    baseline_cached: bool,
) -> Result<(String, Decision), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("failed to read workbook: {e}"))?;
    let baseline_bytes = baseline_path
        .map(std::fs::read)
        .transpose()
        .map_err(|e| format!("failed to read baseline: {e}"))?;
    let (policy, policy_bytes) = load_policy(policy_path).map_err(|e| e.to_string())?;
    let report = run_workbook(path, Default::default()).map_err(|e| e.to_string())?;
    let baseline_values = baseline_path
        .map(|p| {
            if baseline_cached {
                cached_values(p)
            } else {
                computed_values(p)
            }
        })
        .transpose()?;
    let candidate_values = Some(computed_values(path)?);
    let mut baseline_mismatches = std::collections::BTreeSet::new();
    let mut baseline_missing = std::collections::BTreeSet::new();
    if let (Some(candidate), Some(baseline)) = (&candidate_values, &baseline_values) {
        for c in &report.cells {
            let key = (c.sheet.clone(), c.row, c.col);
            match (candidate.get(&key), baseline.get(&key)) {
                (Some(a), Some(b)) if !values_equal(a, b) => {
                    baseline_mismatches.insert(key);
                }
                (Some(_), None) => {
                    baseline_missing.insert(key);
                }
                _ => {}
            }
        }
    }
    let mut fail = if baseline_path.is_some() {
        !baseline_mismatches.is_empty()
    } else {
        report.summary.mismatch > 0
    };
    let mut fallback = false;
    let mut preflight_issues = Vec::new();
    if policy.require_comparison {
        let has_comparison = baseline_values
            .as_ref()
            .is_some_and(|values| !values.is_empty())
            || (baseline_path.is_none()
                && policy.allow_stored_value_match
                && report.summary.no_oracle < report.summary.total_formula_cells);
        if !has_comparison {
            fallback = true;
            preflight_issues.push("{\"code\":\"comparison_required\",\"severity\":\"warning\",\"message\":\"policy requires a usable comparison source\"}".to_string());
        }
    }
    let external_reference = report
        .cells
        .iter()
        .any(|c| has_external_reference(&c.formula));
    let volatile_formula = report
        .cells
        .iter()
        .any(|c| has_volatile_function(&c.formula));
    if policy.require_determinism && volatile_formula {
        fallback = true;
        preflight_issues.push("{\"code\":\"determinism_unavailable\",\"severity\":\"warning\",\"message\":\"workbook contains volatile formulas without injected clock or RNG\"}".to_string());
    }
    for (condition, action, code, message) in [
        (
            external_reference,
            policy.on_external_reference,
            "external_reference",
            "formula contains an external workbook reference",
        ),
        (
            report.flags.has_vba_project,
            policy.on_vba_project,
            "vba_project",
            "workbook contains a VBA project; execution is disabled",
        ),
    ] {
        if condition {
            match action {
                PolicyAction::Fail => {
                    fail = true;
                }
                PolicyAction::Fallback => {
                    fallback = true;
                }
                PolicyAction::Allow => {}
            }
            preflight_issues.push(format!(
                "{{\"code\":{},\"severity\":\"warning\",\"message\":{}}}",
                crate::json::escape_str(code),
                crate::json::escape_str(message)
            ));
        }
    }
    if !baseline_missing.is_empty() {
        fallback = true;
        preflight_issues.push(format!(
            "{{\"code\":\"baseline_cell_unavailable\",\"severity\":\"warning\",\"message\":{} }}",
            crate::json::escape_str("baseline lacks one or more candidate formula cells")
        ));
    }
    if !policy.allow_stored_value_match {
        fallback = true;
        preflight_issues.push("{\"code\":\"stored_evidence_disabled\",\"severity\":\"warning\",\"message\":\"policy disallows stored cached-value evidence\"}".to_string());
    }
    if policy.require_excel_result && (!baseline_cached || baseline_path.is_none()) {
        fallback = true;
        preflight_issues.push("{\"code\":\"excel_evidence_required\",\"severity\":\"warning\",\"message\":\"policy requires a supplied Excel result\"}".to_string());
    }
    if baseline_path.is_some() && !baseline_cached && !policy.allow_baseline_match {
        fallback = true;
        preflight_issues.push("{\"code\":\"baseline_evidence_disabled\",\"severity\":\"warning\",\"message\":\"policy disallows local baseline evidence\"}".to_string());
    }
    let assertion_failures = match evaluate_assertions(path, &policy.assertions) {
        Ok(failures) => failures,
        Err(message) => {
            fallback = true;
            preflight_issues.push(format!("{{\"code\":\"assertion_evaluation_failed\",\"severity\":\"warning\",\"message\":{}}}", crate::json::escape_str(&message)));
            Vec::new()
        }
    };
    if !assertion_failures.is_empty() {
        fail = true;
    }
    if policy.require_comparison
        && (report.summary.engine_unsupported > 0 || report.summary.no_oracle > 0)
    {
        match policy.on_unsupported {
            PolicyAction::Fail => fail = true,
            PolicyAction::Fallback => fallback = true,
            PolicyAction::Allow => {}
        }
    }
    let computed_formula_errors = candidate_values
        .as_ref()
        .map(|values| {
            values
                .values()
                .filter(|v| matches!(v, xl_value::Value::Error(e) if !e.is_recalc_sentinel()))
                .count()
        })
        .unwrap_or(0);
    if computed_formula_errors > 0 {
        match policy.on_formula_error {
            PolicyAction::Fail => fail = true,
            PolicyAction::Fallback => fallback = true,
            PolicyAction::Allow => {}
        }
    }
    let (computed_unsupported, computed_blocked, computed_resource) = candidate_values
        .as_ref()
        .map(|values| {
            values
                .values()
                .fold((0usize, 0usize, 0usize), |(u, b, r), value| match value {
                    xl_value::Value::Error(xl_value::ErrorKind::Unsupported) => (u + 1, b, r),
                    xl_value::Value::Error(xl_value::ErrorKind::Blocked) => (u, b + 1, r),
                    xl_value::Value::Error(xl_value::ErrorKind::Resource) => (u, b, r + 1),
                    _ => (u, b, r),
                })
        })
        .unwrap_or_default();
    for (count, action) in [
        (computed_unsupported, policy.on_unsupported),
        (computed_blocked, policy.on_blocked),
        (computed_resource, policy.on_resource_limit),
    ] {
        if count > 0 {
            match action {
                PolicyAction::Fail => fail = true,
                PolicyAction::Fallback => fallback = true,
                PolicyAction::Allow => {}
            }
        }
    }
    let decision = decide(fail, fallback);
    let candidate_hash = sha256_hex(&bytes);
    let policy_hash = sha256_hex(&policy_bytes);
    let mut payload = String::new();
    payload.push_str("{\"schema_version\":\"recalc.verify.report/v1\",\"decision\":");
    payload.push_str(match decision {
        Decision::Pass => "\"pass\"",
        Decision::Fail => "\"fail\"",
        Decision::Fallback => "\"fallback\"",
    });
    payload.push_str(&format!(",\"exit_code\":{},\"workbook\":{{\"path\":{},\"sha256\":{},\"formula_cells\":{},\"flags\":{{\"has_vba_project\":{},\"date_system_1904\":{},\"calc_mode\":{}}}}},", decision.exit_code(), crate::json::escape_str(&path.display().to_string()), crate::json::escape_str(&candidate_hash), report.summary.total_formula_cells, report.flags.has_vba_project, report.flags.date_system_1904, crate::json::escape_str(report.flags.calc_mode)));
    payload.push_str(&format!(
        "\"engine\":{{\"version\":{},\"git_revision\":{},\"target\":{},\"os\":{}}},",
        crate::json::escape_str(&report.engine.version),
        crate::json::escape_str(&report.engine.git_hash),
        crate::json::escape_str(std::env::consts::ARCH),
        crate::json::escape_str(std::env::consts::OS)
    ));
    let baseline_hash = baseline_bytes.as_deref().map(sha256_hex);
    let comparison_source = if baseline_path.is_some() {
        "baseline"
    } else {
        "cached_value"
    };
    payload.push_str(&format!("\"receipt\":{{\"candidate_sha256\":{},\"baseline_sha256\":{},\"supplied_excel_result_sha256\":null,\"policy_sha256\":{},\"comparison_rules\":[{}],\"determinism\":{{\"seed\":null,\"clock\":null}},\"canonical_payload_sha256\":\"__PAYLOAD_HASH__\",\"excel_build\":null}},", crate::json::escape_str(&candidate_hash), baseline_hash.as_deref().map(crate::json::escape_str).unwrap_or_else(|| "null".to_string()), crate::json::escape_str(&policy_hash), crate::json::escape_str(comparison_source)));
    let s = &report.summary;
    let mut formula_errors = 0usize;
    let mut unsupported_count = 0usize;
    let mut blocked_count = 0usize;
    let mut resource_count = 0usize;
    for c in &report.cells {
        if let Some(xl_value::Value::Error(error)) = candidate_values
            .as_ref()
            .and_then(|v| v.get(&(c.sheet.clone(), c.row, c.col)))
        {
            match error {
                xl_value::ErrorKind::Unsupported => unsupported_count += 1,
                xl_value::ErrorKind::Blocked => blocked_count += 1,
                xl_value::ErrorKind::Resource => resource_count += 1,
                _ => formula_errors += 1,
            }
        }
    }
    // A refusal may be represented by a missing engine value in a legacy
    // report; retain its count rather than under-reporting the refusal.
    unsupported_count = unsupported_count.max(
        s.engine_unsupported
            .saturating_sub(blocked_count + resource_count),
    );
    let comparison_mismatches = if baseline_path.is_some() {
        baseline_mismatches.len()
    } else {
        s.mismatch
    };
    payload.push_str(&format!("\"summary\":{{\"formula_cells\":{},\"recalc_computed\":{},\"formula_errors\":{},\"unsupported\":{},\"blocked\":{},\"resource_limited\":{},\"mismatches\":{},\"assertion_failures\":{},\"evidence_counts\":{{{}:{}}}}},", s.total_formula_cells, s.total_formula_cells.saturating_sub(unsupported_count + blocked_count + resource_count), formula_errors, unsupported_count, blocked_count, resource_count, comparison_mismatches, assertion_failures.len(), crate::json::escape_str(comparison_source), s.total_formula_cells.saturating_sub(s.no_oracle)));
    payload.push_str("\"cells\":[");
    let mut issues = preflight_issues;
    for (sheet, cell_ref, operator) in &assertion_failures {
        issues.push(format!("{{\"code\":\"assertion_failed\",\"severity\":\"error\",\"message\":{},\"sheet\":{},\"ref\":{}}}", crate::json::escape_str(&format!("assertion {operator} failed")), crate::json::escape_str(sheet), crate::json::escape_str(cell_ref)));
    }
    for (i, c) in report.cells.iter().enumerate() {
        if i > 0 {
            payload.push(',');
        }
        let actual = candidate_values
            .as_ref()
            .and_then(|v| v.get(&(c.sheet.clone(), c.row, c.col)));
        let outcome = match actual {
            Some(xl_value::Value::Error(xl_value::ErrorKind::Blocked)) => "blocked",
            Some(xl_value::Value::Error(xl_value::ErrorKind::Resource)) => "resource_limited",
            Some(xl_value::Value::Error(xl_value::ErrorKind::Unsupported)) => "unsupported",
            Some(xl_value::Value::Error(_)) => "formula_error",
            _ => match &c.status {
                CellStatus::EngineUnsupported => "unsupported",
                _ => "recalc_computed",
            },
        };
        let (evidence, source) = if baseline_path.is_some() {
            let key = (c.sheet.clone(), c.row, c.col);
            if baseline_missing.contains(&key) {
                ("evidence_unavailable", "baseline")
            } else if baseline_mismatches.contains(&key) {
                ("differs_baseline", "baseline")
            } else {
                ("matches_baseline", "baseline")
            }
        } else {
            match &c.status {
                CellStatus::NoOracle => ("evidence_unavailable", "cached_value"),
                CellStatus::Mismatch { .. } => ("differs_stored", "cached_value"),
                _ => ("matches_stored", "cached_value"),
            }
        };
        let baseline_key = (c.sheet.clone(), c.row, c.col);
        if baseline_path.is_some() && baseline_mismatches.contains(&baseline_key) {
            issues.push(format!("{{\"code\":\"baseline_mismatch\",\"severity\":\"error\",\"message\":\"computed value differs from baseline\",\"sheet\":{},\"ref\":{}}}", crate::json::escape_str(&c.sheet), crate::json::escape_str(&c.cell_ref)));
        } else if matches!(&c.status, CellStatus::EngineUnsupported) {
            issues.push(format!("{{\"code\":\"unsupported\",\"severity\":\"warning\",\"message\":\"engine refused this cell\",\"sheet\":{},\"ref\":{}}}", crate::json::escape_str(&c.sheet), crate::json::escape_str(&c.cell_ref)));
        } else if matches!(&c.status, CellStatus::NoOracle) {
            issues.push(format!("{{\"code\":\"evidence_unavailable\",\"severity\":\"warning\",\"message\":\"stored cached value is unavailable\",\"sheet\":{},\"ref\":{}}}", crate::json::escape_str(&c.sheet), crate::json::escape_str(&c.cell_ref)));
        } else if baseline_path.is_none() && matches!(&c.status, CellStatus::Mismatch { .. }) {
            issues.push(format!("{{\"code\":\"mismatch\",\"severity\":\"error\",\"message\":\"computed value differs from stored cached value\",\"sheet\":{},\"ref\":{}}}", crate::json::escape_str(&c.sheet), crate::json::escape_str(&c.cell_ref)));
        }
        payload.push_str(&format!("{{\"sheet\":{},\"ref\":{},\"row\":{},\"col\":{},\"formula\":{},\"calculation_outcome\":{},\"evidence\":[{{\"label\":{},\"source\":{}}}]}}", crate::json::escape_str(&c.sheet), crate::json::escape_str(&c.cell_ref), c.row, c.col, crate::json::escape_str(&c.formula), crate::json::escape_str(outcome), crate::json::escape_str(evidence), crate::json::escape_str(source)));
    }
    payload.push_str("],\"issues\":[");
    payload.push_str(&issues.join(","));
    payload.push_str("]}");
    // Hash the complete canonical JSON shape with a fixed placeholder for the
    // hash field itself, then replace only that placeholder. This binds the
    // receipt to summary, cells, and issues rather than a short prefix.
    let payload_hash = sha256_hex(payload.as_bytes());
    payload = payload.replace("__PAYLOAD_HASH__", &payload_hash);
    Ok((payload, decision))
}

/// Compare against a caller-supplied, explicitly identified Excel result
/// workbook. The file is treated as evidence only; the build label is carried
/// into the receipt and never inferred.
pub fn run_supplied_excel_verify(
    path: &Path,
    policy_path: Option<&Path>,
    excel_result_path: &Path,
    excel_build: &str,
) -> Result<(String, Decision), String> {
    if excel_build.trim().is_empty() {
        return Err("--excel-build is required with --excel-result".to_string());
    }
    let (mut json, decision) =
        run_verify_internal(path, policy_path, Some(excel_result_path), true)?;
    let marker = "\"canonical_payload_sha256\":\"";
    let start = json
        .find(marker)
        .ok_or("Verify report missing payload hash")?
        + marker.len();
    let end = json[start..]
        .find('"')
        .ok_or("Verify report has malformed payload hash")?
        + start;
    let old_hash = json[start..end].to_string();
    json = json.replace(
        &format!(
            "\"baseline_sha256\":\"{}\"",
            sha256_hex(&std::fs::read(excel_result_path).map_err(|e| e.to_string())?)
        ),
        "\"baseline_sha256\":null",
    );
    json = json.replace(
        "\"supplied_excel_result_sha256\":null",
        &format!(
            "\"supplied_excel_result_sha256\":\"{}\"",
            sha256_hex(&std::fs::read(excel_result_path).map_err(|e| e.to_string())?)
        ),
    );
    json = json
        .replace("\"baseline\"", "\"supplied_excel_result\"")
        .replace("matches_baseline", "matches_supplied_excel")
        .replace("differs_baseline", "differs_supplied_excel")
        .replace("baseline_mismatch", "excel_result_mismatch")
        .replace(
            "\"excel_build\":null",
            &format!("\"excel_build\":{}", crate::json::escape_str(excel_build)),
        );
    json = json.replace(&old_hash, "__PAYLOAD_HASH__");
    let canonical = sha256_hex(json.as_bytes());
    json = json.replace("__PAYLOAD_HASH__", &canonical);
    Ok((json, decision))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_are_safe() {
        assert_eq!(Policy::default().on_unsupported, PolicyAction::Fallback);
    }
    #[test]
    fn parser_rejects_typos_and_accepts_assertions() {
        assert!(parse_policy("on_unsuppported = \\\"fail\\\"").is_err());
        let p = parse_policy("on_blocked = \"fail\"\n[[assertions]]\nsheet = \"Sheet1\"\nrange = \"A1\"\noperator = \"not_error\"\n").unwrap();
        assert_eq!(p.on_blocked, PolicyAction::Fail);
        assert_eq!(p.assertions.len(), 1);
    }
    #[test]
    fn external_reference_scanner_ignores_quoted_brackets() {
        assert!(has_external_reference("=[Book.xlsx]Sheet1!A1"));
        assert!(!has_external_reference("=\"[not an external ref]\""));
    }
    #[test]
    fn volatile_formula_detection_is_conservative() {
        assert!(has_volatile_function("=NOW()+RAND()"));
        assert!(has_volatile_function("=offset(A1,1,0)"));
        assert!(!has_volatile_function("=SUM(A1:A3)"));
    }
    #[test]
    fn decision_precedence_is_stable() {
        assert_eq!(decide(false, false), Decision::Pass);
        assert_eq!(decide(false, true), Decision::Fallback);
        assert_eq!(decide(true, true), Decision::Fail);
    }

    #[test]
    fn cached_report_contains_v1_receipt_and_decision() {
        let (json, decision) = run_cached_verify(
            Path::new("tests/fixtures/cached_values.xlsx"),
            Some(Path::new("tests/fixtures/verify-policy.toml")),
        )
        .expect("fixture verifies");
        assert!(matches!(
            decision,
            Decision::Pass | Decision::Fail | Decision::Fallback
        ));
        assert!(json.contains("\"schema_version\":\"recalc.verify.report/v1\""));
        assert!(json.contains("\"candidate_sha256\":\""));
        assert!(json.contains("\"canonical_payload_sha256\":\""));
        let marker = "\"canonical_payload_sha256\":\"";
        let start = json.find(marker).unwrap() + marker.len();
        let end = json[start..].find('"').unwrap() + start;
        let digest = &json[start..end];
        assert_eq!(
            digest,
            sha256_hex(json.replace(digest, "__PAYLOAD_HASH__").as_bytes())
        );
    }

    #[test]
    fn unimplemented_policy_requirements_fall_back_loudly() {
        let path = Path::new("tests/fixtures/clean_values.xlsx");
        let policy = Path::new("/tmp/recalc-verify-required-excel-policy.toml");
        std::fs::write(policy, "require_excel_result = true\n").unwrap();
        let (json, decision) = run_cached_verify(path, Some(policy)).unwrap();
        assert_eq!(decision, Decision::Fallback);
        assert!(json.contains("excel_evidence_required"));
        let _ = std::fs::remove_file(policy);
    }

    #[test]
    fn assertions_are_evaluated_after_recalculation() {
        let path = Path::new("tests/fixtures/clean_values.xlsx");
        let policy = Path::new("/tmp/recalc-verify-assertion-policy.toml");
        std::fs::write(policy, "require_comparison = false\n[[assertions]]\nsheet = \"Sheet1\"\nrange = \"A3\"\noperator = \"equals_number\"\nvalue = \"5\"\n").unwrap();
        let (json, decision) = run_cached_verify(path, Some(policy)).unwrap();
        assert_eq!(decision, Decision::Pass);
        assert!(json.contains("\"assertion_failures\":0"));
        std::fs::write(policy, "[[assertions]]\nsheet = \"Sheet1\"\nrange = \"A3\"\noperator = \"equals_number\"\nvalue = \"6\"\n").unwrap();
        let (json, decision) = run_cached_verify(path, Some(policy)).unwrap();
        assert_eq!(decision, Decision::Fail);
        assert!(json.contains("assertion_failed"));
        let _ = std::fs::remove_file(policy);
    }

    #[test]
    fn baseline_comparison_is_explicitly_labelled_and_hashed() {
        let path = Path::new("tests/fixtures/clean_values.xlsx");
        let (json, decision) = run_verify_v1(path, None, Some(path)).unwrap();
        assert_eq!(decision, Decision::Pass);
        assert!(json.contains("\"baseline_sha256\":\""));
        assert!(json.contains("matches_baseline"));
        assert!(json.contains("\"source\":\"baseline\""));
    }

    #[test]
    fn baseline_and_comparison_policy_require_real_evidence() {
        let path = Path::new("tests/fixtures/clean_values.xlsx");
        let policy = Path::new("/tmp/recalc-verify-baseline-policy.toml");
        std::fs::write(policy, "allow_baseline_match = false\n").unwrap();
        let (_, decision) = run_verify_v1(path, Some(policy), Some(path)).unwrap();
        assert_eq!(decision, Decision::Fallback);
        std::fs::write(
            policy,
            "require_comparison = true\nallow_stored_value_match = false\n",
        )
        .unwrap();
        let (json, decision) = run_cached_verify(path, Some(policy)).unwrap();
        assert_eq!(decision, Decision::Fallback);
        assert!(json.contains("comparison_required"));
        let _ = std::fs::remove_file(policy);
    }

    #[test]
    fn supplied_excel_result_requires_and_records_build_identity() {
        let path = Path::new("tests/fixtures/clean_values.xlsx");
        let policy = Path::new("/tmp/recalc-verify-excel-policy.toml");
        std::fs::write(policy, "require_excel_result = true\n").unwrap();
        let (json, decision) =
            run_supplied_excel_verify(path, Some(policy), path, "16.0.12345.20000").unwrap();
        assert_eq!(decision, Decision::Pass);
        assert!(json.contains("supplied_excel_result_sha256"));
        assert!(json.contains("matches_supplied_excel"));
        assert!(json.contains("16.0.12345.20000"));
        assert!(json.contains("\"baseline_sha256\":null"));
        let marker = "\"canonical_payload_sha256\":\"";
        let start = json.find(marker).unwrap() + marker.len();
        let end = json[start..].find('"').unwrap() + start;
        let digest = &json[start..end];
        assert_eq!(
            digest,
            sha256_hex(json.replace(digest, "__PAYLOAD_HASH__").as_bytes())
        );
        let _ = std::fs::remove_file(policy);
    }
}
