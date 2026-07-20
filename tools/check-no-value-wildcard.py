#!/usr/bin/env python3
"""CI guard: no bare ``_ =>`` arm over a ``Value`` match in the load-bearing
coercion / ordering / marshalling functions (RFC-0012 BC-6, Part E item 2).

Fixing the 39 `Value::Lambda` sites protects against *that* variant; it does
**not** protect against the *next* `Value` variant. The load-bearing helpers
below decide type semantics for every value that flows between crates — a bare
``_`` wildcard there would silently absorb a new variant instead of forcing a
"born refusing" decision (Principle 2). This guard mirrors
``tools/check-no-pyo3-in-core.py``: stdlib only, greps a pinned denylist, fails
the build if a bare wildcard arm reappears.

The rule is intentionally strict: these functions must enumerate `Value`
variants explicitly (no bare ``_ =>``), so adding variant #9 breaks the build
here — the compiler, not a checklist, becomes the guard. Tuple/`Option`
fallbacks must bind names (e.g. ``(lhs, rhs) =>``, ``(None, _) =>``) rather than
use a standalone ``_ =>`` arm.

Denylisted functions (file :: fn):
  * xl-value/src/coerce.rs      :: compare_with
  * xl-value/src/coerce.rs      :: scalar_type_rank
  * xl-value/src/coerce.rs      :: total_order
  * xl-value/src/value.rs       :: upholds_number_invariant
  * xl-ffi/src/python.rs        :: value_to_py
  * xl-bench/src/diff.rs        :: classify
  * xl-io/src/sheet_xml.rs      :: resolve_value  (+ its future write-back inverse)

Exit 0 on success, 1 on a violation, 2 on a tooling error (e.g. a denylisted
function was renamed/removed — that must be re-pinned here, never silently
dropped).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

# (relative path from repo root, function name). A function renamed away from
# this list is a tooling error (exit 2): re-pin it, do not drop the guard.
DENYLIST: list[tuple[str, str]] = [
    ("xl-value/src/coerce.rs", "compare_with"),
    ("xl-value/src/coerce.rs", "scalar_type_rank"),
    ("xl-value/src/coerce.rs", "total_order"),
    ("xl-value/src/value.rs", "upholds_number_invariant"),
    ("xl-ffi/src/python.rs", "value_to_py"),
    ("xl-bench/src/diff.rs", "classify"),
    ("xl-io/src/sheet_xml.rs", "resolve_value"),
]

# A function-signature line: optional indent, optional `pub`/`pub(crate)` and
# `const`/`async`/`unsafe` qualifiers, then `fn NAME`.
_FN_RE = re.compile(r"^(?P<indent>\s*)(?:pub(?:\([^)]*\))?\s+)?(?:(?:const|async|unsafe)\s+)*fn\s+")
# A bare wildcard match arm: the whole pattern is `_` (possibly with a match
# guard is NOT counted — the checklist targets the bare catch-all). Matches
# `_ =>` / `_=>`, but never `(_, None) =>`, `(lhs, rhs) =>`, or `_ if c =>`.
_BARE_WILDCARD_ARM_RE = re.compile(r"^_\s*=>")


def find_fn_span(lines: list[str], name: str) -> tuple[int, int] | None:
    """Return the inclusive ``[start, end]`` line span of ``fn name``.

    The end is the first line at the same indentation as the signature whose
    stripped content is exactly ``}`` — i.e. the function's own closing brace,
    not a nested block's. Robust against `{...}` inside format strings and
    nested `fn`/closures without needing a full brace/string parser.
    """
    sig = re.compile(_FN_RE.pattern + re.escape(name) + r"\b")
    for i, line in enumerate(lines):
        if sig.match(line):
            indent = _FN_RE.match(line).group("indent")
            closer = indent + "}"
            for j in range(i + 1, len(lines)):
                if lines[j].rstrip() == closer:
                    return (i, j)
            return (i, len(lines) - 1)  # unterminated; scan to EOF
    return None


def main() -> int:
    repo_root = Path(__file__).resolve().parent.parent
    violations: list[str] = []
    missing: list[str] = []
    checked: list[str] = []

    for rel_path, fn_name in DENYLIST:
        path = repo_root / rel_path
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except OSError as exc:
            print(f"error: cannot read {rel_path}: {exc}", file=sys.stderr)
            return 2

        span = find_fn_span(lines, fn_name)
        if span is None:
            missing.append(f"{rel_path}::{fn_name}")
            continue

        start, end = span
        checked.append(f"{rel_path}::{fn_name}")
        for k in range(start, end + 1):
            if _BARE_WILDCARD_ARM_RE.match(lines[k].strip()):
                violations.append(f"{rel_path}:{k + 1}: bare `_ =>` in `{fn_name}`")

    if missing:
        print(
            "error: denylisted function(s) not found (renamed/removed?) — re-pin "
            f"them in this guard, never drop the coverage: {missing}",
            file=sys.stderr,
        )
        return 2

    if violations:
        print(
            "FAIL: bare `_ =>` wildcard over a `Value` match in a load-bearing "
            "function (RFC-0012 BC-6, Part E). Enumerate the variants explicitly "
            "so the next `Value` variant breaks the build here:",
            file=sys.stderr,
        )
        for v in violations:
            print(f"  {v}", file=sys.stderr)
        return 1

    print(f"OK: no bare `_ =>` over a Value match in load-bearing fns. Checked: {checked}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
