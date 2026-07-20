//! LET/LAMBDA interpreter support — the lexical environment and the concrete
//! closure value (M2 lane 2).
//!
//! # What lives here
//! The **pure** pieces of the interpreter: the read-only lexical-binding
//! environment threaded through [`eval_expr`](crate::eval), the concrete
//! [`Closure`] behind [`Value::Lambda`], and the name-canonicalization / capture
//! helpers. The seam-dependent pieces (the special forms `LET`/`LAMBDA`/`MAP`/…
//! and lambda application, which need [`eval_expr`]) live in
//! [`crate::eval`] next to the evaluator.
//!
//! # Provenance
//! Semantics pinned by the oracle farm: **OXP-200** (LET scoping, closure
//! capture `LET(x,5,LAMBDA(y,x+y))(10)=15`, arity over/under → `#VALUE!`, bare
//! lambda in a cell → `#CALC!`), **OXP-201** (array broadcasting), **OXP-207**
//! (MAP element-wise + positional zip; SCAN output shape; BYROW→column /
//! BYCOL→row vectors; MAKEARRAY 1-based indices). The lambda value shape is
//! RFC-0012 §2 (BC-2/BC-3) and the §10 amendment (production constructor +
//! `LambdaObj` downcast).
//!
//! # Scoping model (RFC-0012 §8 forward note)
//! A [`Closure`] captures its **definition-time** environment (a snapshot of the
//! visible bindings — lexical scope). Application evaluates the body with
//! `captured + params` and an **empty** parent chain, so scope is purely
//! lexical. A free body name that is neither a parameter nor a captured binding
//! resolves loudly (`#UNSUPPORTED!` via the ordinary defined-name path) rather
//! than guess: **name-based recursion is not yet OXP-pinned** (the §8 forward
//! note flags call-time resolution; deferred to a dedicated probe). Because
//! `LET` binds sequentially, a closure can only capture names bound *before* it,
//! so the capture graph is a DAG — no `Arc` cycles, no leaks (BC-2 rider).

use std::sync::Arc;

use xl_ast::Expr;
use xl_value::{Lambda, LambdaObj, Value};

/// One immutable lexical-binding frame — a cons-cell threaded read-only through
/// the evaluator. Each `LET` binding and each bound lambda parameter pushes one
/// frame; lookup walks parent-ward so an inner binding shadows an outer one.
pub(crate) struct LexFrame<'a> {
    /// The binding's canonical name (see [`canon_name`]).
    pub(crate) name: &'a str,
    /// The bound value, borrowed for the frame's lifetime.
    pub(crate) value: &'a Value,
    /// The enclosing environment.
    pub(crate) parent: LexEnv<'a>,
}

/// The lexical environment: a chain of [`LexFrame`]s, or `None` at the root
/// (an ordinary formula with no `LET`/`LAMBDA` scope in force).
pub(crate) type LexEnv<'a> = Option<&'a LexFrame<'a>>;

/// Resolve a canonical name in the lexical environment (innermost frame wins).
pub(crate) fn lex_lookup<'a>(env: LexEnv<'a>, name: &str) -> Option<&'a Value> {
    let mut cur = env;
    while let Some(f) = cur {
        if f.name == name {
            return Some(f.value);
        }
        cur = f.parent;
    }
    None
}

/// The canonical lexical key of a parameter *declaration* (the name slot of a
/// `LET`/`LAMBDA`): strip the mandatory `_xlpm.` prefix (OXP-199 — a bare
/// parameter open-fails the workbook) and uppercase (Excel names are
/// case-insensitive). A declaration is always `_xlpm.`-prefixed; the prefix is
/// stripped defensively either way so the key is the bare name.
pub(crate) fn canon_name(raw: &str) -> String {
    strip_xlpm(raw).to_ascii_uppercase()
}

/// The lexical key of a *reference* (a `Name` node, or a call name), or `None`
/// if the reference is not `_xlpm.`-prefixed. **Only `_xlpm.`-prefixed names
/// resolve against the lexical environment** — Excel's stored form distinguishes
/// a parameter reference `_xlpm.sum` from the builtin `SUM` and from a bare
/// defined name, so a bare name must NEVER be captured by a same-spelled `LET`
/// binding (that would silently hijack a builtin/defined-name — the bug the contract review
/// caught: `LET(_xlpm.sum, LAMBDA(x,0), SUM(3))` must be `3`, not `0`).
pub(crate) fn as_xlpm_param(raw: &str) -> Option<String> {
    match raw.as_bytes().get(..6) {
        Some(prefix) if prefix.eq_ignore_ascii_case(b"_xlpm.") => {
            Some(raw[6..].to_ascii_uppercase())
        }
        _ => None,
    }
}

/// Strip a leading `_xlpm.` prefix (case-insensitive), byte-safe (the prefix is
/// ASCII, so index 6 is a char boundary once the prefix matches).
fn strip_xlpm(raw: &str) -> &str {
    match raw.as_bytes().get(..6) {
        Some(prefix) if prefix.eq_ignore_ascii_case(b"_xlpm.") => &raw[6..],
        _ => raw,
    }
}

/// Snapshot the visible lexical environment into an owned, de-duplicated list
/// (innermost binding kept) for a closure to capture. Owned (not borrowed) so
/// the closure can outlive the frame stack — the essence of a lexical closure.
pub(crate) fn snapshot(env: LexEnv<'_>) -> Vec<(String, Value)> {
    let mut out: Vec<(String, Value)> = Vec::new();
    let mut cur = env;
    while let Some(f) = cur {
        if !out.iter().any(|(n, _)| n == f.name) {
            out.push((f.name.to_string(), f.value.clone()));
        }
        cur = f.parent;
    }
    out
}

/// The concrete closure behind [`Value::Lambda`] — a LET/LAMBDA lambda's
/// parameters, body, and captured environment. Opaque to `xl-value` (BC-3); the
/// interpreter recovers it via [`downcast_closure`]. `Send + Sync` because every
/// field is (`Vec<String>`, [`Expr`], `Vec<(String, Value)>`) — BC-2.
#[derive(Debug)]
pub(crate) struct Closure {
    /// Parameter names, canonicalized (see [`canon_name`]).
    pub(crate) params: Vec<String>,
    /// The lambda body expression (owned so the closure outlives the call).
    pub(crate) body: Expr,
    /// The definition-time environment snapshot (lexical capture).
    pub(crate) captured: Vec<(String, Value)>,
}

impl LambdaObj for Closure {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Build a [`Value::Lambda`] from an interpreter [`Closure`] (RFC-0012 §10).
pub(crate) fn make_lambda(
    params: Vec<String>,
    body: Expr,
    captured: Vec<(String, Value)>,
) -> Value {
    Value::Lambda(Lambda::new(Arc::new(Closure {
        params,
        body,
        captured,
    })))
}

/// Recover the concrete [`Closure`] from a lambda value (RFC-0012 §10 downcast).
/// Always `Some` in practice — the interpreter is the only producer of
/// `Value::Lambda` — but returns `Option` so a hypothetical foreign `LambdaObj`
/// refuses instead of panicking.
pub(crate) fn downcast_closure(lambda: &Lambda) -> Option<&Closure> {
    lambda.obj().as_any().downcast_ref::<Closure>()
}
