#!/usr/bin/env bash
#
# install.sh — bootstrap a remote machine to join a nats-hub.
#
# Creates a Python venv, installs the minimal pip requirements, and copies
# the thin file set (see FILES.txt) into a target directory. Idempotent:
# re-running upgrades requirements in place and refreshes the copied
# files. Does NOT touch the hub or any daemon — you still launch the
# adapter yourself with the credential your operator gave you.
#
# USAGE
#   ./install.sh [target-dir]            # default: ~/nats-hub-remote
#   ./install.sh /path/to/dir
#
# After install, the script prints the exact `python3 remote_agent_adapter.py`
# command line you should fill in with your identity + credential.
#
# SECURITY
#   This script NEVER accepts or stores a token, password, .creds path,
#   or any other secret. Credentials are passed at adapter launch time
#   via env vars (NATS_TOKEN, NATS_USER, ...) or CLI flags. Do not edit
#   this file to bake a token in — see docs/REMOTE_INSTALL.md §Security.

set -euo pipefail

# ── Resolve args ────────────────────────────────────────────────────
TARGET_DIR="${1:-$HOME/nats-hub-remote}"

# Repo root = directory containing this script's parent (packaging/remote
# lives at <repo>/packaging/remote). We resolve upward until we find the
# adapter file, so the script works whether invoked from the repo or
# copied out of it alongside FILES.txt.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT=""
candidate="$SCRIPT_DIR"
for _ in 1 2 3 4 5; do
    if [[ -f "$candidate/remote_agent_adapter.py" ]]; then
        REPO_ROOT="$candidate"
        break
    fi
    candidate="$(dirname "$candidate")"
done

# ── Files to copy (mirror FILES.txt) ────────────────────────────────
# Kept in sync manually. If FILES.txt grows, add the path here too.
REMOTE_FILES=(
    "remote_agent_adapter.py"
    "nats_connect.py"
    "worker_runtime.py"
    "worker_events.py"
    "worker_backends/__init__.py"
    "worker_backends/headless_cli.py"
    "worker_backends/presets.py"
    "worker_backends/sdk_agent.py"
    "requirements-remote.txt"
)

# ── Preflight ───────────────────────────────────────────────────────
if ! command -v python3 >/dev/null 2>&1; then
    echo "ERROR: python3 not found on PATH. Install Python 3.10+ first." >&2
    exit 1
fi

if [[ -n "$REPO_ROOT" ]]; then
    : # found adapter in tree
elif [[ -f "$SCRIPT_DIR/remote_agent_adapter.py" ]]; then
    # Script was copied into a flat dir next to the files (legacy layout).
    REPO_ROOT="$SCRIPT_DIR"
else
    echo "ERROR: could not locate remote_agent_adapter.py." >&2
    echo "Run this script from a checkout of the nats-hub repo, e.g.:" >&2
    echo "    bash packaging/remote/install.sh" >&2
    echo "or copy packaging/remote/ + the files listed in FILES.txt next to this script." >&2
    exit 1
fi

py_version="$(python3 -c 'import sys;print(f"{sys.version_info.major}.{sys.version_info.minor}")')"
major="${py_version%%.*}"
minor="${py_version##*.}"
if [[ "$major" -lt 3 || ( "$major" -eq 3 && "$minor" -lt 10 ) ]]; then
    echo "ERROR: Python 3.10+ required (found $py_version)." >&2
    exit 1
fi

# ── Create target dir + venv ────────────────────────────────────────
echo "→ Target directory: $TARGET_DIR"
mkdir -p "$TARGET_DIR/worker_backends"

VENV_DIR="$TARGET_DIR/.venv"
if [[ ! -d "$VENV_DIR" ]]; then
    echo "→ Creating venv at $VENV_DIR"
    python3 -m venv "$VENV_DIR"
else
    echo "→ Reusing existing venv at $VENV_DIR"
fi

# ── Copy files (refresh each run → idempotent) ─────────────────────
echo "→ Copying thin file set from $REPO_ROOT"
copied=0
missing=0
for rel in "${REMOTE_FILES[@]}"; do
    src="$REPO_ROOT/$rel"
    dst="$TARGET_DIR/$rel"
    if [[ ! -f "$src" ]]; then
        echo "  WARN: $rel not found in source tree; skipping" >&2
        missing=$((missing + 1))
        continue
    fi
    mkdir -p "$(dirname "$dst")"
    cp "$src" "$dst"
    copied=$((copied + 1))
done
echo "  Copied $copied file(s)."

# ── Install pip requirements into the venv ──────────────────────────
REQ_FILE="$TARGET_DIR/requirements-remote.txt"
if [[ ! -f "$REQ_FILE" ]]; then
    echo "ERROR: $REQ_FILE missing after copy." >&2
    exit 1
fi

echo "→ Installing requirements into venv (this can take ~30s the first time)"
# shellcheck disable=SC1091
"$VENV_DIR/bin/python" -m pip install --upgrade pip >/dev/null
"$VENV_DIR/bin/python" -m pip install -r "$REQ_FILE"

# ── Report ──────────────────────────────────────────────────────────
cat <<EOF

─────────────────────────────────────────────────────────────────────
✓ Remote worker scaffold ready at: $TARGET_DIR
  Python:    $VENV_DIR/bin/python ($py_version)
  Adapter:   $TARGET_DIR/remote_agent_adapter.py
  Files:     $copied copied, $missing skipped
─────────────────────────────────────────────────────────────────────

NEXT STEP — launch the adapter. Fill in IDENTITY and your credential.

Activate the venv:

    source "$VENV_DIR/bin/activate"

Example A — token auth over wss:// (self-signed / private-CA cert):

    export NATS_TOKEN='<token-from-operator>'
    python3 remote_agent_adapter.py \\
        --identity <IDENTITY> \\
        --nats-url wss://hub.example.com:8080 \\
        --ca-file /path/to/hub-ca.crt \\
        --backend shell \\
        --execute "my-agent-cli --prompt"

Example B — credentials file (NKEY/JWT) over wss://:

    python3 remote_agent_adapter.py \\
        --identity <IDENTITY> \\
        --nats-url wss://hub.example.com:8080 \\
        --credentials-file ~/.nats/<IDENTITY>.creds \\
        --backend kilo \\
        --model anthropic/claude-sonnet-4.5

Read docs/REMOTE_INSTALL.md for the full worked examples (shell backend,
kilo backend, opencode backend) and the security checklist.

EOF
