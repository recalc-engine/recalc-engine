"""Workbook discovery + deterministic sample selection.

`discover_workbooks` mirrors `xl-bench`'s `corpus::discover_workbooks`: recurse
a directory, keep `.xlsx`/`.xlsm` (ASCII case-insensitive), return sorted.

Deterministic sample rule (documented in docs/competitor-harness.md):
  given the M sorted workbook paths, pick N of them at evenly-spaced indices
  `floor(k * M / N)` for k in 0..N (dedup-collapsed if M < N). This spreads the
  sample across the whole corpus (not the alphabetical first N), and is fully
  reproducible from the corpus contents alone — no RNG, no seed file.
"""
from __future__ import annotations

import os


def discover_workbooks(root: str) -> list:
    out = []
    for dirpath, _dirnames, filenames in os.walk(root):
        for fn in filenames:
            low = fn.lower()
            if low.endswith(".xlsx") or low.endswith(".xlsm"):
                out.append(os.path.join(dirpath, fn))
    out.sort()
    return out


def evenly_spaced_sample(paths: list, n: int) -> list:
    m = len(paths)
    if m == 0 or n <= 0:
        return []
    if m <= n:
        return list(paths)
    idxs = []
    seen = set()
    for k in range(n):
        i = (k * m) // n
        if i not in seen:
            seen.add(i)
            idxs.append(i)
    return [paths[i] for i in idxs]


def sample_corpus(root: str, n: int) -> list:
    return evenly_spaced_sample(discover_workbooks(root), n)


if __name__ == "__main__":
    import argparse

    ap = argparse.ArgumentParser(description="Print the deterministic N-book sample.")
    ap.add_argument("root")
    ap.add_argument("-n", type=int, default=50)
    args = ap.parse_args()
    for p in sample_corpus(args.root, args.n):
        print(p)
