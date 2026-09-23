#!/usr/bin/env bash
# Launch the Kokoro stack-check GUI. Dependencies, the Python version, and the
# managed-CPython / copy-link-mode settings all live in pyproject.toml + uv.lock; `uv run`
# creates .venv and syncs them from the lock on first use. No sudo, no system packages.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export PATH="$HOME/.local/bin:$PATH"

if ! command -v uv >/dev/null 2>&1; then
  echo "Installing uv…"
  curl -LsSf https://astral.sh/uv/install.sh | sh
  export PATH="$HOME/.local/bin:$PATH"
fi

cd "$HERE"
exec uv run stack_check.py
