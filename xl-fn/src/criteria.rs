//! The **criteria mini-language** shared by `SUMIF`/`COUNTIF` (and, later,
//! `SUMIFS`/`COUNTIFS`/`AVERAGEIF`, which will reuse this module unchanged).
//!
//! A *criterion* is a single [`Value`] — a number, a boolean, or (the rich
//! case) a text string such as `">5"`, `"<>0"`, `"apple"`, or `"a*c"`. This
//! module compiles that value into a [`Matcher`] once per call and then tests
//! each range cell against it with [`matches`].
//!
//! # Provenance
//! Behavior contract: `docs/specs/SUMIF.md` and `docs/specs/COUNTIF.md` (which
//! cite the Microsoft Learn `SUMIF`/`COUNTIF` pages, verified 2026-07-05). The
//! wildcard grammar (`*`, `?`, `~` escaping) follows Microsoft's documented
//! rule set (support.microsoft.com "Using wildcard characters in searches").
//! **All** value coercion/parsing is deferred to `xl-value`
//! ([`to_number`]) — this module never re-implements number parsing, so the
//! criteria engine inherits en-US numeric parsing (percent, grouping commas,
//! scientific) automatically.
//!
//! # The grammar this implements (documented, non-guessed)
//! 1. **Bare number / bool criteria** → equality. `COUNTIF(r, 5)` matches
//!    numeric-`5` cells; `COUNTIF(r, TRUE)` matches boolean-`TRUE` cells.
//! 2. **Text criteria** — a leading comparison operator is parsed off first
//!    (`>=`, `<=`, `<>`, `>`, `<`, `=`, longest-match first); the remainder is
//!    the *operand*:
//!    - operand parses as a number → numeric comparison/equality (`">5"`,
//!      `"=5"`, `"<>5"`); this now includes **currency** and **date/time** text
//!      (`">$5"` → `>5`, `">1/1/2020"` → `>43831`) via the resolved `to_number`
//!      (OXP-010/160 — see below);
//!    - operand is text → case-insensitive equality with wildcard support
//!      (`"apple"`, `"=a*c"`, `"<>*x*"`); ordering operators (`>`,`<`,`>=`,
//!      `<=`) against a **non-numeric** operand are a case-insensitive
//!      lexicographic text comparison (`">apple"`);
//!    - empty operand → the empty-criteria rule (`""` matches blank/empty,
//!      `"<>"` matches non-blank).
//! 3. **Wildcards** in a text-equality operand: `*` (any run), `?` (any single
//!    char), with `~*`/`~?`/`~~` escaping a literal `*`/`?`/`~`. A lone `~`
//!    before any other character is a literal `~`.
//! 4. **Dynamic criteria** (`">"&A1`) reach this module *already concatenated*
//!    into a final [`Value`] by the engine — there is no formula evaluation
//!    here, only the resulting number/text.
//!
//! # Oracle-resolved — OXP-101 (text half) / OXP-102 (blank value)
//! `RUN-2026-07-11-oracle01` settled two corners that previously deferred; both
//! are now implemented:
//! - **OXP-101 (text ordering)** — an ordering comparison (`>`,`<`,`>=`,`<=`)
//!   whose operand is **non-numeric text** (`">apple"`) is a case-insensitive
//!   **lexicographic** comparison over `Text` cells ([`Matcher::TextOrder`]).
//!   The run's `=COUNTIF(A1:A5,">apple")` counted `banana/cherry/avocado/
//!   blueberry` (4) but not the equal `apple`, and `=SUMIF(A1:A5,">apple",…)`
//!   summed exactly those four rows (140). Number/bool/blank/error cells do not
//!   satisfy a text-ordering criterion (parallel to `NumOrder` excluding
//!   non-numbers) — the conservative reading of the all-text probe.
//! - **OXP-102 (blank criteria value)** — a **blank** criteria *value* (a bare
//!   reference to an empty cell, e.g. `COUNTIF(r, Z9)` with `Z9` empty) is
//!   numeric **equality to `0`**. The run's `=COUNTIF(A1:A5,C1)` (C1 empty)
//!   counted the three `0` cells (3) and `=SUMIF(A1:A5,C1,A1:A5)` summed them
//!   to 0. An `Array`/`Ref` criteria value stays deferred (no observation).
//!
//! # Oracle-resolved — OXP-101/162 (date & currency operands)
//! **Date/time and currency criteria operands are now parsed and compared
//! numerically** (RUN-2026-07-11-oracle01), reproducing the pinned counts:
//! `=COUNTIF(rng,">$5")` = 3, `"=$5"` = 2, `"=1/1/2020"` = 1, `">1/1/2020"` = 3.
//! The operand is coerced through the resolved [`xl_value::to_number`] (OXP-010
//! currency `"$5"` → 5; OXP-160 date/time `"1/1/2020"` → serial 43831,
//! `"16:48"` → 0.7) and fed to [`Matcher::NumOrder`] / [`EqTarget::Number`].
//! This supersedes the earlier "date half deferred" stance — the parser now
//! exists ([`xl_value::parse_datetime_text`]), so the pinned counts are
//! reproduced rather than deferred. A **no-year** operand (`"1/1"`) is deferred
//! by `to_number` itself (→ text ordering here); a genuinely unprobed criteria
//! sub-case is deferred at the coercion layer, never guessed.
//!
//! # Oracle-pending corners (conservative defaults, never guessed)
//! - The exact **blank-cell × criterion matrix** beyond the two documented
//!   anchors (`""` matches a blank *cell*; `"<>"` excludes it) is oracle-
//!   pending (COUNTIF.md). A blank *cell* here matches **only** the `""`
//!   criterion and nothing else — the conservative documented default — so a
//!   blank inside a range never poisons the whole call.
//! - Whether an **error-valued cell** can be targeted by an error-literal
//!   criterion (`"#N/A"`) is oracle-pending (SUMIF.md/COUNTIF.md). The
//!   conservative default here: an error *cell* satisfies **no** criterion (it
//!   is excluded), matching the documented "error cells are simply excluded".
//! - **Numeric-text equality — pinned by OXP-177** (Excel build 16.0). A
//!   numeric-equivalent equality criterion (`"5"`, `"003607"`, `3607`) matches
//!   by **numeric value**: it matches a `Number` cell *and* any numeric `Text`
//!   cell whose [`to_number`] equals it (leading zeros irrelevant — `"003607"`,
//!   `3607`, and `"3607"` are one match class). See [`equals`]. This resolved a
//!   real FUSE `SUMIF`/`COUNTIF` silently-wrong-`0` on leading-zero ID codes.
//!   Only *equality* is pinned; the `">n"` **ordering** case over numeric text
//!   stays `Number`-cell-only (unprobed — not guessed).
//!   - **Derived boundary (OXP-177 probed only decimal digit strings).** Because
//!     [`equals`] delegates the cell side to the shared [`to_number`], the
//!     "numeric value" rule *also* equates a numeric criterion with exotic
//!     numeric-text cells — currency `"$5"`, grouped `"1,000"`, scientific
//!     `"5e0"`, percent `"500%"`, date-text `"1/1/2020"`. These are the minimal
//!     principled generalization (each accepted *form* is itself an oracle-pinned
//!     `to_number` rule, and this module must not re-implement number parsing),
//!     but the *cell-side* equivalence for those forms is **derived, not
//!     observed** — a cell-side matrix is queued as **OXP-178**.
//!   - **`NeedsOracle` swallow (known interface limit).** A deferral-class text
//!     cell (`"1/1"`, `"-$5"`, `"6:45 PM"`) under a numeric equality criterion
//!     reads as "no match" because [`matches`] returns `bool` and
//!     `to_number(..).is_ok_and(..)` collapses `Err(Unsupported)` to `false`.
//!     No regression (these were "no match" before too), but the deferral cannot
//!     reach the aggregate as `#UNSUPPORTED!` without the same `matches`-returns-
//!     `bool` interface change tracked for the OXP-031 cell-side residual below.
//!
//! # Oracle-deferred (never guessed) — OXP-031 (non-ASCII text collation)
//! `RUN-2026-07-11-oracle01` (HELD) established that Excel collates non-ASCII
//! text by Windows **locale** rules, with equivalences a clean-room
//! code-point/ASCII fold provably gets wrong (`"ä" < "z"`, `"ß" = "ss"`). The
//! frozen contract therefore refuses them: `xl-value::compare` /
//! `values_equal` return `#UNSUPPORTED!` whenever **either** operand is
//! non-ASCII text (their guard is `!str::is_ascii`).
//!
//! This module does its **own** text folding, ordering and wildcarding
//! ([`text_order_holds`] via `to_lowercase`; [`glob_match`]/[`ci_eq`] via
//! `to_lowercase`) — it does **not** route through `compare` — so it must
//! mirror that guard or it silently re-introduces the exact code-point fold the
//! frozen contract refuses. It does, at **compile time** ([`parse`]):
//! - a **text-ordering** operand (`">ä"`) that contains any non-ASCII
//!   character compiles to [`Matcher::Unsupported`] instead of
//!   [`Matcher::TextOrder`] (which would rank it by code point);
//! - a **text-equality / wildcard** operand (`"café"`, `"caf*é"`, `"<>café"`)
//!   that contains any non-ASCII character compiles to [`Matcher::Unsupported`]
//!   instead of an [`EqTarget::Text`] glob (which would case-fold/match it by
//!   code point).
//!
//! `// OXP-031`: this keeps the criteria path **consistent with the
//! frozen-contract compare deferral** — the two agree on which text
//! comparisons are explicitly answerable, and neither guesses an unprobed locale
//! collation. Purely-ASCII criteria are unaffected (no regression); numeric,
//! bool, blank and error criteria never reach a text-fold path, so they are
//! untouched. See [`is_non_ascii`] for the (mirrored) detection.
//!
//! ## Cell-side collation — resolved by OXP-189 (RUN 2026-07-13, Excel 16.0)
//! The compile-time guard above fires on the **criterion** operand. The mirror
//! case — an **ASCII criterion tested against a non-ASCII range *cell*** (e.g.
//! `COUNTIF(r,">z")` over a cell `"ä"`) — was left open pending the dedicated
//! collation sweep the two constraints below demand. **OXP-189 ran that sweep:**
//! over `A = {ä, zz, ß}`, Excel applies the **same** locale collation to
//! criteria matching that OXP-031 pinned for the operators — `">z"` = 1 (only
//! `zz`; `ä < z`, `ß < z`), `"<z"` = 2, `"ss"` = 1 (`ß = ss`), `"<>ss"` = 2 —
//! all of which Recalc's code-point/`to_lowercase` fold gets wrong (it would
//! count 3, 0, 0, 3). The structural wildcard `"?"` = 2 (matches the two
//! single-char cells `ä`/`ß`, not the two-char `zz`) is collation-**independent**
//! and Recalc already reproduces it.
//!
//! The cell side is therefore now **deferred**, mirroring the criterion side,
//! subject to the two constraints that made a hasty fix wrong:
//! 1. [`matches`] still returns `bool`; the per-cell deferral reaches the
//!    aggregate through [`refuse_cell`] — the callers' existing
//!    [`sentinel_of`]-propagation seam, one `Option<ErrorKind>` widened to also
//!    carry the collation refusal, so no `*IF*`/`*IFS`/`D*` interface changed.
//! 2. It defers **only** collation-dependent matches against a non-ASCII cell —
//!    a [`Matcher::TextOrder`], or a text-equality [`EqTarget::Text`] pattern
//!    that contains at least one [`Token::Literal`] (a case-folded comparison).
//!    A structural-only pattern (`"?"`, `"*"`, `"*?"` — all [`Token::Any`]/
//!    [`Token::Single`]) is length-based, collation-free, and still computes, so
//!    the OXP-189 `"?"` = 2 pin is preserved (no regression). Numeric/bool/empty
//!    targets and `NumOrder` never collate a text cell and are untouched.

use xl_value::{ErrorKind, Value, to_number};

/// A compiled criterion, produced by [`parse`] and consumed by [`matches`].
///
/// The two "short-circuit" variants ([`Matcher::Unsupported`],
/// [`Matcher::PropagateError`]) are surfaced to the calling function via
/// [`Matcher::short_circuit`] so it can return the whole call's result before
/// walking any cells.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Matcher {
    /// Ordering comparison against a numeric threshold (`">5"`, `"<=0"`).
    /// Only a `Number` cell can satisfy it; every other type (and blank/error)
    /// fails to match.
    NumOrder { op: OrderOp, rhs: f64 },
    /// Ordering comparison against a **text** threshold (`">apple"`), evaluated
    /// case-insensitively and lexicographically (OXP-101,
    /// `RUN-2026-07-11-oracle01`). Only a `Text` cell can satisfy it; every
    /// other type (and blank/error) fails to match — the mirror of `NumOrder`.
    TextOrder { op: OrderOp, rhs: String },
    /// Equality (`negated == false`) or inequality (`negated == true`) against
    /// an [`EqTarget`]. Inequality is broad: a cell of a *different* type than
    /// the target counts as "not equal" and therefore matches `"<>x"`.
    Equality { target: EqTarget, negated: bool },
    /// The criterion is genuinely spec-ambiguous (see the module OXP notes) →
    /// the calling function returns `#UNSUPPORTED!`.
    Unsupported,
    /// The criteria value was itself an error → the calling function
    /// propagates it.
    PropagateError(ErrorKind),
}

/// The four strict/loose ordering operators (equality/inequality are handled
/// by [`Matcher::Equality`], not here).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OrderOp {
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `<`
    Lt,
    /// `<=`
    Le,
}

/// The right-hand side of an equality/inequality criterion.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EqTarget {
    /// Numeric equality: matches a `Number` cell equal to this value.
    Number(f64),
    /// Boolean equality: matches a `Bool` cell equal to this value.
    Bool(bool),
    /// Case-insensitive, wildcard-aware text equality: matches a `Text` cell
    /// whose string matches the compiled pattern.
    Text(Vec<Token>),
    /// The empty-operand criterion (`""` / `"<>"`): equality matches a blank
    /// cell or an empty-string cell.
    Empty,
}

/// One element of a compiled wildcard pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Token {
    /// `*` — matches any run of characters (including none).
    Any,
    /// `?` — matches exactly one character.
    Single,
    /// A literal character (case-insensitive), including a `~`-escaped
    /// `*`/`?`/`~`.
    Literal(char),
}

impl Matcher {
    /// If this criterion cannot be evaluated against cells — because it is
    /// oracle-deferred or wraps an error value — return the whole call's
    /// result; otherwise `None` (proceed to walk the range).
    pub(crate) fn short_circuit(&self) -> Option<Value> {
        match self {
            Matcher::Unsupported => Some(Value::Error(ErrorKind::Unsupported)),
            Matcher::PropagateError(k) => Some(Value::Error(*k)),
            _ => None,
        }
    }
}

/// Compile a criteria [`Value`] into a [`Matcher`] (once per call).
///
/// See the module docs for the full grammar and the oracle-deferred corners.
pub(crate) fn parse(criteria: &Value) -> Matcher {
    match criteria {
        Value::Error(k) => Matcher::PropagateError(*k),
        Value::Number(n) => Matcher::Equality {
            target: EqTarget::Number(*n),
            negated: false,
        },
        Value::Bool(b) => Matcher::Equality {
            target: EqTarget::Bool(*b),
            negated: false,
        },
        Value::Text(t) => parse_text(t.as_str()),
        // OXP-102 (RUN-2026-07-11-oracle01): a blank criteria *value* (a bare
        // reference to an empty cell) is numeric equality to 0 — the run's
        // `COUNTIF(A1:A5, <empty>)` counted the range's `0` cells and its
        // self-`SUMIF` summed them to 0.
        Value::Blank => Matcher::Equality {
            target: EqTarget::Number(0.0),
            negated: false,
        },
        // An Array/Ref criteria value remains oracle-deferred (no observation).
        Value::Array(_) | Value::Ref(_) => Matcher::Unsupported,
        // BC-6 (RFC-0012): a lambda criteria value is refused (`#UNSUPPORTED!`
        // via `Matcher::Unsupported`'s short-circuit), never guessed.
        Value::Lambda(_) => Matcher::Unsupported,
    }
}

/// Parse a text criterion: strip any leading comparison operator, then classify
/// the operand.
fn parse_text(s: &str) -> Matcher {
    let (op, rest) = split_operator(s);
    match op {
        // Equality / inequality share operand classification; only the
        // `negated` flag differs.
        ParsedOp::Eq => build_equality(rest, false),
        ParsedOp::Ne => build_equality(rest, false).negate(),
        ParsedOp::Order(order) => build_order(order, rest),
    }
}

/// Build an ordering matcher from an operand string (the text after `>`/`<`/…).
///
/// An **empty** operand (`">"`, unobserved) defers to [`Matcher::Unsupported`].
/// A **numeric** operand is a numeric comparison — this now includes
/// **currency** (`">$5"` → `>5`) and **full-date/time** operands
/// (`">1/1/2020"` → `>43831`, `"<16:48:00"` → `<0.7`), which parse via the
/// resolved `to_number` (OXP-010 currency, OXP-160 date/time) and reproduce the
/// pinned counts (`COUNTIF(rng,">$5")` = 3, `">1/1/2020"` = 3,
/// RUN-2026-07-11-oracle01). Any other non-numeric operand is a
/// case-insensitive lexicographic text comparison (`">apple"`; OXP-101).
///
/// `// OXP-031`: a non-numeric ordering operand carrying any **non-ASCII**
/// character (`">ä"`) defers — [`Matcher::TextOrder`] would rank it by code
/// point via `to_lowercase`, which Excel's locale collation contradicts
/// (`"ä" < "z"`). This mirrors `xl-value::compare`'s frozen-contract guard so
/// the criteria path is consistent with it (see the module docs). Purely-ASCII
/// ordering operands (`">apple"`) are unaffected.
fn build_order(op: OrderOp, operand: &str) -> Matcher {
    if operand.is_empty() {
        return Matcher::Unsupported;
    }
    if let Some(n) = numeric_operand(operand) {
        return Matcher::NumOrder { op, rhs: n };
    }
    // OXP-031: defer a non-ASCII text-ordering operand rather than code-point
    // rank it — consistent with the frozen-contract `compare` deferral.
    if is_non_ascii(operand) {
        return Matcher::Unsupported;
    }
    Matcher::TextOrder {
        op,
        rhs: operand.to_string(),
    }
}

impl Matcher {
    /// Flip an [`Matcher::Equality`] to its inequality (for the `<>` operator).
    /// Non-equality matchers are returned unchanged (defensive; `parse_text`
    /// only calls this on the equality it just built).
    fn negate(self) -> Matcher {
        match self {
            Matcher::Equality { target, negated } => Matcher::Equality {
                target,
                negated: !negated,
            },
            other => other,
        }
    }
}

/// Build an equality matcher from an operand string (the text after any `=`).
///
/// `// OXP-031`: a **non-numeric** operand carrying any **non-ASCII** character
/// (`"café"`, `"caf*é"`) defers to [`Matcher::Unsupported`] instead of an
/// [`EqTarget::Text`] glob — [`glob_match`]/[`ci_eq`] would case-fold/match it
/// by code point via `to_lowercase`, which Excel's locale collation contradicts
/// (`"ß" = "ss"`). This mirrors `xl-value::compare`'s frozen-contract guard,
/// keeping the criteria path consistent with it (see the module docs). The
/// deferral survives negation (`"<>café"`): [`Matcher::negate`] leaves an
/// `Unsupported` unchanged. Numeric operands and the empty-operand rule
/// (`""`/`"<>"`) never reach the guard, and purely-ASCII patterns (`"a*c"`,
/// `"*"`) are unaffected.
fn build_equality(operand: &str, negated: bool) -> Matcher {
    if operand.is_empty() {
        return Matcher::Equality {
            target: EqTarget::Empty,
            negated,
        };
    }
    match numeric_operand(operand) {
        Some(n) => Matcher::Equality {
            target: EqTarget::Number(n),
            negated,
        },
        // OXP-031: defer a non-ASCII text-equality/wildcard operand rather than
        // code-point fold/glob it — consistent with the `compare` deferral.
        None if is_non_ascii(operand) => Matcher::Unsupported,
        None => Matcher::Equality {
            target: EqTarget::Text(compile_pattern(operand)),
            negated,
        },
    }
}

/// Whether `s` contains any non-ASCII character — the OXP-031 deferral trigger.
///
/// Mirrors `xl-value::coerce`'s `has_non_ascii_text` (`!str::is_ascii`) exactly,
/// so the criteria engine defers precisely the text operands
/// `compare`/`values_equal` refuse to rank, keeping the two consistent with the
/// frozen contract (see the module docs). Detection only — never a collation.
fn is_non_ascii(s: &str) -> bool {
    !s.is_ascii()
}

/// Try to read `operand` as a number via `xl-value`'s coercion (percent,
/// grouping commas, scientific, **currency, and now date/time text** all handled
/// there). `None` if it is not a number (→ treat as a text operand).
///
/// `// OXP-101/162`: a **currency** (`"$5"` → 5) or **full-date** (`"1/1/2020"`
/// → serial 43831) operand parses to its number here and is compared
/// numerically — reproducing the pinned criteria counts
/// (`COUNTIF(rng,">$5")` = 3, `"=$5"` = 2, `"=1/1/2020"` = 1,
/// `">1/1/2020"` = 3, RUN-2026-07-11-oracle01). This is the resolved behavior:
/// `to_number` now coerces currency (OXP-010) and date/time text (OXP-160), so a
/// criteria date/currency operand is no longer deferred. A **time-of-day**
/// operand (`"16:48"`) parses to its serial fraction by the same mechanism; a
/// **no-year** date (`"1/1"`) is deferred by `to_number` itself (→ `None` here →
/// treated as text), consistent with the frozen contract.
fn numeric_operand(operand: &str) -> Option<f64> {
    to_number(&Value::text(operand)).ok()
}

/// The operator classification returned by [`split_operator`].
enum ParsedOp {
    /// `=` or no operator (implicit equality).
    Eq,
    /// `<>`
    Ne,
    /// A strict/loose ordering operator.
    Order(OrderOp),
}

/// Strip a leading comparison operator, longest-match first. A string with no
/// recognized operator is an implicit equality against the whole string.
fn split_operator(s: &str) -> (ParsedOp, &str) {
    // Two-character operators first (longest match), then single-character.
    if let Some(rest) = s.strip_prefix(">=") {
        (ParsedOp::Order(OrderOp::Ge), rest)
    } else if let Some(rest) = s.strip_prefix("<=") {
        (ParsedOp::Order(OrderOp::Le), rest)
    } else if let Some(rest) = s.strip_prefix("<>") {
        (ParsedOp::Ne, rest)
    } else if let Some(rest) = s.strip_prefix('>') {
        (ParsedOp::Order(OrderOp::Gt), rest)
    } else if let Some(rest) = s.strip_prefix('<') {
        (ParsedOp::Order(OrderOp::Lt), rest)
    } else if let Some(rest) = s.strip_prefix('=') {
        (ParsedOp::Eq, rest)
    } else {
        // No operator → implicit equality against the whole string.
        (ParsedOp::Eq, s)
    }
}

/// Compile a text-equality operand into a wildcard pattern.
///
/// `*` → [`Token::Any`], `?` → [`Token::Single`]; `~` escapes the next
/// `*`/`?`/`~` into a literal; a `~` before anything else is itself a literal.
fn compile_pattern(s: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '~' => match chars.peek() {
                Some('*' | '?' | '~') => {
                    let escaped = chars.next().expect("peeked char exists");
                    tokens.push(Token::Literal(escaped));
                }
                _ => tokens.push(Token::Literal('~')),
            },
            '*' => tokens.push(Token::Any),
            '?' => tokens.push(Token::Single),
            other => tokens.push(Token::Literal(other)),
        }
    }
    tokens
}

/// If `v` is a **Recalc sentinel** (`#UNSUPPORTED!`/`#BLOCKED!`/`#RESOURCE!`,
/// [`xl_value::ErrorKind::is_recalc_sentinel`]), return its kind; `None` for
/// every other value, *including* a genuine Excel error.
///
/// # Why every `*IF`/`*IFS`/`D*` caller must check this before [`matches`]
/// [`matches`] treats **every** error cell — sentinel or genuine — as "no
/// match" (its `cell.is_error()` guard below). That is the correct,
/// oracle-documented conservative default for a *genuine* Excel error (the
/// cell really does hold `#N/A`/`#DIV/0!`/… and Excel's own criteria
/// semantics exclude it — see the module's "blank / error cell defaults"
/// notes). It is **wrong** for a sentinel: Recalc never actually evaluated
/// that cell, so whether real Excel's tested value would have satisfied the
/// criterion is unknowable. Reporting "excluded" would launder that gap into
/// a confident, possibly-wrong answer — exactly the Principle-2 ("never
/// silently wrong") violation this helper exists to close.
///
/// Every criteria-walk call site must therefore call `sentinel_of` on a
/// **criteria-tested** cell immediately before passing it to [`matches`], in
/// the walk's existing scan order, and propagate the first sentinel kind it
/// finds (kind preserved, never collapsed to a different sentinel) out of
/// the whole aggregation — mirroring how a matched **aggregated** cell's
/// error already propagates via `coerce_number_arg`. `matches`' own
/// signature (`-> bool`) is deliberately unchanged: it cannot report a
/// sentinel itself, so the check happens in the caller, one layer up.
///
/// A sentinel in a cell that is **not** criteria-tested (e.g. an unmatched
/// `sum_range`/`average_range` cell) is genuinely irrelevant to the result
/// and must **not** be checked here — this helper is only for the
/// criteria-tested cell.
pub(crate) fn sentinel_of(v: &Value) -> Option<ErrorKind> {
    match v {
        Value::Error(k) if k.is_recalc_sentinel() => Some(*k),
        // BC-6 (RFC-0012): a lambda is not a recalc sentinel — `None`, made an
        // explicit decision rather than swallowed by the wildcard.
        Value::Lambda(_) => None,
        _ => None,
    }
}

/// Whether the whole aggregation must **refuse** on account of THIS
/// criteria-tested `cell` under `matcher` — and with which [`ErrorKind`].
///
/// Combines the two per-cell refusals a criteria walk must apply, at the one
/// point they apply (immediately before [`matches`], on a **criteria-tested**
/// cell only — never a `sum_range`/`average_range` value cell):
///
/// 1. **Recalc sentinel** ([`sentinel_of`]) — the cell was never evaluated, so
///    whether it would match is unknowable; propagate its kind (unchanged).
/// 2. **OXP-189 (RUN 2026-07-13) non-ASCII cell-side collation** — a
///    collation-dependent match against a **non-ASCII** `Text` cell would run
///    Recalc's code-point/`to_lowercase` fold, which disagrees with Excel's
///    locale collation (`COUNTIF({ä,zz,ß},">z")` = 1, `"ss"` = 1 via `ß`=`ss`),
///    so it defers to `#UNSUPPORTED!` rather than return a wrong count. See the
///    module "Cell-side collation" section for the full pin and the mirror with
///    the criterion-side OXP-031 guard.
///
/// Every criteria-walk call site uses this in place of a bare [`sentinel_of`]
/// check, in its existing scan order, propagating the first refusal it finds.
pub(crate) fn refuse_cell(matcher: &Matcher, cell: &Value) -> Option<ErrorKind> {
    if let Some(k) = sentinel_of(cell) {
        return Some(k);
    }
    if collation_defers(matcher, cell) {
        return Some(ErrorKind::Unsupported);
    }
    None
}

/// OXP-189: whether matching `cell` under `matcher` needs a Windows-locale
/// collation Recalc cannot faithfully reproduce — a **non-ASCII `Text` cell**
/// under a **collation-dependent** matcher. See [`refuse_cell`] / the module
/// "Cell-side collation" section.
///
/// Collation-dependent (→ defer): a text-**ordering** comparison, or a
/// text-**equality** pattern carrying at least one literal character (a
/// case-folded compare — `ß`≠`ss` under code-point fold). Collation-**free**
/// (→ compute, no defer): a structural-only wildcard pattern (`"?"`/`"*"`, all
/// [`Token::Any`]/[`Token::Single`] — length-based, OXP-189 `"?"` = 2), and
/// every non-text target (`NumOrder`, numeric/bool/empty equality never collate
/// a text cell).
fn collation_defers(matcher: &Matcher, cell: &Value) -> bool {
    let Value::Text(s) = cell else {
        return false;
    };
    if s.as_str().is_ascii() {
        return false;
    }
    match matcher {
        // Ordering against a non-ASCII text cell: code-point rank disagrees with
        // Excel collation (`ä`<`z`, `ß`<`z`) — OXP-189 H1/H2.
        Matcher::TextOrder { .. } => true,
        // Case-folding text equality with ≥1 literal char: `ci_eq`/`glob_match`
        // would fold by code point (`ß`≠`ss`) — OXP-189 H3/H4. A structural-only
        // pattern (all `Any`/`Single`) is length-based (H5) and does not defer.
        Matcher::Equality {
            target: EqTarget::Text(pattern),
            ..
        } => pattern.iter().any(|t| matches!(t, Token::Literal(_))),
        // NumOrder and numeric/bool/empty equality targets never collate a text
        // cell (a non-ASCII text cell fails them type-wise, collation-free).
        _ => false,
    }
}

/// Test one range `cell` against a compiled criterion.
///
/// Blank cells match **only** the `""` criterion; error cells match nothing
/// (both are the conservative documented defaults — see the module OXP notes).
/// The short-circuit matchers ([`Matcher::Unsupported`] /
/// [`Matcher::PropagateError`]) never match a cell; callers handle them via
/// [`Matcher::short_circuit`] before ever walking cells.
///
/// **Every caller must check [`sentinel_of`] on `cell` first** (see its
/// docs) — this function makes no sentinel/genuine-error distinction itself,
/// by design, since it cannot report a sentinel through its `bool` result.
pub(crate) fn matches(matcher: &Matcher, cell: &Value) -> bool {
    // Error cells are excluded from every criterion (error-literal criteria is
    // oracle-pending; the default is exclusion).
    if cell.is_error() {
        return false;
    }
    // A blank cell matches only the empty-equality criterion (`""`).
    if cell.is_blank() {
        return matches!(
            matcher,
            Matcher::Equality {
                target: EqTarget::Empty,
                negated: false,
            }
        );
    }
    match matcher {
        Matcher::NumOrder { op, rhs } => match cell {
            Value::Number(c) => order_holds(*op, *c, *rhs),
            // BC-6 (RFC-0012): a lambda ≠ a numeric-order criterion — explicit.
            Value::Lambda(_) => false,
            _ => false,
        },
        Matcher::TextOrder { op, rhs } => match cell {
            Value::Text(s) => text_order_holds(*op, s.as_str(), rhs),
            // BC-6 (RFC-0012): a lambda ≠ a text-order criterion — explicit.
            Value::Lambda(_) => false,
            _ => false,
        },
        Matcher::Equality { target, negated } => negated ^ equals(target, cell),
        Matcher::Unsupported | Matcher::PropagateError(_) => false,
    }
}

/// Whether `lhs <op> rhs` holds for finite numbers.
fn order_holds(op: OrderOp, lhs: f64, rhs: f64) -> bool {
    match op {
        OrderOp::Gt => lhs > rhs,
        OrderOp::Ge => lhs >= rhs,
        OrderOp::Lt => lhs < rhs,
        OrderOp::Le => lhs <= rhs,
    }
}

/// Whether `lhs <op> rhs` holds for a case-insensitive lexicographic text
/// comparison (OXP-101, `RUN-2026-07-11-oracle01`). Case is folded the same way
/// [`ci_eq`] folds it (Unicode-lowercase), keeping ordering and equality
/// case-consistent across the criteria engine.
fn text_order_holds(op: OrderOp, lhs: &str, rhs: &str) -> bool {
    use std::cmp::Ordering;
    let ord = lhs.to_lowercase().cmp(&rhs.to_lowercase());
    match op {
        OrderOp::Gt => ord == Ordering::Greater,
        OrderOp::Ge => ord != Ordering::Less,
        OrderOp::Lt => ord == Ordering::Less,
        OrderOp::Le => ord != Ordering::Greater,
    }
}

/// Whether a (non-blank, non-error) `cell` equals an [`EqTarget`]. Equality is
/// type-sensitive: a mismatched cell type is "not equal".
fn equals(target: &EqTarget, cell: &Value) -> bool {
    match target {
        // OXP-177 (Excel build 16.0): a numeric-equivalent equality criterion
        // matches by NUMERIC VALUE — a `Number` cell equal to `n`, AND a numeric
        // `Text` cell whose `to_number` equals `n` (leading zeros irrelevant:
        // `"003607"` == `3607` == `"3607"`). Non-numeric text never coerces, so
        // it never matches. Only equality is pinned; the `NumOrder` (`">n"`) case
        // over numeric text stays `Number`-only (unpinned — not guessed).
        EqTarget::Number(n) => match cell {
            Value::Number(c) => *c == *n,
            Value::Text(_) => to_number(cell).is_ok_and(|c| c == *n),
            // BC-6 (RFC-0012): a lambda ≠ a numeric equality target — explicit.
            Value::Lambda(_) => false,
            _ => false,
        },
        EqTarget::Bool(b) => matches!(cell, Value::Bool(c) if *c == *b),
        EqTarget::Text(pattern) => {
            matches!(cell, Value::Text(s) if glob_match(pattern, s.as_str()))
        }
        // A non-blank cell equals `Empty` only when it is the empty string.
        EqTarget::Empty => matches!(cell, Value::Text(s) if s.as_str().is_empty()),
    }
}

/// Case-insensitive Excel glob match of a compiled `pattern` against `text`.
///
/// Standard linear-time-with-backtracking glob: `*` greedily consumes and
/// backtracks on a later mismatch, `?` consumes exactly one character, literals
/// compare case-insensitively. This is Excel's `*?~` glob, **not** a regex.
fn glob_match(pattern: &[Token], text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut ti = 0usize; // index into `chars`
    let mut pi = 0usize; // index into `pattern`
    let mut star: Option<usize> = None; // pattern index of the last `*`
    let mut star_ti = 0usize; // text index where that `*` started matching

    while ti < chars.len() {
        if pi < pattern.len() {
            match pattern[pi] {
                Token::Single => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                Token::Literal(c) => {
                    if ci_eq(c, chars[ti]) {
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                }
                Token::Any => {
                    star = Some(pi);
                    star_ti = ti;
                    pi += 1;
                    continue;
                }
            }
        }
        // Mismatch or pattern exhausted: backtrack to the last `*`, letting it
        // absorb one more character.
        if let Some(sp) = star {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    // Text consumed; any trailing `*`s match the empty remainder.
    while pi < pattern.len() && pattern[pi] == Token::Any {
        pi += 1;
    }
    pi == pattern.len()
}

/// Case-insensitive single-character equality (Unicode-lowercase fold, matching
/// `xl-value`'s comparison convention).
fn ci_eq(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(x: f64) -> Value {
        Value::number(x)
    }
    fn txt(s: &str) -> Value {
        Value::text(s)
    }

    // ---- comparison operators -------------------------------------------

    #[test]
    fn numeric_comparison_operators() {
        let cases = [
            (">5", 6.0, true),
            (">5", 5.0, false),
            (">=5", 5.0, true),
            ("<5", 4.0, true),
            ("<5", 5.0, false),
            ("<=5", 5.0, true),
            ("<=5", 6.0, false),
        ];
        for (crit, cell, want) in cases {
            let m = parse(&txt(crit));
            assert_eq!(matches(&m, &num(cell)), want, "{crit} vs {cell}");
        }
    }

    #[test]
    fn comparison_only_matches_numeric_cells() {
        // ">5" never matches a text/bool cell (no cross-type ranking).
        let m = parse(&txt(">5"));
        assert!(!matches(&m, &txt("zzz")));
        assert!(!matches(&m, &Value::Bool(true)));
        assert!(!matches(&m, &Value::Blank));
    }

    #[test]
    fn equals_operator_number() {
        let m = parse(&txt("=5"));
        assert!(matches(&m, &num(5.0)));
        assert!(!matches(&m, &num(6.0)));
        // OXP-177: "=5" matches a numeric-text cell "5" (and "05") by numeric
        // value; non-numeric text still does not.
        assert!(matches(&m, &txt("5")));
        assert!(matches(&m, &txt("05")));
        assert!(!matches(&m, &txt("5x")));
    }

    // ---- literal equality (case-insensitive) ----------------------------

    #[test]
    fn bare_text_is_case_insensitive_literal_equality() {
        let m = parse(&txt("Apple"));
        assert!(matches(&m, &txt("apple")));
        assert!(matches(&m, &txt("APPLE")));
        assert!(!matches(&m, &txt("apples")));
        // A number cell never matches a text literal.
        assert!(!matches(&m, &num(1.0)));
    }

    #[test]
    fn bare_number_criteria_value_is_numeric_equality() {
        let m = parse(&num(5.0));
        assert!(matches(&m, &num(5.0)));
        // OXP-177: a number criterion also matches numeric-text cells by value.
        assert!(matches(&m, &txt("5")));
        assert!(matches(&m, &txt("05")));
        assert!(!matches(&m, &txt("five")));
    }

    #[test]
    fn numeric_criteria_negation_excludes_numeric_text() {
        // The entailed complement of the OXP-177 pin: `<>5` must EXCLUDE the
        // numeric-value-5 class (number 5, text "5"/"05") and INCLUDE a
        // non-numeric text cell. Guards the derived negation path.
        let m = parse(&txt("<>5"));
        assert!(!matches(&m, &num(5.0)));
        assert!(!matches(&m, &txt("5")));
        assert!(!matches(&m, &txt("05")));
        assert!(matches(&m, &txt("5x")));
        assert!(matches(&m, &num(6.0)));
    }

    #[test]
    fn numeric_text_criteria_matches_number() {
        // "5" (text) matches numeric 5 — numeric equality via xl-value parse.
        let m = parse(&txt("5"));
        assert!(matches(&m, &num(5.0)));
    }

    #[test]
    fn bool_criteria_value_matches_bool_cell() {
        let m = parse(&Value::Bool(true));
        assert!(matches(&m, &Value::Bool(true)));
        assert!(!matches(&m, &Value::Bool(false)));
        assert!(!matches(&m, &num(1.0)));
    }

    // ---- wildcards ------------------------------------------------------

    #[test]
    fn wildcard_star_any_run() {
        let m = parse(&txt("a*c"));
        assert!(matches(&m, &txt("ac")));
        assert!(matches(&m, &txt("abc")));
        assert!(matches(&m, &txt("aXYZc")));
        assert!(!matches(&m, &txt("ab")));
        assert!(!matches(&m, &txt("xabc")));
    }

    #[test]
    fn wildcard_question_single_char() {
        let m = parse(&txt("a?c"));
        assert!(matches(&m, &txt("abc")));
        assert!(matches(&m, &txt("aXc")));
        assert!(!matches(&m, &txt("ac")));
        assert!(!matches(&m, &txt("abbc")));
    }

    #[test]
    fn wildcard_leading_and_trailing_star() {
        let contains = parse(&txt("*mid*"));
        assert!(matches(&contains, &txt("mid")));
        assert!(matches(&contains, &txt("XXmidYY")));
        assert!(!matches(&contains, &txt("mud")));

        let prefix = parse(&txt("pre*"));
        assert!(matches(&prefix, &txt("prefix")));
        assert!(!matches(&prefix, &txt("xprefix")));
    }

    #[test]
    fn wildcard_is_case_insensitive() {
        let m = parse(&txt("A*C"));
        assert!(matches(&m, &txt("axc")));
    }

    #[test]
    fn wildcard_escapes_literal_star_question_tilde() {
        // "~*" matches a literal asterisk, not "any run".
        let star = parse(&txt("~*"));
        assert!(matches(&star, &txt("*")));
        assert!(!matches(&star, &txt("anything")));

        // "a~?c" matches literal "a?c".
        let q = parse(&txt("a~?c"));
        assert!(matches(&q, &txt("a?c")));
        assert!(!matches(&q, &txt("abc")));

        // "~~" matches a literal tilde.
        let tilde = parse(&txt("~~"));
        assert!(matches(&tilde, &txt("~")));
        assert!(!matches(&tilde, &txt("~~")));
    }

    #[test]
    fn lone_tilde_is_literal() {
        // A "~" not before *,?,~ is itself literal.
        let m = parse(&txt("~a"));
        assert!(matches(&m, &txt("~a")));
    }

    // ---- <> inequality (broad) ------------------------------------------

    #[test]
    fn not_equal_number_is_broad() {
        let m = parse(&txt("<>5"));
        assert!(matches(&m, &num(6.0)));
        assert!(!matches(&m, &num(5.0)));
        // "<>5" matches text and bool cells (they are "not equal" to 5).
        assert!(matches(&m, &txt("apple")));
        assert!(matches(&m, &Value::Bool(true)));
    }

    #[test]
    fn not_equal_text_with_wildcard() {
        let m = parse(&txt("<>*x*"));
        assert!(!matches(&m, &txt("axb"))); // contains x → equal → excluded
        assert!(matches(&m, &txt("abc"))); // no x → not equal → matches
        // A number cell is "not equal" to a text pattern → matches.
        assert!(matches(&m, &num(1.0)));
    }

    // ---- empty-operand criteria -----------------------------------------

    #[test]
    fn empty_criteria_matches_blank_and_empty_string() {
        let m = parse(&txt(""));
        assert!(matches(&m, &Value::Blank));
        assert!(matches(&m, &txt("")));
        assert!(!matches(&m, &num(0.0)));
        assert!(!matches(&m, &txt("x")));
    }

    #[test]
    fn not_blank_criteria_excludes_blank_matches_nonblank() {
        let m = parse(&txt("<>"));
        assert!(!matches(&m, &Value::Blank));
        assert!(!matches(&m, &txt("")));
        assert!(matches(&m, &num(0.0)));
        assert!(matches(&m, &txt("x")));
    }

    // ---- blank / error cell defaults ------------------------------------

    #[test]
    fn blank_cell_matches_only_empty_criteria() {
        assert!(!matches(&parse(&txt(">5")), &Value::Blank));
        assert!(!matches(&parse(&txt("<>5")), &Value::Blank));
        assert!(!matches(&parse(&num(0.0)), &Value::Blank));
        assert!(matches(&parse(&txt("")), &Value::Blank));
    }

    #[test]
    fn error_cell_matches_nothing() {
        let err = Value::Error(ErrorKind::Na);
        assert!(!matches(&parse(&txt(">5")), &err));
        assert!(!matches(&parse(&txt("<>5")), &err));
        assert!(!matches(&parse(&txt("#N/A")), &err));
    }

    // ---- dynamic (pre-concatenated) criteria ----------------------------

    #[test]
    fn pre_concatenated_dynamic_criteria() {
        // `">"&A1` with A1=10 arrives as the final text ">10".
        let m = parse(&txt(">10"));
        assert!(matches(&m, &num(11.0)));
        assert!(!matches(&m, &num(10.0)));
    }

    // ---- oracle deferrals / short-circuits ------------------------------

    #[test]
    fn text_ordering_operand_is_case_insensitive_lexicographic() {
        // OXP-101 (RUN-2026-07-11-oracle01): a non-numeric, non-date ordering
        // operand is a case-insensitive lexicographic text comparison.
        // Models `=COUNTIF(A1:A5,">apple")` = 4 (banana/cherry/avocado/
        // blueberry match; the equal "apple" does not).
        let m = parse(&txt(">apple"));
        let cells = ["apple", "banana", "cherry", "avocado", "blueberry"];
        let hits = cells.iter().filter(|c| matches(&m, &txt(c))).count();
        assert_eq!(hits, 4);
        assert!(!matches(&m, &txt("apple"))); // equal → excluded
        assert!(matches(&m, &txt("avocado"))); // "av" > "ap"
        // Case-insensitive, and a non-Text cell never satisfies text ordering.
        assert!(matches(&parse(&txt(">APPLE")), &txt("banana")));
        assert!(!matches(&m, &num(1.0)));
        assert!(!matches(&m, &Value::Bool(true)));
        assert!(!matches(&m, &Value::Blank));
        // <= / >= boundaries (inclusive) and < .
        assert!(matches(&parse(&txt("<=apple")), &txt("apple")));
        assert!(matches(&parse(&txt(">=apple")), &txt("apple")));
        assert!(matches(&parse(&txt("<banana")), &txt("apple")));
        // Not a short-circuit — it is a supported matcher.
        assert_eq!(m.short_circuit(), None);
    }

    #[test]
    fn currency_and_date_operands_parse_and_reproduce_pinned_counts() {
        // OXP-101/162 (RUN-2026-07-11-oracle01): currency and full-date criteria
        // operands parse to their number via the resolved `to_number` and compare
        // numerically. The pinned COUNTIF counts are 3/2/1/3; synthetic ranges
        // below yield exactly those.
        let count = |crit: &str, cells: &[Value]| {
            let m = parse(&txt(crit));
            cells.iter().filter(|c| matches(&m, c)).count()
        };

        // Currency: `">$5"` = 3, `"=$5"` = 2 (operand `$5` → 5.0).
        let money = [num(5.0), num(5.0), num(6.0), num(7.0), num(8.0)];
        assert_eq!(
            parse(&txt(">$5")),
            Matcher::NumOrder {
                op: OrderOp::Gt,
                rhs: 5.0
            }
        );
        assert_eq!(count(">$5", &money), 3); // 6, 7, 8
        assert_eq!(count("=$5", &money), 2); // the two 5s

        // Full date: `">1/1/2020"` = 3, `"=1/1/2020"` = 1 (operand → serial 43831).
        let dates = [
            num(43830.0),
            num(43831.0),
            num(43832.0),
            num(43833.0),
            num(43834.0),
        ];
        assert_eq!(
            parse(&txt(">1/1/2020")),
            Matcher::NumOrder {
                op: OrderOp::Gt,
                rhs: 43831.0
            }
        );
        assert_eq!(count(">1/1/2020", &dates), 3); // 43832, 43833, 43834
        assert_eq!(count("=1/1/2020", &dates), 1); // exactly 43831
        // A date-equality operand is a numeric equality, not a text one.
        assert_eq!(
            parse(&txt("=1/1/2020")),
            Matcher::Equality {
                target: EqTarget::Number(43831.0),
                negated: false,
            }
        );
        // These are supported matchers now — no short-circuit.
        assert_eq!(parse(&txt(">1/1/2020")).short_circuit(), None);
    }

    #[test]
    fn empty_and_no_year_operands_still_defer_or_textualize() {
        // A bare operator (`">"`, unobserved) still defers loudly.
        assert_eq!(parse(&txt(">")), Matcher::Unsupported);
        assert_eq!(
            parse(&txt(">")).short_circuit(),
            Some(Value::Error(ErrorKind::Unsupported))
        );
        // A no-year date (`">1/1"`) is deferred by `to_number` itself, so it
        // falls to a (case-insensitive) text ordering here — not a numeric one.
        assert!(matches!(parse(&txt(">1/1")), Matcher::TextOrder { .. }));
    }

    #[test]
    fn blank_criteria_value_matches_zero() {
        // OXP-102 (RUN-2026-07-11-oracle01): a blank criteria *value* is
        // numeric equality to 0 — `=COUNTIF(A1:A5,C1)` (C1 empty) counted the
        // three `0` cells (3) and `=SUMIF(A1:A5,C1,A1:A5)` summed them to 0.
        let m = parse(&Value::Blank);
        assert!(matches(&m, &num(0.0)));
        assert!(!matches(&m, &num(5.0)));
        assert!(!matches(&m, &txt("")));
        assert!(!matches(&m, &Value::Blank)); // a blank *cell* is not matched
        assert_eq!(m.short_circuit(), None);
    }

    #[test]
    fn array_and_ref_criteria_value_is_oracle_deferred() {
        // OXP-102: only the blank criteria *value* was resolved; an Array/Ref
        // criteria value has no observation and stays deferred.
        assert_eq!(
            parse(&Value::Array(xl_value::Array::from_scalar(num(1.0)))),
            Matcher::Unsupported
        );
    }

    #[test]
    fn error_criteria_value_propagates() {
        let m = parse(&Value::Error(ErrorKind::Ref));
        assert_eq!(m, Matcher::PropagateError(ErrorKind::Ref));
        assert_eq!(m.short_circuit(), Some(Value::Error(ErrorKind::Ref)));
    }

    #[test]
    fn supported_matcher_has_no_short_circuit() {
        assert_eq!(parse(&txt(">5")).short_circuit(), None);
        assert_eq!(parse(&txt("apple")).short_circuit(), None);
    }

    #[test]
    fn percent_and_grouping_operands_parse_numerically() {
        // Reuse of xl-value numeric parsing: "50%" → 0.5, "1,000" → 1000.
        assert!(matches(&parse(&txt(">50%")), &num(0.6)));
        assert!(!matches(&parse(&txt(">50%")), &num(0.4)));
        assert!(matches(&parse(&txt(">=1,000")), &num(1000.0)));
    }

    // ---- OXP-031: non-ASCII text matching defers (compare-guard consistency) --

    #[test]
    fn non_ascii_text_ordering_criterion_defers() {
        // OXP-031 (RUN-2026-07-11-oracle01, HELD): a text-ordering operand with a
        // non-ASCII char would be ranked by code point (`to_lowercase` + `cmp`),
        // which Excel's locale collation contradicts (`"ä" < "z"`). Mirror
        // `xl-value::compare`'s guard — defer, never guess.
        for crit in [">ä", "<ä", ">=café", "<=Ω", ">straße", ">=naïve"] {
            assert_eq!(parse(&txt(crit)), Matcher::Unsupported, "{crit}");
        }
        // It short-circuits the whole COUNTIF/SUMIF call to `#UNSUPPORTED!`.
        assert_eq!(
            parse(&txt(">ä")).short_circuit(),
            Some(Value::Error(ErrorKind::Unsupported))
        );
    }

    #[test]
    fn non_ascii_text_equality_and_wildcard_criterion_defers() {
        // Equality, wildcard, and inequality operands carrying a non-ASCII char
        // defer (OXP-031): a code-point case-fold / glob would silently disagree
        // with Excel's locale collation (`"ß" = "ss"`).
        for crit in [
            "café", "=café", "caf*é", "cafÉ", "*ä*", "a?ç", "<>café", "~ä",
        ] {
            assert_eq!(parse(&txt(crit)), Matcher::Unsupported, "{crit}");
        }
        // `<>café` must defer despite the `<>` negation (negate() leaves an
        // Unsupported unchanged), and short-circuits the call.
        assert_eq!(parse(&txt("<>café")), Matcher::Unsupported);
        assert_eq!(
            parse(&txt("café")).short_circuit(),
            Some(Value::Error(ErrorKind::Unsupported))
        );
    }

    #[test]
    fn ascii_criteria_unaffected_by_non_ascii_guard() {
        // No regression: purely-ASCII criteria keep compiling to their supported
        // matchers and match exactly as before the OXP-031 guard.
        let ord = parse(&txt(">apple"));
        assert!(matches!(ord, Matcher::TextOrder { .. }));
        assert!(matches(&ord, &txt("banana")));
        assert!(!matches(&ord, &txt("apple")));

        assert!(matches(&parse(&txt("apple")), &txt("APPLE")));
        assert!(matches(&parse(&txt("a*c")), &txt("aXYZc")));
        assert!(!matches(&parse(&txt("a*c")), &txt("ab")));

        // A pure `*`/`?` wildcard is collation-INDEPENDENT: it matches any text
        // cell, non-ASCII included, so the ASCII criterion must NOT defer and
        // must still match a non-ASCII *cell* (see the module "Residual" note —
        // deferring these would be a regression, not a fix).
        let star = parse(&txt("*"));
        assert!(matches!(star, Matcher::Equality { .. }));
        assert_eq!(star.short_circuit(), None);
        assert!(matches(&star, &txt("café")));
        assert!(matches(&parse(&txt("?")), &txt("ä")));

        // Numeric / bool / empty criteria never reach the text-fold guard.
        assert!(matches(&parse(&txt(">5")), &num(6.0)));
        assert!(matches(&parse(&num(5.0)), &num(5.0)));
        assert_eq!(parse(&txt("")).short_circuit(), None);
    }

    // ---- OXP-189: non-ASCII CELL-side collation defers (refuse_cell) ------

    #[test]
    fn oxp189_non_ascii_cell_under_text_ordering_defers() {
        // OXP-189 (RUN 2026-07-13, Excel 16.0): over A = {ä, zz, ß},
        // COUNTIF(A,">z") = 1 (only `zz`; `ä`<`z`, `ß`<`z`) and `"<z"` = 2 —
        // both of which Recalc's code-point rank gets wrong. An ASCII ordering
        // criterion against a non-ASCII text cell therefore defers via
        // `refuse_cell`, so the whole aggregation returns `#UNSUPPORTED!`.
        let gt = parse(&txt(">z"));
        assert!(matches!(gt, Matcher::TextOrder { .. }));
        assert_eq!(refuse_cell(&gt, &txt("ä")), Some(ErrorKind::Unsupported));
        assert_eq!(refuse_cell(&gt, &txt("ß")), Some(ErrorKind::Unsupported));
        // ASCII cells never collate-defer (the `zz` cell is answered directly).
        assert_eq!(refuse_cell(&gt, &txt("zz")), None);
    }

    #[test]
    fn oxp189_non_ascii_cell_under_literal_text_equality_defers() {
        // OXP-189: COUNTIF({ä,zz,ß},"ss") = 1 via `ß`=`ss`, and `"<>ss"` = 2 —
        // a case-folded literal comparison Recalc's `ci_eq` gets wrong. Deferral
        // survives the `<>` negation (the pattern still carries literals).
        let eq = parse(&txt("ss"));
        assert!(matches!(eq, Matcher::Equality { .. }));
        assert_eq!(refuse_cell(&eq, &txt("ß")), Some(ErrorKind::Unsupported));
        assert_eq!(
            refuse_cell(&parse(&txt("<>ss")), &txt("ß")),
            Some(ErrorKind::Unsupported)
        );
        // A literal ASCII match against a non-ASCII cell also collates → defer.
        assert_eq!(
            refuse_cell(&parse(&txt("apple")), &txt("äpple")),
            Some(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn oxp189_structural_wildcard_over_non_ascii_cell_does_not_defer() {
        // OXP-189 H5: COUNTIF({ä,zz,ß},"?") = 2 — `?`/`*` are length-based and
        // collation-INDEPENDENT, so a structural-only pattern must NOT defer on a
        // non-ASCII cell (deferring would regress the pinned count). `matches`
        // still answers it directly: `?` matches the single-char `ä`/`ß`.
        for crit in ["?", "*", "*?", "??"] {
            let m = parse(&txt(crit));
            assert_eq!(refuse_cell(&m, &txt("ä")), None, "{crit}");
        }
        assert!(matches(&parse(&txt("?")), &txt("ä")));
        assert!(!matches(&parse(&txt("?")), &txt("zz"))); // two chars ≠ one `?`
    }

    #[test]
    fn oxp189_non_collating_matchers_over_non_ascii_cell_do_not_defer() {
        // A numeric-order or numeric/bool/empty-equality matcher never collates a
        // text cell (a non-ASCII text cell fails them type-wise, collation-free),
        // so `refuse_cell` returns `None` and the existing "no match" stands.
        assert_eq!(refuse_cell(&parse(&txt(">5")), &txt("ä")), None);
        assert_eq!(refuse_cell(&parse(&num(5.0)), &txt("ä")), None);
        assert_eq!(refuse_cell(&parse(&Value::Bool(true)), &txt("ä")), None);
        assert_eq!(refuse_cell(&parse(&txt("")), &txt("ä")), None); // Empty target
    }

    #[test]
    fn oxp189_ascii_cells_and_non_text_cells_never_collate_defer() {
        // No regression: an ASCII text cell, and any non-text cell, never trigger
        // the collation deferral under any collation-dependent matcher.
        let gt = parse(&txt(">z"));
        assert_eq!(refuse_cell(&gt, &txt("zz")), None);
        assert_eq!(refuse_cell(&gt, &num(1.0)), None);
        assert_eq!(refuse_cell(&gt, &Value::Blank), None);
        assert_eq!(refuse_cell(&parse(&txt("ss")), &txt("SS")), None); // ASCII
    }

    #[test]
    fn oxp189_sentinel_still_propagates_through_refuse_cell() {
        // `refuse_cell` subsumes the sentinel guard: a Recalc sentinel in the
        // criteria-tested cell still propagates its exact kind, ahead of (and
        // independent of) the collation check.
        for k in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(refuse_cell(&parse(&txt(">z")), &Value::Error(k)), Some(k));
        }
        // A genuine (non-sentinel) error is not a refusal — matches excludes it.
        assert_eq!(
            refuse_cell(&parse(&txt(">z")), &Value::Error(ErrorKind::Div0)),
            None
        );
    }

    // ---- sentinel_of (the *IF/*IFS/D* caller guard) ----------------------

    #[test]
    fn sentinel_of_detects_all_three_sentinel_kinds() {
        for k in [
            ErrorKind::Unsupported,
            ErrorKind::Blocked,
            ErrorKind::Resource,
        ] {
            assert_eq!(sentinel_of(&Value::Error(k)), Some(k), "{k:?}");
        }
    }

    #[test]
    fn sentinel_of_is_none_for_genuine_errors_and_non_errors() {
        for k in [
            ErrorKind::Na,
            ErrorKind::Div0,
            ErrorKind::Value,
            ErrorKind::Ref,
            ErrorKind::Name,
            ErrorKind::Num,
            ErrorKind::Null,
        ] {
            assert_eq!(sentinel_of(&Value::Error(k)), None, "{k:?}");
        }
        assert_eq!(sentinel_of(&num(1.0)), None);
        assert_eq!(sentinel_of(&txt("x")), None);
        assert_eq!(sentinel_of(&Value::Blank), None);
        assert_eq!(sentinel_of(&Value::Bool(true)), None);
    }

    #[test]
    fn non_ascii_guard_is_criterion_side_scoped() {
        // OXP-031 residual, pinned (see module docs): the guard fires on the
        // *criterion* the user wrote, NOT on range *cells*. An ASCII criterion
        // stays a supported matcher regardless of what the range holds — the
        // matcher is compiled before any cell is seen, and `matches` returns
        // `bool`, so a per-cell deferral cannot reach the aggregate result
        // without a cross-caller interface change (tracked follow-up). We pin
        // the scope here rather than enshrine the code-point fold of a cell as
        // "correct".
        assert_eq!(parse(&txt(">apple")).short_circuit(), None);
        assert_eq!(parse(&txt("apple")).short_circuit(), None);
        assert_eq!(parse(&txt("a*c")).short_circuit(), None);
    }
}
