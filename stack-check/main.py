#!/usr/bin/env python3
"""Launch the Kokoro stack-check GUI, on Windows and Linux alike.

    python stack-check/main.py         (python3 on Linux)

Run it with ANY Python 3 -- the system one is fine, and needs no packages. It does not run the
app itself: it finds uv (installing it into ~/.local/bin if it is missing) and hands over to
`uv run stack_check.py`, which fetches the managed CPython 3.12 and syncs .venv from uv.lock on
first use. Dependencies, the Python version and the uv settings all live in pyproject.toml +
uv.lock; nothing here duplicates them. No admin, no sudo, no system packages.

One launcher rather than a run.sh and a run.cmd: the two only ever did this, and a shell twin
per platform is a pair that has to be kept in step by hand.

Keep this file to the standard library and to syntax an old system Python accepts: it runs
BEFORE uv has provided the interpreter the app needs.
"""

import os
import shutil
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
WINDOWS = os.name == "nt"
# Astral's official installers. Both put uv in ~/.local/bin (%USERPROFILE%\.local\bin).
INSTALLER_URL = "https://astral.sh/uv/install.ps1" if WINDOWS else "https://astral.sh/uv/install.sh"
UV_EXE = "uv.exe" if WINDOWS else "uv"


def find_uv():
    """uv on PATH, else where its installer puts it (a fresh install is not on PATH yet)."""
    found = shutil.which("uv")
    if found:
        return found
    for d in (os.environ.get("UV_INSTALL_DIR"), os.environ.get("XDG_BIN_HOME"),
              str(Path.home() / ".local" / "bin")):
        if d and (Path(d) / UV_EXE).is_file():
            return str(Path(d) / UV_EXE)
    return None


def install_uv():
    """Download Astral's installer and run it -- no curl or irm needed, just this Python."""
    print("Installing uv...", flush=True)
    with tempfile.TemporaryDirectory() as tmp:
        script = Path(tmp) / Path(INSTALLER_URL).name
        with urllib.request.urlopen(INSTALLER_URL, timeout=60) as r:
            script.write_bytes(r.read())
        if WINDOWS:
            cmd = ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass",
                   "-File", str(script)]
        else:
            cmd = ["sh", str(script)]
        subprocess.run(cmd, check=True)


def main():
    uv = find_uv()
    if uv is None:
        try:
            install_uv()
        except (OSError, subprocess.CalledProcessError) as e:
            sys.exit("Could not install uv (%s). Install it by hand - see "
                     "https://docs.astral.sh/uv/ - and re-run." % e)
        uv = find_uv()
        if uv is None:
            sys.exit("uv was installed but cannot be found; open a new shell and re-run.")
    # A child rather than os.exec*: on Windows exec spawns a new process and returns at once,
    # so the console would get its prompt back while the GUI is still running.
    try:
        rc = subprocess.run([uv, "run", "stack_check.py"], cwd=str(HERE)).returncode
    except KeyboardInterrupt:
        rc = 130
    sys.exit(rc)


if __name__ == "__main__":
    main()
