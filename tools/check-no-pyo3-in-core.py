#!/usr/bin/env python3
"""CI guard: binding / server / parallel dependency isolation (denylist).

Enforces the the dependency-approval policy conditions that confine the FFI,
server, and parallel-recalc dependencies to their designated crates:

  - pyo3 / napi / napi-derive / napi-build / wasm-bindgen  → `xl-ffi` only
  - tokio / hyper / hyper-util                             → `xl-server` only
  - rayon / rayon-core                                     → `xl-engine` only

The scan covers **every workspace member**, not just the core library crates:
the ratified conditions forbid, e.g., tokio in `xl-ffi` (napi condition 9) and
in the `xl-bench` harness, not merely in the six core crates.

(The script keeps its historical name so CI/gate invocations stay stable; it
now guards the whole binding-dep denylist across the whole workspace — see the
2026-07-14 napi-rs / wasm-bindgen / rayon / xl-server-async approvals.)

## Two check shapes (why not one uniform transitive scan)

- **Leaf deps (pyo3/napi*/wasm-bindgen/tokio/hyper*)** live in crates nothing
  else depends on (`xl-ffi`, `xl-server`), so a transitive-closure check over
  any workspace member is exact: reaching the dep means the member itself pulled
  it. Checked for ALL members against a per-dep allowed-owner set.
- **rayon** lives in `xl-engine`, which `xl-ffi`/`xl-server`/`xl-bench` depend
  on — so those legitimately *reach* rayon transitively via `xl-engine` (a
  native binding over a parallel engine is fine; the wasm build simply never
  enables the `parallel` feature, a feature-resolution matter, not a graph one).
  A transitive check would false-positive there. rayon is therefore checked only
  against the CORE crates that must never reach it — every core crate except its
  owner `xl-engine`. Those five do not depend on `xl-engine`, so a rayon edge in
  their closure is a genuine violation.

We resolve with `--all-features` so every binding/server/parallel feature is on
and the deps actually materialize in the graph.

Stdlib only (`json`, `subprocess`). Exit 0 on success, 1 on violation, 2 on a
tooling error.
"""

from __future__ import annotations

import json
import subprocess
import sys

CORE_CRATES = {"xl-io", "xl-ast", "xl-value", "xl-graph", "xl-fn", "xl-engine"}

# Leaf deps: checked over EVERY workspace member. dep -> allowed owner crate(s).
LEAF_OWNERS: dict[str, set[str]] = {
    "pyo3": {"xl-ffi"},
    "napi": {"xl-ffi"},
    "napi-derive": {"xl-ffi"},
    "napi-build": {"xl-ffi"},
    "wasm-bindgen": {"xl-ffi"},
    "tokio": {"xl-server"},
    "hyper": {"xl-server"},
    "hyper-util": {"xl-server"},
}

# rayon: owned by xl-engine; forbidden in the OTHER core crates (which do not
# depend on xl-engine, so a rayon edge there is genuine, not transitive-legit).
RAYON = {"rayon", "rayon-core"}
RAYON_FORBIDDEN_IN = CORE_CRATES - {"xl-engine"}


def load_metadata() -> dict:
    # Capture stdout ONLY so cargo's "Downloading ..." lines can't corrupt the
    # JSON; stderr is inherited so any real error is visible.
    try:
        proc = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--all-features"],
            check=True,
            stdout=subprocess.PIPE,
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        print(f"error: `cargo metadata` failed: {exc}", file=sys.stderr)
        raise SystemExit(2) from exc
    return json.loads(proc.stdout)


def main() -> int:
    md = load_metadata()

    id_to_name = {pkg["id"]: pkg["name"] for pkg in md["packages"]}

    resolve = md.get("resolve")
    if not resolve or not resolve.get("nodes"):
        print("error: cargo metadata returned no resolve graph", file=sys.stderr)
        return 2
    nodes = {node["id"]: node for node in resolve["nodes"]}

    # Workspace members (the crates WE own), name -> id.
    member_ids = set(md.get("workspace_members", []))
    name_to_id = {id_to_name[i]: i for i in member_ids}

    def transitive_names(root_id: str) -> set[str]:
        seen: set[str] = set()
        stack = [root_id]
        while stack:
            nid = stack.pop()
            for dep in nodes.get(nid, {}).get("deps", []):
                pkg_id = dep["pkg"]
                if pkg_id not in seen:
                    seen.add(pkg_id)
                    stack.append(pkg_id)
        return {id_to_name[i] for i in seen}

    # Sanity: the core crates must all be present as workspace members.
    missing = sorted(CORE_CRATES - set(name_to_id))
    if missing:
        print(
            f"error: expected core crate(s) not found in workspace metadata: {missing}",
            file=sys.stderr,
        )
        return 2

    closures = {name: transitive_names(mid) for name, mid in name_to_id.items()}

    violations: list[str] = []
    # Leaf deps: check every workspace member.
    for name, closure in closures.items():
        for dep, owners in LEAF_OWNERS.items():
            if dep in closure and name not in owners:
                violations.append(f"{name} -> {dep}")
    # rayon: check the core crates that must never reach it.
    for name in sorted(RAYON_FORBIDDEN_IN):
        closure = closures.get(name, set())
        for dep in RAYON:
            if dep in closure:
                violations.append(f"{name} -> {dep}")

    if violations:
        print(
            "FAIL: workspace crate(s) reach a denylisted binding/server/parallel "
            "dep outside its owning crate (the Recalc design rules isolation violated): "
            f"{sorted(set(violations))}",
            file=sys.stderr,
        )
        return 1

    denylist = sorted(set(LEAF_OWNERS) | RAYON)
    print(
        "OK: binding/server/parallel deps stay in their owning crates "
        f"({denylist}). Checked all workspace members: {sorted(name_to_id)}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
