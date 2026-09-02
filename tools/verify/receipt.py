#!/usr/bin/env python3
"""Build a deterministic, content-addressed Verify corpus receipt bundle.

This tool never opens or uploads workbook contents. It records exact input
bytes, report bytes, and the explicitly supplied oracle-build label so a farm
run can be reproduced without treating cached values as an Excel oracle.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import platform
import sys
from typing import Any


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def files(root: pathlib.Path, suffixes: tuple[str, ...]) -> list[pathlib.Path]:
    return sorted(
        (p for p in root.rglob("*") if p.is_file() and p.suffix.lower() in suffixes),
        key=lambda p: p.relative_to(root).as_posix(),
    )


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=pathlib.Path, required=True)
    parser.add_argument("--reports", type=pathlib.Path, required=True)
    parser.add_argument("--out", type=pathlib.Path, required=True)
    parser.add_argument("--excel-build", required=True)
    parser.add_argument("--run-id", required=True)
    args = parser.parse_args(argv)
    if not args.excel_build.strip() or not args.run_id.strip():
        parser.error("--excel-build and --run-id must be non-empty")
    if not args.corpus.is_dir() or not args.reports.is_dir():
        parser.error("--corpus and --reports must be directories")

    workbooks = files(args.corpus, (".xlsx", ".xlsm"))
    reports = files(args.reports, (".json",))
    corpus_entries = [
        {
            "path": p.relative_to(args.corpus).as_posix(),
            "bytes": p.stat().st_size,
            "sha256": sha256(p),
        }
        for p in workbooks
    ]
    report_entries: list[dict[str, Any]] = []
    decisions = {"pass": 0, "fail": 0, "fallback": 0, "invalid": 0}
    refusal_counts = {"unsupported": 0, "blocked": 0, "resource_limited": 0, "formula_errors": 0, "mismatches": 0}
    for path in reports:
        entry: dict[str, Any] = {
            "path": path.relative_to(args.reports).as_posix(),
            "bytes": path.stat().st_size,
            "sha256": sha256(path),
        }
        try:
            document = json.loads(path.read_text(encoding="utf-8"))
            decision = document.get("decision")
            if decision in decisions:
                decisions[decision] += 1
            else:
                decisions["invalid"] += 1
            summary = document.get("summary", {})
            for key in refusal_counts:
                value = summary.get(key)
                if isinstance(value, int) and value >= 0:
                    refusal_counts[key] += value
            entry["decision"] = decision
            entry["schema_version"] = document.get("schema_version")
        except (OSError, UnicodeError, json.JSONDecodeError):
            decisions["invalid"] += 1
            entry["invalid_json"] = True
        report_entries.append(entry)

    manifest = {
        "schema_version": "recalc.verify.corpus-manifest/v1",
        "run_id": args.run_id,
        "excel_build": args.excel_build,
        "corpus": corpus_entries,
        "workbook_count": len(corpus_entries),
    }
    receipt = {
        "schema_version": "recalc.verify.receipt-manifest/v1",
        "run_id": args.run_id,
        "excel_build": args.excel_build,
        "tool": "recalc.verify.receipt/v1",
        "platform": {"system": platform.system(), "machine": platform.machine(), "python": platform.python_version()},
        "corpus_manifest_sha256": hashlib.sha256(canonical(manifest)).hexdigest(),
        "reports": report_entries,
        "report_count": len(report_entries),
        "decisions": decisions,
        "refusal_counts": refusal_counts,
    }
    args.out.mkdir(parents=True, exist_ok=True)
    write_json(args.out / "corpus-manifest.json", manifest)
    write_json(args.out / "receipt-manifest.json", receipt)
    return 0 if decisions["invalid"] == 0 else 2


def canonical(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")


def write_json(path: pathlib.Path, value: Any) -> None:
    path.write_bytes(canonical(value))


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
