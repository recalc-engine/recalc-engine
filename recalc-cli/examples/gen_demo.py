#!/usr/bin/env python3
"""Generates `demo.xlsx` and `demo-stale.xlsx`, the two small workbooks that
ship with the `recalc` command-line archive for the `recalc verify`
walkthrough in `README.md` (next to this script).

Both workbooks hold one sheet, `Budget`, a small quarterly budget:

| Cell   | Content                                        | Cached value      |
|--------|------------------------------------------------|-------------------|
| A1:F1  | header row (text)                              | n/a               |
| A2:A5  | line-item labels (text)                        | n/a               |
| B2:E5  | Q1..Q4 inputs (numbers)                        | n/a               |
| F2:F5  | `=SUM(B2:E2)` .. `=SUM(B5:E5)`                 | 49000 12000 8100 3500 |
| A6     | `Total` (text)                                 | n/a               |
| B6:E6  | `=SUM(B2:B5)` .. `=SUM(E2:E5)`                 | 17300 18000 18250 19050 |
| F6     | `=SUM(F2:F5)`                                  | 72600             |
| B7     | `=AVERAGE(B6:E6)`                              | 18150             |
| B8     | `=MAX(B6:E6)`                                  | 19050             |
| B9     | annual budget input (number)                   | 75000 (stale: 70000) |
| B10    | `=IF(F6>B9,"Over budget","Within budget")`     | `Within budget`   |
| B11    | `=ROUND(F6/B9*100,1)`                          | 96.8              |

`demo.xlsx` carries a correct cached `<v>` for every formula cell, so
`recalc verify` passes against the stored values.

`demo-stale.xlsx` is the same workbook after an edit that changed the annual
budget input (B9: 75000 -> 70000) without a recalculation. The two cells that
depend on B9 (B10 and B11) still carry the old cached values, so
`recalc verify` fails and reports them as `differs_stored`.

Packaging follows `xl-bench/tests/fixtures/gen_fixture.py`: a fixed zip
member order, `ZIP_STORED`, and fixed `1980-01-01` `ZipInfo` timestamps, so
each run of this script writes byte-identical files. Text inputs are inline
strings (`t="inlineStr"` with `<is><t>`), and text formula results are
`t="str"` with `<v>`; the package has no shared-string table.

Run: `python3 gen_demo.py` (writes both files next to this script).
Stdlib only (`zipfile`), no third-party dependency.
"""
from __future__ import annotations

import io
import os
import zipfile
from xml.sax.saxutils import escape

OUT_DIR = os.path.dirname(os.path.abspath(__file__))

SHEET_NAME = "Budget"

CONTENT_TYPES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"""

ROOT_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"""

WORKBOOK_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="{SHEET_NAME}" sheetId="1" r:id="rId1"/></sheets>
</workbook>"""

WORKBOOK_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"""

# Line items: label, then the four quarterly inputs (Q1..Q4).
LINE_ITEMS = [
    ("Salaries", (12000, 12000, 12500, 12500)),
    ("Rent", (3000, 3000, 3000, 3000)),
    ("Marketing", (1500, 2200, 1800, 2600)),
    ("Software", (800, 800, 950, 950)),
]

ANNUAL_BUDGET = 75000
# The edited input in `demo-stale.xlsx`. B10 and B11 depend on it, and
# their cached values below are NOT updated in the stale workbook.
ANNUAL_BUDGET_STALE = 70000

# Cached values of every formula cell in `demo.xlsx`, as the engine computes
# them from the inputs above. Numbers are written as plain `<v>` text; text
# results are written with `t="str"`.
CACHED = {
    "F2": 49000,
    "F3": 12000,
    "F4": 8100,
    "F5": 3500,
    "B6": 17300,
    "C6": 18000,
    "D6": 18250,
    "E6": 19050,
    "F6": 72600,
    "B7": 18150,
    "B8": 19050,
    "B10": "Within budget",
    "B11": 96.8,
}


def text_cell(ref: str, text: str) -> str:
    """A literal text input as an inline string."""
    return f'<c r="{ref}" t="inlineStr"><is><t>{escape(text)}</t></is></c>'


def number_cell(ref: str, value: float) -> str:
    return f'<c r="{ref}"><v>{fmt(value)}</v></c>'


def formula_cell(ref: str, formula: str, cached) -> str:
    """A formula cell with its cached result (`cached` may be None to omit
    the `<v>` element, which `recalc verify` reports as
    `evidence_unavailable`)."""
    f = escape(formula)
    if cached is None:
        return f'<c r="{ref}"><f>{f}</f></c>'
    if isinstance(cached, str):
        return f'<c r="{ref}" t="str"><f>{f}</f><v>{escape(cached)}</v></c>'
    return f'<c r="{ref}"><f>{f}</f><v>{fmt(cached)}</v></c>'


def fmt(value: float) -> str:
    """Shortest round-trip decimal text of a number (`repr`), with a bare
    integer for whole values (`18150`, not `18150.0`)."""
    if float(value).is_integer():
        return str(int(value))
    return repr(float(value))


def sheet_xml(annual_budget: float, cached: dict) -> str:
    cols = "BCDEF"
    rows: list[str] = []
    # Row 1: header.
    rows.append(
        "".join(
            [
                text_cell("A1", "Line item"),
                text_cell("B1", "Q1"),
                text_cell("C1", "Q2"),
                text_cell("D1", "Q3"),
                text_cell("E1", "Q4"),
                text_cell("F1", "Total"),
            ]
        )
    )
    # Rows 2..5: line items with a per-row SUM in column F.
    for i, (label, quarters) in enumerate(LINE_ITEMS):
        r = i + 2
        cells = [text_cell(f"A{r}", label)]
        for col, q in zip(cols, quarters):
            cells.append(number_cell(f"{col}{r}", q))
        cells.append(formula_cell(f"F{r}", f"SUM(B{r}:E{r})", cached.get(f"F{r}")))
        rows.append("".join(cells))
    # Row 6: column totals.
    last = len(LINE_ITEMS) + 1
    cells = [text_cell("A6", "Total")]
    for col in cols:
        cells.append(formula_cell(f"{col}6", f"SUM({col}2:{col}{last})", cached.get(f"{col}6")))
    rows.append("".join(cells))
    # Rows 7..11: summary block.
    rows.append(text_cell("A7", "Average per quarter") + formula_cell("B7", "AVERAGE(B6:E6)", cached.get("B7")))
    rows.append(text_cell("A8", "Highest quarter") + formula_cell("B8", "MAX(B6:E6)", cached.get("B8")))
    rows.append(text_cell("A9", "Annual budget") + number_cell("B9", annual_budget))
    rows.append(
        text_cell("A10", "Status")
        + formula_cell("B10", 'IF(F6>B9,"Over budget","Within budget")', cached.get("B10"))
    )
    rows.append(
        text_cell("A11", "Budget used (%)") + formula_cell("B11", "ROUND(F6/B9*100,1)", cached.get("B11"))
    )
    body = "\n".join(f'<row r="{i + 1}">{r}</row>' for i, r in enumerate(rows))
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">\n'
        "<sheetData>\n" + body + "\n</sheetData>\n</worksheet>"
    )


def build(sheet: str) -> bytes:
    buf = io.BytesIO()
    # Fixed member order + fixed 1980-01-01 timestamps keep repeated runs of
    # this script byte-identical.
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_STORED) as zf:
        for name, content in [
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", ROOT_RELS),
            ("xl/workbook.xml", WORKBOOK_XML),
            ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
            ("xl/worksheets/sheet1.xml", sheet),
        ]:
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            zf.writestr(info, content)
    return buf.getvalue()


def workbooks() -> list[tuple[str, bytes]]:
    return [
        ("demo.xlsx", build(sheet_xml(ANNUAL_BUDGET, CACHED))),
        # Same cached values, edited input: B10 and B11 are now stale.
        ("demo-stale.xlsx", build(sheet_xml(ANNUAL_BUDGET_STALE, CACHED))),
    ]


def main() -> int:
    for filename, data in workbooks():
        out_path = os.path.join(OUT_DIR, filename)
        with open(out_path, "wb") as f:
            f.write(data)
        print(f"Wrote {len(data)} bytes to {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
