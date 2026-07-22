"""Oracle extraction for the competitor-measurement harness.

The oracle here is IDENTICAL to the one `xl-bench`'s `verify-dir` uses: every
real `.xlsx`/`.xlsm` carries Excel's own last-computed value in each formula
cell's cached ``<v>`` element, and that cached value is the "expected" value we
score every engine against (recalc AND the competitors). This module is a
faithful, stdlib-only re-implementation of the two Rust pieces that define that
oracle:

  * ``xl-io``'s ``sheet_xml::resolve_value`` — how a ``<c>``'s ``t`` attribute +
    ``<v>``/``<is>`` text becomes a typed value (number / text / bool / error /
    blank); and
  * ``xl-bench``'s ``sidecar::CachedValueSource`` — a formula cell whose cached
    value resolves to ``Blank`` (a ``<c>`` with an ``<f>`` but no sibling
    ``<v>``) has *no oracle* and must be reported ``no_oracle``, never scored.

Keeping this a byte-for-byte behavioural mirror of the Rust path is what makes
the competitor numbers apples-to-apples with recalc's own ``verify-dir`` funnel.
Any divergence here is a fidelity-of-comparison bug, not a competitor result.

Stdlib only (zipfile + xml.etree) — the Cargo zero-dependency rule does not
apply to VM tooling, but there is no reason to take a dependency for this.

The "formula cell" universe (the denominator) is exactly ``xl-io``'s: every
``<c>`` that has an ``<f>`` child element, shared/array follow-ons included
(they carry no formula text of their own but DO carry a cached ``<v>``). This
matches ``report.rs``, which counts a cell iff ``cell.formula.is_some()``.
"""
from __future__ import annotations

import re
import struct
import xml.etree.ElementTree as ET
import zipfile
from dataclasses import dataclass

# Zip-bomb guard, mirroring tools/corpus/_xlsxutil.py and xl-io's caps in
# spirit: refuse any member whose declared uncompressed size is absurd.
MAX_MEMBER_SIZE = 300 * 1024 * 1024

_MAIN_NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
_REL_NS = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
_PKG_REL_NS = "http://schemas.openxmlformats.org/package/2006/relationships"

# Excel's built-in error spellings, mirroring xl-value::ErrorKind::as_str and
# xl-io::sheet_xml::parse_error_kind. The three recalc sentinels
# (#UNSUPPORTED!/#BLOCKED!/#RESOURCE!) are DELIBERATELY absent: genuine Excel
# never writes them into a file, so they can never appear in an oracle cached
# value. parse_error_kind maps any unrecognised string to Unsupported; we keep
# the raw string here so a canonical round-trip stays exact for the known set.
_KNOWN_ERRORS = {
    "#NULL!",
    "#DIV/0!",
    "#VALUE!",
    "#REF!",
    "#NAME?",
    "#NUM!",
    "#N/A",
    "#GETTING_DATA",
    "#SPILL!",
    "#CALC!",
}


class OracleLoadError(Exception):
    """The workbook could not be parsed to extract its oracle.

    Treated exactly as ``xl-bench verify-dir`` treats an ``xl-io`` load failure:
    the workbook contributes ZERO cells to the funnel and is counted separately
    as a load failure (it never inflates or deflates any engine's fidelity).
    """


# --- typed values -----------------------------------------------------------

# A resolved oracle value is one of the scalar Excel types. We tag it exactly
# like xl_value::Value's scalar variants that a cached <v> can produce.
NUMBER = "number"
TEXT = "text"
BOOL = "bool"
ERROR = "error"
BLANK = "blank"


@dataclass(frozen=True)
class OVal:
    """A scalar Excel value: (kind, payload).

    * number -> payload is a float
    * text   -> payload is a str (compared case-SENSITIVELY downstream, per
                diff.rs's deliberate departure from Excel's `=` operator)
    * bool   -> payload is a bool
    * error  -> payload is the canonical Excel error string, e.g. "#DIV/0!"
    * blank  -> payload is None
    """

    kind: str
    payload: object = None

    @staticmethod
    def number(x: float) -> "OVal":
        return OVal(NUMBER, float(x))

    @staticmethod
    def text(s: str) -> "OVal":
        return OVal(TEXT, s)

    @staticmethod
    def boolean(b: bool) -> "OVal":
        return OVal(BOOL, bool(b))

    @staticmethod
    def error(s: str) -> "OVal":
        return OVal(ERROR, s)

    @staticmethod
    def blank() -> "OVal":
        return OVal(BLANK, None)


# An oracle record is either a value or the explicit "no oracle" signal. We use
# ``None`` to mean NoOracle (mirrors OracleRecord::NoOracle).
OracleRecord = "OVal | None"


@dataclass
class FormulaCell:
    sheet: str
    row: int  # 0-based
    col: int  # 0-based
    oracle: object  # OVal, or None for NoOracle


# --- A1 parsing (mirror xl-io cellref::parse_a1) ----------------------------

_A1_RE = re.compile(r"^\$?([A-Za-z]{1,3})\$?([0-9]+)$")


def parse_a1(ref: str) -> tuple[int, int]:
    """`"B7"` -> (row=6, col=1), both 0-based. Raises on malformed refs."""
    m = _A1_RE.match(ref)
    if not m:
        raise OracleLoadError(f"malformed cell ref {ref!r}")
    letters, digits = m.group(1).upper(), m.group(2)
    col = 0
    for ch in letters:
        col = col * 26 + (ord(ch) - ord("A") + 1)
    col -= 1
    row = int(digits) - 1
    if row < 0 or col < 0:
        raise OracleLoadError(f"out-of-range cell ref {ref!r}")
    return row, col


# --- xsd bool + error parsing (mirror xl-io) --------------------------------


def parse_xsd_bool(text: str) -> bool:
    t = text.strip()
    if t in ("1", "true", "TRUE", "True"):
        return True
    if t in ("0", "false", "FALSE", "False"):
        return False
    # xl-io's parse_xsd_bool is stricter; anything else is a structural error.
    raise OracleLoadError(f"invalid xsd:boolean {text!r}")


def canonical_error(text: str) -> str:
    """Canonicalise a cached error string. Known Excel errors round-trip
    verbatim; anything else mirrors xl-io mapping unknown -> Unsupported, which
    genuine Excel never writes, so we surface it as the raw string tagged as an
    error the engine will (correctly) never match."""
    t = text.strip()
    if t in _KNOWN_ERRORS:
        return t
    # Unknown string in a cached <v t="e">. xl-io maps this to #UNSUPPORTED!.
    # Since a real Excel oracle never emits recalc sentinels, keep the raw text;
    # it simply won't equal any engine's real error output.
    return t


# --- resolve_value (mirror xl-io sheet_xml::resolve_value) ------------------


def resolve_value(
    cell_type: "str | None",
    v_text: "str | None",
    inline_text: "str | None",
    shared_strings: list,
) -> OVal:
    if inline_text is not None:
        return OVal.text(inline_text)
    if v_text is None:
        return OVal.blank()
    t = cell_type
    if t == "s":
        idx = int(v_text)
        if idx < 0 or idx >= len(shared_strings):
            raise OracleLoadError(f"shared string index {idx} out of range")
        return OVal.text(shared_strings[idx])
    if t in ("str", "inlineStr"):
        return OVal.text(v_text)
    if t == "b":
        return OVal.boolean(parse_xsd_bool(v_text))
    if t == "e":
        return OVal.error(canonical_error(v_text))
    if t == "d":
        # xl-io maps ISO-date typed cells to #UNSUPPORTED!; extremely rare in
        # cached formula results. Represent as a non-matchable error string.
        return OVal.error("#UNSUPPORTED!")
    if t in ("n", None):
        return OVal.number(float(v_text.strip()))
    raise OracleLoadError(f"unrecognised cell type t={t!r}")


# --- zip/XML plumbing -------------------------------------------------------


def _local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1] if "}" in tag else tag


def _read_member(zf: zipfile.ZipFile, name: str) -> bytes:
    try:
        info = zf.getinfo(name)
    except KeyError as e:
        raise OracleLoadError(f"missing part {name}") from e
    if info.file_size > MAX_MEMBER_SIZE:
        raise OracleLoadError(f"part {name} exceeds size cap ({info.file_size})")
    return zf.read(name)


def _parse_xml(data: bytes) -> ET.Element:
    # ElementTree does not resolve external entities and does not fetch external
    # DTDs; that covers the XXE surface xl-io hardens against for this trusted,
    # read-only corpus. A malformed part raises, caught by callers as a load
    # failure.
    try:
        return ET.fromstring(data)
    except ET.ParseError as e:
        raise OracleLoadError(f"XML parse error: {e}") from e


def _read_shared_strings(zf: zipfile.ZipFile) -> list:
    if "xl/sharedStrings.xml" not in zf.namelist():
        return []
    root = _parse_xml(_read_member(zf, "xl/sharedStrings.xml"))
    out = []
    for si in root:
        if _local(si.tag) != "si":
            continue
        # Concatenate every <t> descendant (plain + rich-run text), mirroring
        # xl-io::read_rich_text.
        parts = []
        for t in si.iter():
            if _local(t.tag) == "t":
                parts.append(t.text or "")
        out.append("".join(parts))
    return out


def _sheet_name_to_part(zf: zipfile.ZipFile) -> list:
    """Return [(display_name, worksheet_part_path)] in workbook tab order,
    worksheets only (chartsheets/dialog sheets skipped, mirroring the sheets
    xl-io surfaces)."""
    wb = _parse_xml(_read_member(zf, "xl/workbook.xml"))
    # r:id -> target from xl/_rels/workbook.xml.rels
    rels_name = "xl/_rels/workbook.xml.rels"
    rid_to_target = {}
    if rels_name in zf.namelist():
        rels = _parse_xml(_read_member(zf, rels_name))
        for rel in rels:
            if _local(rel.tag) != "Relationship":
                continue
            rid = rel.get("Id")
            target = rel.get("Target")
            rtype = rel.get("Type", "")
            if rid and target and rtype.endswith("/worksheet"):
                rid_to_target[rid] = target
    out = []
    for sheets in wb:
        if _local(sheets.tag) != "sheets":
            continue
        for sheet in sheets:
            if _local(sheet.tag) != "sheet":
                continue
            name = sheet.get("name")
            # r:id attribute (relationships ns) — match by local name.
            rid = None
            for k, v in sheet.attrib.items():
                if _local(k) == "id":
                    rid = v
                    break
            if name is None or rid is None:
                continue
            target = rid_to_target.get(rid)
            if target is None:
                continue  # not a worksheet (chartsheet etc.)
            part = _resolve_target(target)
            out.append((name, part))
    return out


def _resolve_target(target: str) -> str:
    """Resolve a workbook-rels Target to a package path. Targets are relative
    to xl/ (the workbook part's folder); absolute targets (leading /) are taken
    from the package root."""
    if target.startswith("/"):
        return target[1:]
    # Strip any ../ then anchor at xl/
    t = target
    base = "xl/"
    while t.startswith("../"):
        t = t[3:]
        base = ""  # ../ from xl/ lands at package root
    return base + t


def _iter_formula_cells(zf: zipfile.ZipFile, part: str, shared_strings: list):
    """Yield (row0, col0, OracleRecord) for every <c> with an <f> child.

    OracleRecord is an OVal, or None for NoOracle (cached value resolves to
    blank — the sidecar.rs rule)."""
    root = _parse_xml(_read_member(zf, part))
    for sheetdata in root:
        if _local(sheetdata.tag) != "sheetData":
            continue
        for row in sheetdata:
            if _local(row.tag) != "row":
                continue
            for c in row:
                if _local(c.tag) != "c":
                    continue
                has_f = False
                v_text = None
                inline_text = None
                for child in c:
                    lc = _local(child.tag)
                    if lc == "f":
                        has_f = True
                    elif lc == "v":
                        v_text = child.text if child.text is not None else ""
                    elif lc == "is":
                        # inline string: concat <t> descendants
                        parts = []
                        for t in child.iter():
                            if _local(t.tag) == "t":
                                parts.append(t.text or "")
                        inline_text = "".join(parts)
                if not has_f:
                    continue
                ref = c.get("r")
                if ref is None:
                    # xl-io requires r; treat as structural failure of the book.
                    raise OracleLoadError("<c> missing required r attribute")
                row0, col0 = parse_a1(ref)
                cell_type = c.get("t")
                val = resolve_value(cell_type, v_text, inline_text, shared_strings)
                # sidecar.rs: a formula cell whose cached value is Blank has no
                # oracle. (Lambda is not producible from a cached <v>.)
                record = None if val.kind == BLANK else val
                yield row0, col0, record


def extract_oracle(path: str) -> list:
    """Open `path`, return a list of FormulaCell (every <f>-bearing cell) with
    its oracle record. Raises OracleLoadError on any parse failure — the caller
    treats that exactly as xl-bench treats an xl-io load failure."""
    try:
        zf = zipfile.ZipFile(path)
    except (zipfile.BadZipFile, OSError) as e:
        raise OracleLoadError(f"cannot open zip: {e}") from e
    with zf:
        shared = _read_shared_strings(zf)
        sheets = _sheet_name_to_part(zf)
        cells = []
        for name, part in sheets:
            for row0, col0, record in _iter_formula_cells(zf, part, shared):
                cells.append(FormulaCell(sheet=name, row=row0, col=col0, oracle=record))
        return cells


def col_to_letters(col: int) -> str:
    """0-based column -> letters (0 -> "A", 26 -> "AA"). Mirrors xl-io."""
    s = ""
    n = col + 1
    while n > 0:
        n, rem = divmod(n - 1, 26)
        s = chr(ord("A") + rem) + s
    return s


def a1_ref(row: int, col: int) -> str:
    """0-based (row, col) -> A1 string (e.g. (6, 1) -> "B7")."""
    return f"{col_to_letters(col)}{row + 1}"


def cell_key(sheet: str, row: int, col: int) -> str:
    return f"{sheet}\x1f{row}\x1f{col}"


def bits(x: float) -> bytes:
    return struct.pack("<d", x)
