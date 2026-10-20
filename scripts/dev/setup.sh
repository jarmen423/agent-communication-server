#!/usr/bin/env bash
# One-time dev setup: nats-server into .tools/bin, Python venv in .venv.
# Idempotent — safe to re-run. No sudo.
#
# Usage: scripts/dev/setup.sh [--extras]
set -euo pipefail
source "$(dirname "$0")/lib.sh"
cd "$REPO_ROOT"

extras=0
for a in "$@"; do
  case "$a" in
    --extras) extras=1 ;;
    *) echo "unknown flag: $a" >&2; exit 1 ;;
  esac
done

"$REPO_ROOT/scripts/dev/install_nats_server.sh"

echo "==> python venv (.venv)"
if command -v uv >/dev/null 2>&1; then
  [[ -d .venv ]] || uv venv --quiet .venv
  uv pip install --quiet --python .venv/bin/python -r requirements-dev.txt
  (( extras )) && uv pip install --quiet --python .venv/bin/python -r requirements-extras.txt
else
  [[ -d .venv ]] || python3 -m venv .venv
  .venv/bin/python -m pip install --quiet --upgrade pip
  .venv/bin/python -m pip install --quiet -r requirements-dev.txt
  (( extras )) && .venv/bin/python -m pip install --quiet -r requirements-extras.txt
fi
echo "    $(.venv/bin/python --version) ready"

echo
"$REPO_ROOT/scripts/dev/doctor.sh"
