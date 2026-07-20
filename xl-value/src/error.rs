//! Excel error values (`ErrorKind`) and their canonical display strings.
//!
//! ## Provenance
//! Excel's built-in error set and their exact spellings are documented at
//! Microsoft Learn, "Detect errors in formulas" and
//! "How to correct a #… error", e.g.
//! <https://support.microsoft.com/en-us/office/how-to-correct-a-value-error>
//! and the error-type reference behind `ERROR.TYPE`
//! <https://support.microsoft.com/en-us/office/error-type-function>.
//! `#GETTING_DATA`, `#SPILL!`, and `#CALC!` are the newer errors documented
//! alongside dynamic arrays and data types on Microsoft Learn.
//!
//! `#UNSUPPORTED!`, `#BLOCKED!`, and `#RESOURCE!` are **Recalc-specific**
//! sentinels required by the project's "never silently wrong" principle
//! (`implementation-plan.md` §0). They are not produced by Excel; they are
//! how Recalc refuses to guess (`#UNSUPPORTED!`), reports a sandbox refusal
//! (`#BLOCKED!` — `WEBSERVICE`/`RTD`/`STOCKHISTORY`), or reports a hard
//! resource cap hit (`#RESOURCE!`).

use core::fmt;

/// An Excel error value, plus Recalc's project-specific sentinels.
///
/// `ErrorKind` is `Copy` and cheap; it is embedded directly in
/// [`crate::Value::Error`]. Comparisons and coercions **propagate** errors:
/// an error operand short-circuits the whole operation to that same error
/// (leftmost error wins — see [`crate::compare`] and the `to_*` functions).
///
/// The `Display` and [`ErrorKind::as_str`] output is the **exact** literal
/// Excel shows in a cell (e.g. `#DIV/0!`), so it round-trips through the
/// UI-visible string. This is a frozen part of the contract: other crates
/// match on these strings when reading cached error values from `.xlsx`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// `#NULL!` — intersection of two ranges that do not intersect.
    Null,
    /// `#DIV/0!` — division by zero (or by an empty/blank cell).
    Div0,
    /// `#VALUE!` — a value is of the wrong type for the operation.
    Value,
    /// `#REF!` — a reference is not valid (e.g. a deleted cell).
    Ref,
    /// `#NAME?` — unrecognized text (a name/function that is not defined).
    Name,
    /// `#NUM!` — invalid numeric value (overflow, domain error, non-finite).
    Num,
    /// `#N/A` — a value is not available to a function or formula.
    Na,
    /// `#GETTING_DATA` — a cell is awaiting an external data source.
    GettingData,
    /// `#SPILL!` — a dynamic array cannot spill into blocked cells.
    Spill,
    /// `#CALC!` — the calculation engine hit an unsupported array situation.
    Calc,
    /// `#UNSUPPORTED!` — **Recalc-specific.** A function/feature/semantic is
    /// not yet implemented; Recalc refuses to guess (`implementation-plan.md`
    /// §0, "Never silently wrong").
    Unsupported,
    /// `#BLOCKED!` — **Recalc-specific.** A sandboxed capability was refused
    /// (`WEBSERVICE`/`RTD`/`STOCKHISTORY`, network, filesystem).
    Blocked,
    /// `#RESOURCE!` — **Recalc-specific.** A hard resource cap was reached
    /// (memory/time/size) and the engine degraded gracefully.
    Resource,
}

impl ErrorKind {
    /// The exact literal Excel (or Recalc, for the sentinels) shows in a
    /// cell, e.g. `"#DIV/0!"`. Never allocates.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Null => "#NULL!",
            ErrorKind::Div0 => "#DIV/0!",
            ErrorKind::Value => "#VALUE!",
            ErrorKind::Ref => "#REF!",
            ErrorKind::Name => "#NAME?",
            ErrorKind::Num => "#NUM!",
            ErrorKind::Na => "#N/A",
            ErrorKind::GettingData => "#GETTING_DATA",
            ErrorKind::Spill => "#SPILL!",
            ErrorKind::Calc => "#CALC!",
            ErrorKind::Unsupported => "#UNSUPPORTED!",
            ErrorKind::Blocked => "#BLOCKED!",
            ErrorKind::Resource => "#RESOURCE!",
        }
    }

    /// `true` for the three Recalc-specific sentinels
    /// ([`ErrorKind::Unsupported`], [`ErrorKind::Blocked`],
    /// [`ErrorKind::Resource`]); `false` for genuine Excel errors.
    ///
    /// The fidelity report uses this to separate "Excel would also error
    /// here" from "Recalc bailed out".
    #[must_use]
    pub const fn is_recalc_sentinel(self) -> bool {
        matches!(
            self,
            ErrorKind::Unsupported | ErrorKind::Blocked | ErrorKind::Resource
        )
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
