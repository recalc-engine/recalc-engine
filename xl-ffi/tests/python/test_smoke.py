"""Python smoke test for the `recalc` extension (the PyO3 binding in xl-ffi).

Runs against a REAL, committed `.xlsx` fixture — `xl-bench`'s
`cached_values.xlsx` (see `xl-bench/tests/fixtures/README.md` for its exact,
hand-authored contents). No workbook bytes are invented here.

Build + run:
    python3 -m venv .venv
    .venv/bin/pip install maturin pytest
    .venv/bin/maturin develop --features python -m xl-ffi/Cargo.toml
    .venv/bin/pytest xl-ffi/tests/python -q

Fixture `Sheet1` (post-recalc):
    A1 = 2      (literal)                 -> value(0,0) == 2.0
    A2 = 3      (literal)
    A3 = SUM(A1:A2) == 5                  -> value(2,0) == 5.0
    A4 = SUM(A1:A2) == 5 (cached 999)     -> recompute overwrites to 5.0
    A5 = NOTAREALFN(A1) -> #UNSUPPORTED!  -> CellError, diagnostic UnknownFunction
    A6 = SUM(A1:A2)  (no cached value)
"""

from __future__ import annotations

import pathlib

import pytest

import recalc


def _fixture() -> pathlib.Path:
    here = pathlib.Path(__file__).resolve()
    for base in [here, *here.parents]:
        cand = base / "xl-bench" / "tests" / "fixtures" / "cached_values.xlsx"
        if cand.is_file():
            return cand
    raise FileNotFoundError("could not locate xl-bench cached_values.xlsx fixture")


def _loaded():
    wb = recalc.open(str(_fixture()))
    wb.recalc()
    return wb


def test_sheet_names_non_empty():
    wb = _loaded()
    names = wb.sheet_names()
    assert names, "sheet_names() must be non-empty"
    assert names == ["Sheet1"]


def test_literal_and_formula_values_are_floats():
    wb = _loaded()
    a1 = wb.value("Sheet1", 0, 0)
    a3 = wb.value("Sheet1", 2, 0)
    assert isinstance(a1, float) and a1 == 2.0
    assert isinstance(a3, float) and a3 == 5.0
    # A4's poisoned cached 999 is overwritten by recompute (== 5).
    assert wb.value("Sheet1", 3, 0) == 5.0


def test_cell_a1_convenience_matches_value():
    wb = _loaded()
    assert wb.cell("Sheet1", "A3") == wb.value("Sheet1", 2, 0) == 5.0
    assert wb.cell("Sheet1", "A1") == 2.0


def test_error_cell_is_a_distinguishable_cellerror():
    wb = _loaded()
    a5 = wb.value("Sheet1", 4, 0)
    assert isinstance(a5, recalc.CellError), f"expected CellError, got {a5!r}"
    # Distinguishable from a str: an error is NOT the bare code string.
    assert not isinstance(a5, str)
    assert a5.code == "#UNSUPPORTED!"
    assert str(a5) == "#UNSUPPORTED!"
    assert "CellError" in repr(a5)


def test_diagnostics_surface_the_unsupported_cell():
    wb = _loaded()
    diags = wb.diagnostics()
    assert diags, "diagnostics() must report the NOTAREALFN cell"
    a5 = [d for d in diags if d.sheet == "Sheet1" and d.row == 4 and d.col == 0]
    assert a5, "expected a diagnostic at Sheet1!A5 (row 4, col 0)"
    assert a5[0].kind == "UnknownFunction"
    assert a5[0].message


def test_diagnostics_queryable_before_recalc():
    # diagnostics() must be callable on a freshly-opened workbook (no recalc).
    # This fixture's only refusal (NOTAREALFN) is an EVAL-time UnknownFunction,
    # so it is absent pre-recalc and present post-recalc. The *load-time* refusal
    # path (parse errors / unsupported constructs surfaced at open, before any
    # recalc) is proven by the xl-engine test
    # `load_time_refusals_surface_before_recalc`.
    wb = recalc.open(str(_fixture()))
    pre = wb.diagnostics()  # must not raise
    assert all(d.kind != "UnknownFunction" for d in pre)
    wb.recalc()
    post = wb.diagnostics()
    assert any(d.kind == "UnknownFunction" for d in post)


def test_blank_never_populated_cell_is_none():
    wb = _loaded()
    assert wb.value("Sheet1", 500, 500) is None


def test_malformed_a1_raises_value_error():
    wb = _loaded()
    for bad in ["", "1", "A", "A0", "A1:B2", "Sheet1!A1", "A 1"]:
        with pytest.raises(ValueError):
            wb.cell("Sheet1", bad)


def test_unknown_sheet_raises_key_error():
    wb = _loaded()
    with pytest.raises(KeyError):
        wb.value("NoSuchSheet", 0, 0)


def test_open_bytes_matches_open():
    data = _fixture().read_bytes()
    wb = recalc.open_bytes(data)
    wb.recalc()
    assert wb.sheet_names() == ["Sheet1"]
    assert wb.value("Sheet1", 2, 0) == 5.0
