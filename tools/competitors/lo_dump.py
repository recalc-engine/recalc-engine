# -*- coding: utf-8 -*-
"""LibreOffice in-process dump macro (runs inside soffice's bundled Python).

Invoked by run_libreoffice.py via a ``vnd.sun.star.script:`` URL. Reads the
target workbook path and output JSON path from the environment
(``RECALC_LO_TARGET`` / ``RECALC_LO_OUT``), force-recalculates the whole
document (``calculateAll``), and writes this harness's runner-JSON contract:
one entry per FORMULA cell, typed number / bool / text / error / declined.

Clean-room note: this drives LibreOffice purely through its documented, public
UNO scripting API (com.sun.star.*). No LibreOffice/Gnumeric SOURCE is read.

Type mapping:
  * cell.getError() != 0     -> the Excel error string from getString() if it is
                                one of the 7 canonical (non-localized) Excel
                                errors; otherwise declined (an "Err:NNN"
                                LibreOffice-internal error with no Excel analog).
  * numeric result           -> number, UNLESS the cell's number format carries
                                the LOGICAL bit, in which case bool (recovers
                                TRUE/FALSE, which UNO otherwise exposes as 1/0).
  * string result            -> text
  * anything unexpected       -> declined (never guessed).
"""
import json
import os

import uno  # provided by soffice's bundled python
from com.sun.star.beans import PropertyValue
from com.sun.star.table.CellContentType import FORMULA

# com.sun.star.sheet.FormulaResult
_FR_VALUE = 1
_FR_STRING = 2
_FR_ERROR = 4
# com.sun.star.util.NumberFormat.LOGICAL
_NF_LOGICAL = 1024

_EXCEL_ERRORS = {
    "#NULL!", "#DIV/0!", "#VALUE!", "#REF!", "#NAME?", "#NUM!", "#N/A",
    "#GETTING_DATA", "#SPILL!", "#CALC!",
}


def _is_logical(doc, cell):
    try:
        fmt = doc.getNumberFormats().getByKey(cell.NumberFormat)
        return bool(fmt.Type & _NF_LOGICAL)
    except Exception:
        return False


def _classify_cell(doc, cell):
    try:
        if cell.getError() != 0:
            s = cell.getString()
            if s in _EXCEL_ERRORS:
                return {"k": "error", "v": s}
            return {"k": "declined"}
        # Determine the formula result type.
        try:
            frt = cell.FormulaResultType2
        except Exception:
            frt = _FR_VALUE if cell.getType() == FORMULA else _FR_STRING
        if frt == _FR_ERROR:
            s = cell.getString()
            return {"k": "error", "v": s} if s in _EXCEL_ERRORS else {"k": "declined"}
        if frt == _FR_STRING:
            return {"k": "text", "v": cell.getString()}
        # VALUE (default)
        if _is_logical(doc, cell):
            return {"k": "bool", "v": cell.getValue() != 0.0}
        return {"k": "number", "v": cell.getValue()}
    except Exception:
        return {"k": "declined"}


def dump(*args):
    target = os.environ["RECALC_LO_TARGET"]
    out = os.environ["RECALC_LO_OUT"]
    result = {"engine": "libreoffice", "workbook": target, "status": "ok", "cells": {}}

    ctx = uno.getComponentContext()
    smgr = ctx.ServiceManager
    desktop = smgr.createInstanceWithContext("com.sun.star.frame.Desktop", ctx)

    hidden = PropertyValue()
    hidden.Name = "Hidden"
    hidden.Value = True
    read_only = PropertyValue()
    read_only.Name = "ReadOnly"
    read_only.Value = True  # never modify the read-only corpus file
    url = uno.systemPathToFileUrl(os.path.abspath(target))

    doc = None
    try:
        doc = desktop.loadComponentFromURL(url, "_blank", 0, (hidden, read_only))
        doc.calculateAll()
        sheets = doc.Sheets
        for si in range(sheets.Count):
            sheet = sheets.getByIndex(si)
            name = sheet.Name
            cursor = sheet.createCursor()
            cursor.gotoEndOfUsedArea(False)
            ra = cursor.RangeAddress
            cells_out = {}
            for r in range(ra.EndRow + 1):
                for c in range(ra.EndColumn + 1):
                    cell = sheet.getCellByPosition(c, r)
                    if cell.getType() != FORMULA:
                        continue
                    cells_out["%d,%d" % (r, c)] = _classify_cell(doc, cell)
            if cells_out:
                result["cells"][name] = cells_out
    except Exception as e:  # whole-document failure
        result["status"] = "load_failure"
        result["message"] = "lo_dump exception: %r" % (e,)
    finally:
        if doc is not None:
            try:
                doc.close(False)
            except Exception:
                pass

    with open(out, "w") as f:
        json.dump(result, f)

    try:
        desktop.terminate()
    except Exception:
        pass
