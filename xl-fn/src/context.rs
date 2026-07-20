//! The evaluation context ([`EvalContext`]) — the sandbox authority seam.
//!
//! # Provenance / rationale
//! `implementation-plan.md` §2 requires that Recalc functions receive their
//! ambient authority (clock, randomness) through an **injected context** rather
//! than reaching for `std::time::SystemTime` / a global RNG. That keeps recalc
//! deterministic and sandboxable (seedable `RAND`, injectable clock; no network
//! or wall-clock reads from a formula). This module wires the *seam* only:
//! v0 has no volatile functions implemented, so every capability returns
//! [`ErrorKind::Unsupported`]. When `NOW`/`TODAY`/`RAND` land (a later task)
//! they consume these methods instead of ambient authority, and the injected
//! clock/RNG are threaded in here without changing any function signature.
//!
//! The methods take `&self`: v0 holds no mutable state, and a future stateful
//! RNG can add interior mutability (a `RefCell`/atomic seed) behind this same
//! `&self` interface, so the evaluator can keep passing a shared `&EvalContext`
//! to nested calls (avoiding `&mut` aliasing during argument evaluation).
//!
//! # The date system is context, not an argument
//! The 1900/1904 date system ([`DateSystem`]) is workbook-level state that the
//! date functions (`YEAR`/`MONTH`/`DAY`/`DATE`/`EOMONTH`) must consult
//! consistently (`docs/specs/DATE.md` §6). It rides here — injected by
//! `xl-engine` from the loaded workbook's `workbookPr/@date1904` flag — for the
//! same reason as the clock/RNG: a function must not reach for ambient state.
//! It defaults to the 1900 system (Excel-on-Windows default).

use xl_value::ErrorKind;

use crate::datecore::DateSystem;

/// Ambient authority handed to every function evaluation.
///
/// Holds the capability seam (clock/RNG, `#UNSUPPORTED!` in v0) plus the
/// workbook's [`DateSystem`]. Construct with [`EvalContext::new`]
/// (1900 default), [`EvalContext::with_date_system`], or
/// [`EvalContext::default`].
#[derive(Clone, Debug, Default)]
pub struct EvalContext {
    /// The active 1900/1904 date system. Read by the date functions; injected
    /// by `xl-engine` from the workbook. Future: `clock: Clock`, `rng: Rng`
    /// slots join it here without changing any function signature.
    date_system: DateSystem,
}

impl EvalContext {
    /// A fresh context with the v0 defaults (1900 date system; clock and RNG
    /// both unsupported).
    #[must_use]
    pub fn new() -> EvalContext {
        EvalContext {
            date_system: DateSystem::Excel1900,
        }
    }

    /// A context pinned to a specific [`DateSystem`] (the rest of the defaults
    /// unchanged). `xl-engine` calls this with the loaded workbook's flag.
    #[must_use]
    pub fn with_date_system(date_system: DateSystem) -> EvalContext {
        EvalContext { date_system }
    }

    /// The workbook's active 1900/1904 date system, consulted by the date
    /// functions.
    #[must_use]
    pub fn date_system(&self) -> DateSystem {
        self.date_system
    }

    /// The current date-time serial (`NOW`).
    ///
    /// # Errors
    /// Always [`ErrorKind::Unsupported`] in v0: no clock is injected yet, and
    /// Recalc never reads the wall clock ambiently (`implementation-plan.md`
    /// §2, sandbox defaults).
    pub fn now(&self) -> Result<f64, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    /// The current date serial (`TODAY`).
    ///
    /// # Errors
    /// Always [`ErrorKind::Unsupported`] in v0 — see [`EvalContext::now`].
    pub fn today(&self) -> Result<f64, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    /// A uniform random draw in `[0, 1)` (`RAND`).
    ///
    /// # Errors
    /// Always [`ErrorKind::Unsupported`] in v0: no seeded RNG is injected yet.
    /// Until then Recalc refuses rather than drawing non-deterministically.
    ///
    /// # Determinism constraint for the future implementation
    /// The eventual draw must be an **order-independent pure function** of
    /// `(workbook seed, cell address, recalc generation)` — NOT a sequential
    /// draw-stream threaded through evaluation order. A stateful stream (a
    /// `RefCell`/atomic seed advanced per call) would make the result depend on
    /// evaluation order, which the parallel executor (RFC-0014 R5) evaluates
    /// concurrently — so it would break the serial==parallel bit-identity
    /// guarantee and is forbidden. Equivalently, a stateful RNG must close the
    /// parallel gate. This context type must also stay `Sync` (a `const`
    /// assertion in `xl-engine`'s parallel build enforces it), so any seed
    /// state must be `Sync` and order-free.
    pub fn rand(&self) -> Result<f64, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }
}
