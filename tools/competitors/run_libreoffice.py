#!/usr/bin/env python3
"""LibreOffice runner (orchestrator) — measures headless `soffice` on one book.

Invoked by harness.py in an isolated subprocess:
    python run_libreoffice.py <wb> <out> [--soffice soffice] [--lo-timeout 110]

Design (crash isolation): this launches ONE dedicated, headless soffice instance
with its own throwaway UserInstallation profile, runs the `lo_dump.py` macro
inside it (which force-recalculates and writes the runner JSON to <out>), then
tears the instance down — always, even on timeout/exception. The macro is placed
in the profile's user Scripts/python dir and addressed via a documented
`vnd.sun.star.script:` URL; the target/out paths are passed through the
environment. No LibreOffice source is read — public UNO API only.

Failure handling:
  * soffice not found / macro import error / no output -> exit 1 (the harness
    marks this an engine workbook failure and declines every oracle-bearing
    cell — never a silent skip).
  * internal --lo-timeout elapses -> kill the soffice process group, exit 1.

soffice is single-instance per UserInstallation; a unique profile per invocation
lets many run in parallel and avoids stale lockfiles.
"""
from __future__ import annotations

import argparse
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
MACRO_SRC = os.path.join(HERE, "lo_dump.py")

SCRIPT_URL = (
    "vnd.sun.star.script:lo_dump.py$dump?language=Python&location=user"
)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("workbook")
    ap.add_argument("out")
    ap.add_argument("--soffice", default=os.environ.get("RECALC_SOFFICE", "soffice"))
    ap.add_argument("--lo-timeout", type=float, default=110.0)
    args = ap.parse_args()

    soffice = shutil.which(args.soffice) or args.soffice
    if not os.path.exists(soffice) and shutil.which(args.soffice) is None:
        sys.stderr.write(f"soffice not found: {args.soffice}\n")
        return 1

    # Throwaway profile with the dump macro installed.
    profile = tempfile.mkdtemp(prefix="lo_prof_")
    scripts_dir = os.path.join(profile, "user", "Scripts", "python")
    os.makedirs(scripts_dir, exist_ok=True)
    shutil.copyfile(MACRO_SRC, os.path.join(scripts_dir, "lo_dump.py"))

    # Ensure a fresh output (macro overwrites; remove any stale file).
    try:
        os.unlink(args.out)
    except OSError:
        pass

    env = dict(os.environ)
    env["RECALC_LO_TARGET"] = os.path.abspath(args.workbook)
    env["RECALC_LO_OUT"] = os.path.abspath(args.out)
    # Force a stable, en-US-ish environment (recalc's v1 scope is en-US only).
    env.setdefault("LC_ALL", "C")
    env["HOME"] = profile  # keep any stray dotfiles inside the throwaway profile

    profile_url = "file://" + profile
    cmd = [
        soffice,
        "--headless",
        "--invisible",
        "--norestore",
        "--nologo",
        "--nodefault",
        "--nofirststartwizard",
        "--nolockcheck",
        f"-env:UserInstallation={profile_url}",
        SCRIPT_URL,
    ]

    rc = 1
    proc = None
    try:
        proc = subprocess.Popen(
            cmd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
            start_new_session=True,  # own process group -> clean group kill
        )
        deadline = time.time() + args.lo_timeout
        while True:
            if proc.poll() is not None:
                break
            # The macro terminates the desktop when done; as a belt-and-braces
            # signal, also stop as soon as a well-formed output file appears.
            if os.path.exists(args.out) and os.path.getsize(args.out) > 0:
                # give soffice a brief grace to terminate itself
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    pass
                break
            if time.time() > deadline:
                sys.stderr.write("lo_dump timed out\n")
                break
            time.sleep(0.2)

        if os.path.exists(args.out) and os.path.getsize(args.out) > 0:
            rc = 0
        else:
            err = b""
            if proc and proc.stderr:
                try:
                    err = proc.stderr.read() or b""
                except Exception:
                    err = b""
            sys.stderr.write("no LibreOffice output produced: "
                             + err.decode("utf-8", "replace")[-500:] + "\n")
            rc = 1
    finally:
        if proc is not None and proc.poll() is None:
            try:
                os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
        shutil.rmtree(profile, ignore_errors=True)

    return rc


if __name__ == "__main__":
    sys.exit(main())
