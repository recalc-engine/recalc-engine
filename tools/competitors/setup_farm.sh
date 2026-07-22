#!/usr/bin/env bash
# Idempotent setup for the competitor-measurement harness on the measurement
# host (the ONLY host this setup touches).
#
# Installs:
#   * LibreOffice Calc headless (OS package; ships its own bundled Python + UNO,
#     which lo_dump.py runs inside — no python3-uno needed).
#   * A Python venv with the pure-Python competitor engines (pycel, formulas).
#
# The FUSE corpus ($CORPUS_DIR) stays READ-ONLY; nothing here writes there.
# The Cargo workspace zero-dependency rule is untouched — these are OS/host tools.
#
# Usage (on the measurement host):
#   CORPUS_DIR=/path/to/corpus bash tools/competitors/setup_farm.sh   # install
#   bash tools/competitors/setup_farm.sh --check                      # verify
set -euo pipefail

VENV="${RECALC_COMP_VENV:-$HOME/.venvs/recalc-competitors}"
CORPUS_DIR="${CORPUS_DIR:-}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Privilege escalation: use sudo when present and not already root; run apt
# directly when root (a minimal Debian image may have no sudo). Empty SUDO
# means "run the command as-is".
if [ "$(id -u)" -eq 0 ]; then
  SUDO=""
elif command -v sudo >/dev/null 2>&1; then
  SUDO="sudo"
else
  echo "ERROR: not root and no sudo available; cannot install OS packages." >&2
  exit 1
fi

check() {
  echo "== checks =="
  command -v soffice >/dev/null 2>&1 && echo "soffice: $(command -v soffice)" || echo "soffice: MISSING"
  if [ -x "$VENV/bin/python" ]; then
    echo "venv python: $VENV/bin/python"
    "$VENV/bin/python" - <<'PY' 2>&1 | sed 's/^/  /' || true
try:
    import pycel, formulas, numpy, openpyxl
    print("pycel", getattr(pycel, "__version__", "?"), "| formulas ok | numpy", numpy.__version__)
except Exception as e:
    print("engine import FAILED:", e)
PY
  else
    echo "venv: MISSING ($VENV)"
  fi
  if [ -n "$CORPUS_DIR" ]; then
    echo "corpus: $(ls "$CORPUS_DIR" 2>/dev/null | wc -l) workbooks under $CORPUS_DIR"
  else
    echo "corpus: set \$CORPUS_DIR to point at the corpus root"
  fi
}

if [ "${1:-}" = "--check" ]; then
  check
  exit 0
fi

echo "== installing LibreOffice (headless) =="
if ! command -v soffice >/dev/null 2>&1; then
  # `apt-get update` is best-effort: some enterprise apt repos 401 without a
  # subscription, which must not abort the whole setup — the Debian main repo
  # (which carries libreoffice) still refreshes. The install below stays fatal,
  # so a genuinely missing package still fails loudly.
  $SUDO apt-get update -y || echo "WARN: apt-get update had errors (continuing; install will fail loudly if a package is truly missing)"
  # libreoffice-calc pulls the Calc app + bundled python-uno scripting provider.
  $SUDO apt-get install -y --no-install-recommends \
    libreoffice-calc libreoffice-script-provider-python
else
  echo "soffice already present: $(command -v soffice)"
fi

echo "== ensuring python venv support =="
# Debian ships `venv`/`ensurepip` in a separate package (python3-venv, which
# pulls the version-specific python3.N-venv). Without it `python3 -m venv`
# fails with "ensurepip is not available". Idempotent: only install if missing.
if ! python3 -c 'import ensurepip' >/dev/null 2>&1; then
  $SUDO apt-get install -y --no-install-recommends python3-venv
fi

echo "== creating venv $VENV =="
python3 -m venv "$VENV"
"$VENV/bin/pip" install --upgrade pip >/dev/null
"$VENV/bin/pip" install -r "$HERE/requirements.txt"

echo
check
echo
echo "Done. Run a smoke with:"
echo "  python3 $HERE/harness.py --engine libreoffice --soffice \$(command -v soffice) \\"
echo "      --sample-dir \"\$CORPUS_DIR\" --n 50 --out /tmp/libreoffice-smoke.json"
echo "  python3 $HERE/harness.py --engine pycel --engine-python $VENV/bin/python \\"
echo "      --sample-dir \"\$CORPUS_DIR\" --n 50 --out /tmp/pycel-smoke.json"
