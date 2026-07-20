#!/usr/bin/env python3
"""Generates `cached_values.xlsx` and `clean_values.xlsx` — tiny,
hand-authored, synthetic fixtures for `xl-bench`'s integration tests
(`xl-bench/tests/cached_fixture.rs`, `xl-bench/tests/verify_dir.rs`).

Provenance / why this exists (see `xl-bench/tests/fixtures/README.md`):
`zip` (the crate xl-io uses to build/read xlsx packages) is approved for
`xl-io` only (the dependency-approval policy) — `xl-bench` may not
add it as a dependency just to synthesize a test fixture in-process the way
`xl-io/tests/support/mod.rs` does. So these fixtures are generated **once,
offline, by this stdlib-only script** and the resulting `.xlsx` bytes are
committed; the script itself is committed alongside them so each fixture's
exact provenance and contents are auditable and reproducible (re-run this
script to regenerate byte-for-byte, since Sheet1's cell walk order and the
zip member order below are both fixed).

Unlike `tools/gridgen/gridgen.py` (which deliberately omits cached `<v>`
values — the farm computes them later), `cached_values.xlsx` **injects**
cached values, including a deliberately WRONG one, specifically so
`xl-bench`'s diff harness has every classification case to exercise in one
small file:

| Cell | Formula          | Cached `<v>`   | Expected `xl-bench` status      |
|------|------------------|----------------|----------------------------------|
| A1   | (literal 2)      | n/a            | not a formula cell, excluded     |
| A2   | (literal 3)      | n/a            | not a formula cell, excluded     |
| A3   | `=SUM(A1:A2)`    | `5`            | Exact (engine also computes 5)   |
| A4   | `=SUM(A1:A2)`    | `999`          | Mismatch (poisoned oracle value) |
| A5   | `=NOTAREALFN(A1)`| `42`           | EngineUnsupported (unknown fn)   |
| A6   | `=SUM(A1:A2)`    | `<v>` omitted  | NoOracle (blank cached value)    |

`clean_values.xlsx` is the mismatch-free counterpart (only A1-A3 above, so
every judged cell is Exact) — the corpus-runner tests need a file whose row
in the aggregate index reports "ok" next to `cached_values.xlsx`'s
"mismatch" row and a garbage file's "load failure" row.

Run: `python3 gen_fixture.py` (writes both files next to this script).
Stdlib only (`zipfile`), no third-party dependency.
"""
from __future__ import annotations

import os
import zipfile

FIXTURE_DIR = os.path.dirname(os.path.abspath(__file__))

CONTENT_TYPES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
</Types>"""

ROOT_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"""

WORKBOOK_XML = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>
</workbook>"""

WORKBOOK_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"""

# Row/cell layout — see the table in the module docstring above.
CACHED_SHEET_XML = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData>
<row r="1"><c r="A1"><v>2</v></c></row>
<row r="2"><c r="A2"><v>3</v></c></row>
<row r="3"><c r="A3"><f>SUM(A1:A2)</f><v>5</v></c></row>
<row r="4"><c r="A4"><f>SUM(A1:A2)</f><v>999</v></c></row>
<row r="5"><c r="A5"><f>NOTAREALFN(A1)</f><v>42</v></c></row>
<row r="6"><c r="A6"><f>SUM(A1:A2)</f></c></row>
</sheetData>
</worksheet>"""

# The mismatch-free counterpart: one formula cell whose cached value the
# engine reproduces exactly.
CLEAN_SHEET_XML = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData>
<row r="1"><c r="A1"><v>2</v></c></row>
<row r="2"><c r="A2"><v>3</v></c></row>
<row r="3"><c r="A3"><f>SUM(A1:A2)</f><v>5</v></c></row>
</sheetData>
</worksheet>"""


def build(sheet_xml: str) -> bytes:
    import io

    buf = io.BytesIO()
    # Fixed member order + no timestamps/permissions variance (ZipInfo
    # defaults to a fixed 1980-01-01 date_time unless set) keeps repeated
    # runs of this script byte-identical.
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_STORED) as zf:
        for name, content in [
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", ROOT_RELS),
            ("xl/workbook.xml", WORKBOOK_XML),
            ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
            ("xl/worksheets/sheet1.xml", sheet_xml),
        ]:
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            zf.writestr(info, content)
    return buf.getvalue()


def main() -> int:
    for filename, sheet_xml in [
        ("cached_values.xlsx", CACHED_SHEET_XML),
        ("clean_values.xlsx", CLEAN_SHEET_XML),
    ]:
        out_path = os.path.join(FIXTURE_DIR, filename)
        data = build(sheet_xml)
        with open(out_path, "wb") as f:
            f.write(data)
        print(f"Wrote {len(data)} bytes to {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
