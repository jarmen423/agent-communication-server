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
#   ./install.sh [target-dir] [options]      # default target: ~/nats-hub-remote
#
#   --from-release TAG      Install the file set from the GitHub release TAG
#                           (or "latest"): downloads nats-hub-remote-*.tar.gz
#                           over HTTPS and verifies it against SHA256SUMS.
#   --release-base-url URL  Directory holding SHA256SUMS + the bundle
#                           (https:// or file://). Default: the GitHub release.
#   --repo URL              GitHub repo for release downloads
#                           (default: https://github.com/jarmen423/agent-communication-server)
#   --no-pip                Skip `pip install` (air-gapped: install nats-py yourself)
#
# Where the files come from:
#   * --from-release given: the verified release bundle. If the release is
#     unreachable, fall back to a local checkout when there is one.
#     A checksum mismatch is fatal.
#   * otherwise: the checkout (or flat bundle) this script lives in; if there
#     is none (script downloaded on its own), the latest release.
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
TARGET_DIR=""
RELEASE_TAG=""
RELEASE_BASE_URL="${NATS_HUB_RELEASE_BASE_URL:-}"
REPO_URL="${NATS_HUB_REPO:-https://github.com/jarmen423/agent-communication-server}"
NO_PIP=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help) awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "${BASH_SOURCE[0]}"; exit 0 ;;
        --from-release) RELEASE_TAG="${2:?--from-release requires a tag}"; shift 2 ;;
        --release-base-url) RELEASE_BASE_URL="${2:?--release-base-url requires a value}"; shift 2 ;;
        --repo) REPO_URL="${2:?--repo requires a value}"; shift 2 ;;
        --no-pip) NO_PIP=1; shift ;;
        -*) echo "ERROR: unknown option: $1" >&2; exit 2 ;;
        *)
            if [[ -z "$TARGET_DIR" ]]; then TARGET_DIR="$1"; shift
            else echo "ERROR: unexpected argument: $1" >&2; exit 2
            fi
            ;;
    esac
done
TARGET_DIR="${TARGET_DIR:-$HOME/nats-hub-remote}"
REPO_URL="${REPO_URL%.git}"
REPO_URL="${REPO_URL%/}"

# Source root = the directory holding remote_agent_adapter.py. In a checkout
# that is two levels up (packaging/remote/); in a release bundle or a flat
# copy it is this script's own directory.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCAL_ROOT=""
candidate="$SCRIPT_DIR"
for _ in 1 2 3 4 5; do
    if [[ -f "$candidate/remote_agent_adapter.py" ]]; then
        LOCAL_ROOT="$candidate"
        break
    fi
    candidate="$(dirname "$candidate")"
done

# ── Release download (HTTPS or file:// only, sha256-verified) ───────
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
    else echo "ERROR: need sha256sum or shasum to verify downloads." >&2; exit 1
    fi
}

fetch() {  # fetch <url> <dest>
    local url="$1" dest="$2"
    case "$url" in
        file://*) cp "${url#file://}" "$dest" 2>/dev/null ;;
        https://*)
            if command -v curl >/dev/null 2>&1; then
                curl -fsSL --proto '=https' --tlsv1.2 -o "$dest" "$url"
            elif command -v wget >/dev/null 2>&1; then
                wget -q --https-only -O "$dest" "$url"
            else
                echo "  need curl or wget to download releases" >&2; return 1
            fi
            ;;
        *) echo "ERROR: refusing non-TLS download URL: $url (use https:// or file://)" >&2; exit 1 ;;
    esac
}

# Sets RELEASE_ROOT and returns 0 on success; returns 1 if the release is
# unavailable. Exits on a checksum mismatch or a malformed bundle.
RELEASE_ROOT=""
fetch_release_bundle() {
    local tag="$1" base tmp line expected asset actual
    if [[ -n "$RELEASE_BASE_URL" ]]; then base="${RELEASE_BASE_URL%/}"
    elif [[ "$tag" == "latest" ]]; then base="$REPO_URL/releases/latest/download"
    else base="$REPO_URL/releases/download/$tag"
    fi
    tmp="$(mktemp -d)"
    # shellcheck disable=SC2064
    trap "rm -rf '$tmp'" EXIT
    echo "→ Release bundle from $base"
    fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || { echo "  could not download SHA256SUMS" >&2; return 1; }
    line="$(grep -E '^[0-9a-f]{64}[[:space:]]+\*?nats-hub-remote-[A-Za-z0-9._+-]+\.tar\.gz$' "$tmp/SHA256SUMS" | head -n1 || true)"
    [[ -n "$line" ]] || { echo "  release has no nats-hub-remote bundle" >&2; return 1; }
    expected="${line%%[[:space:]]*}"
    asset="${line##*[[:space:]]}"
    asset="${asset#\*}"
    fetch "$base/$asset" "$tmp/$asset" || { echo "  could not download $asset" >&2; return 1; }
    actual="$(sha256_of "$tmp/$asset")"
    if [[ "$actual" != "$expected" ]]; then
        echo "ERROR: checksum mismatch for $asset (expected $expected, got $actual). Refusing to install." >&2
        exit 1
    fi
    echo "  ✓ sha256 verified: $asset"
    tar -xzf "$tmp/$asset" -C "$tmp"
    RELEASE_ROOT="$tmp/${asset%.tar.gz}"
    if [[ ! -f "$RELEASE_ROOT/remote_agent_adapter.py" ]]; then
        echo "ERROR: $asset has no remote_agent_adapter.py" >&2
        exit 1
    fi
    return 0
}

REPO_ROOT=""
SOURCE_DESC=""
if [[ -n "$RELEASE_TAG" ]]; then
    if fetch_release_bundle "$RELEASE_TAG"; then
        REPO_ROOT="$RELEASE_ROOT"; SOURCE_DESC="release $RELEASE_TAG (sha256 verified)"
    elif [[ -n "$LOCAL_ROOT" ]]; then
        echo "→ Release unavailable; falling back to local checkout $LOCAL_ROOT"
        REPO_ROOT="$LOCAL_ROOT"; SOURCE_DESC="local checkout (release unavailable)"
    fi
elif [[ -n "$LOCAL_ROOT" ]]; then
    REPO_ROOT="$LOCAL_ROOT"; SOURCE_DESC="local checkout"
elif fetch_release_bundle latest; then
    REPO_ROOT="$RELEASE_ROOT"; SOURCE_DESC="release latest (sha256 verified)"
fi

# ── Preflight ───────────────────────────────────────────────────────
if ! command -v python3 >/dev/null 2>&1; then
    echo "ERROR: python3 not found on PATH. Install Python 3.10+ first." >&2
    exit 1
fi

if [[ -z "$REPO_ROOT" ]]; then
    echo "ERROR: could not locate remote_agent_adapter.py (no checkout, no release)." >&2
    echo "Run this script from a checkout of the nats-hub repo, e.g.:" >&2
    echo "    bash packaging/remote/install.sh" >&2
    echo "or pass --from-release <tag>, or copy packaging/remote/ + the files in FILES.txt next to it." >&2
    exit 1
fi

# ── Files to copy: FILES.txt (bundle root, or packaging/remote/ in a checkout)
REMOTE_FILES=()
for list in "$REPO_ROOT/FILES.txt" "$REPO_ROOT/packaging/remote/FILES.txt" "$SCRIPT_DIR/FILES.txt"; do
    if [[ -f "$list" ]]; then
        while IFS= read -r rel; do
            REMOTE_FILES+=("$rel")
        done < <(sed -e 's/#.*//' -e 's/[[:space:]]*$//' "$list" | grep -v '^$')
        break
    fi
done
if [[ ${#REMOTE_FILES[@]} -eq 0 ]]; then
    # Fallback when FILES.txt is absent (hand-copied legacy layout).
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

if (( NO_PIP )); then
    echo "→ --no-pip: skipping requirements (install later: $VENV_DIR/bin/python -m pip install -r $REQ_FILE)"
else
    echo "→ Installing requirements into venv (this can take ~30s the first time)"
    "$VENV_DIR/bin/python" -m pip install --upgrade pip >/dev/null
    "$VENV_DIR/bin/python" -m pip install -r "$REQ_FILE"
fi

# ── Report ──────────────────────────────────────────────────────────
cat <<EOF

─────────────────────────────────────────────────────────────────────
✓ Remote worker scaffold ready at: $TARGET_DIR
  Python:    $VENV_DIR/bin/python ($py_version)
  Adapter:   $TARGET_DIR/remote_agent_adapter.py
  Source:    $SOURCE_DESC
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
