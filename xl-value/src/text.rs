//! Interned text ([`Text`]) and the minimal global string interner backing it.
//!
//! ## Provenance / design note
//! Excel strings are compared **case-insensitively** and appear in vast
//! numbers of duplicated cells (shared strings in OOXML;
//! `implementation-plan.md` §2 "interned strings/formulas"). Interning gives
//! O(1) `Arc` clones and cheap identity checks.
//!
//! ### Interner design (a genuine fork — the simpler-to-freeze option chosen)
//! Two designs were considered:
//!
//! 1. **A single process-global pool** (chosen). `Text::new` deduplicates
//!    against one lazily-initialized global set, so two independently
//!    constructed `Text`s with equal contents share one `Arc<str>` and are
//!    pointer-equal. Simple, and it makes the "equal strings intern equal"
//!    invariant hold across the whole process with no plumbing.
//! 2. **An explicit per-workbook pool** threaded through every API. More
//!    scalable (bounded lifetime, no cross-workbook contention) but it would
//!    leak a pool handle into the *frozen* value contract, which is exactly
//!    what we do not want to freeze prematurely.
//!
//! The **public surface of [`Text`] is identical under either design**
//! (`new`, `as_str`, `Deref<str>`, `Eq`/`Hash`/`Ord` by content). The global
//! pool can therefore be swapped for a scoped arena later via an internal
//! change only — no change to the enum contract — so option 1 is frozen now.
//!
//! ### Known limitation
//! The global pool is **never emptied**: every distinct string interned in
//! the process is retained for the process lifetime. For batch recalc of
//! many workbooks this is a slow leak. Growth is faster than "distinct
//! workbook strings" suggests: number→text coercion ([`crate::to_text`])
//! also interns its output, and computed-number strings are mostly unique —
//! see the "Performance caveat" on that function. The pool's mutex is also a
//! serialization point for future parallel recalc. All accepted for v0; the
//! fix is to move to a scoped arena (and/or a non-interning constructor for
//! transient strings) later — tracked as a perf RFC, not an oracle
//! experiment, since it has no bearing on computed values.

use core::fmt;
use core::ops::Deref;
use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};

/// Process-global interner. Holds a strong `Arc` for every distinct string,
/// which is what makes pointer-equality stable across independent `new`
/// calls (the pool's own reference keeps each allocation alive).
static POOL: LazyLock<Mutex<HashSet<Arc<str>>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// An interned, immutable string value — the payload of [`crate::Value::Text`].
///
/// Cloning is a cheap `Arc` bump. Equality, hashing, and ordering are **by
/// content** (case-sensitively — see below), so `Text` behaves like a normal
/// string key; interning is a transparent optimization, not a semantic
/// change.
///
/// Note that `Text`'s own `Eq`/`Ord` are **case-sensitive** (identity of the
/// stored bytes). Excel's *case-insensitive* comparison semantics live in
/// [`crate::compare`] / [`crate::text_eq_ci`], deliberately kept out of
/// `Text` so the interner never has to fold case.
#[derive(Clone)]
pub struct Text(Arc<str>);

impl Text {
    /// Interns `s`, returning a `Text` that shares its allocation with every
    /// other `Text` of equal content. Two calls `Text::new("x")` from
    /// anywhere in the process return pointer-equal values (see
    /// [`Text::ptr_eq`]).
    ///
    /// Never panics: a poisoned interner lock is recovered rather than
    /// propagated, upholding the crate-wide "coercion/model ops never panic"
    /// invariant.
    #[must_use]
    pub fn new(s: &str) -> Text {
        let mut pool = POOL.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = pool.get(s) {
            Text(Arc::clone(existing))
        } else {
            let arc: Arc<str> = Arc::from(s);
            pool.insert(Arc::clone(&arc));
            Text(arc)
        }
    }

    /// Borrows the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `true` iff the two `Text`s share the same interned allocation.
    ///
    /// Because [`Text::new`] deduplicates globally, this is equivalent to
    /// content equality for any `Text` built through `new`; it is exposed so
    /// callers can assert the interner invariant cheaply.
    #[must_use]
    pub fn ptr_eq(&self, other: &Text) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl From<&str> for Text {
    fn from(s: &str) -> Self {
        Text::new(s)
    }
}

impl From<String> for Text {
    fn from(s: String) -> Self {
        Text::new(&s)
    }
}

impl Deref for Text {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        // Content equality. For interned values this is also pointer
        // equality, but comparing content keeps correctness independent of
        // the interner implementation.
        *self.0 == *other.0
    }
}

impl Eq for Text {}

impl PartialEq<str> for Text {
    fn eq(&self, other: &str) -> bool {
        &*self.0 == other
    }
}

impl PartialEq<&str> for Text {
    fn eq(&self, other: &&str) -> bool {
        &*self.0 == *other
    }
}

impl core::hash::Hash for Text {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        (*self.0).hash(state);
    }
}

impl PartialOrd for Text {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Text {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        (*self.0).cmp(&*other.0)
    }
}

impl fmt::Debug for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}

impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
