#!/usr/bin/env bash
# install_remote.sh — install hub-delegate on a remote client machine.
#
# One-liner (from a clone of this repo):
#   bash scripts/install_remote.sh <HUB_HOST>
#
# Or:
#   curl -fsSL https://raw.githubusercontent.com/<org>/nats-hub/main/scripts/install_remote.sh \
#     | bash -s -- <HUB_HOST>
#
# What it does:
#   1. Builds hub-delegate (cargo) or uses HUB_DELEGATE_BIN / a release binary
#   2. Installs the binary under ~/.local/share/nats-hub/bin/
#   3. Writes ~/bin/hub-delegate-remote with --nats-url nats://HUB_HOST:4222 baked in
#
# The remote machine does NOT need nats-server, hub-server, or SurrealDB —
# only the hub-delegate client. Auth (if the hub requires it) uses NATS_*
# env vars at runtime (NATS_TOKEN, NATS_USER/NATS_PASSWORD, …) — this script
# never bakes secrets.

set -euo pipefail

usage() {
  cat <<'EOF'
Usage: install_remote.sh <HUB_HOST> [options]

  HUB_HOST   IP or hostname of the machine running nats-server + hub-server

Options:
  --port PORT          NATS TCP port (default: 4222)
  --repo URL           git clone URL if this machine has no checkout
                       (default: https://github.com/jarmen423/agent-communication-server.git)
  --bin PATH           Use an existing hub-delegate binary instead of building
  --from-release TAG   Download hub-delegate from a GitHub release tag (if published)
  -h, --help           Show this help

Env overrides:
  HUB_DELEGATE_BIN     Same as --bin
  NATS_HUB_REPO        Same as --repo
  CARGO_TARGET_DIR     Where cargo writes build artifacts
EOF
}

HUB_HOST=""
NATS_PORT="4222"
REPO_URL="${NATS_HUB_REPO:-https://github.com/jarmen423/agent-communication-server.git}"
EXISTING_BIN="${HUB_DELEGATE_BIN:-}"
RELEASE_TAG=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --port)
      NATS_PORT="${2:?--port requires a value}"
      shift 2
      ;;
    --repo)
      REPO_URL="${2:?--repo requires a value}"
      shift 2
      ;;
    --bin)
      EXISTING_BIN="${2:?--bin requires a value}"
      shift 2
      ;;
    --from-release)
      RELEASE_TAG="${2:?--from-release requires a tag}"
      shift 2
      ;;
    -*)
      echo "ERROR: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
    *)
      if [[ -z "$HUB_HOST" ]]; then
        HUB_HOST="$1"
        shift
      else
        echo "ERROR: unexpected argument: $1" >&2
        usage >&2
        exit 2
      fi
      ;;
  esac
done

if [[ -z "$HUB_HOST" ]]; then
  echo "ERROR: HUB_HOST is required." >&2
  usage >&2
  exit 2
fi

# Strip scheme / trailing path if someone pastes a full URL by mistake.
HUB_HOST="${HUB_HOST#nats://}"
HUB_HOST="${HUB_HOST#ws://}"
HUB_HOST="${HUB_HOST#wss://}"
HUB_HOST="${HUB_HOST%%/*}"
if [[ "$HUB_HOST" == *:* && "$HUB_HOST" != \[*\] ]]; then
  # host:port form — keep host, prefer explicit --port if both set
  maybe_port="${HUB_HOST##*:}"
  maybe_host="${HUB_HOST%:*}"
  if [[ "$maybe_port" =~ ^[0-9]+$ ]]; then
    HUB_HOST="$maybe_host"
    if [[ "$NATS_PORT" == "4222" ]]; then
      NATS_PORT="$maybe_port"
    fi
  fi
fi

NATS_URL="nats://${HUB_HOST}:${NATS_PORT}"
INSTALL_ROOT="${HOME}/.local/share/nats-hub"
BIN_DIR="${INSTALL_ROOT}/bin"
WRAPPER_DIR="${HOME}/bin"
BINARY_PATH="${BIN_DIR}/hub-delegate"
WRAPPER_PATH="${WRAPPER_DIR}/hub-delegate-remote"

mkdir -p "$BIN_DIR" "$WRAPPER_DIR"

echo "→ Hub target:  ${NATS_URL}"
echo "→ Install dir: ${INSTALL_ROOT}"

# ── Obtain hub-delegate binary ──────────────────────────────────────
obtain_binary() {
  if [[ -n "$EXISTING_BIN" ]]; then
    if [[ ! -x "$EXISTING_BIN" && ! -f "$EXISTING_BIN" ]]; then
      echo "ERROR: --bin path not found: $EXISTING_BIN" >&2
      exit 1
    fi
    echo "→ Using existing binary: $EXISTING_BIN"
    cp -f "$EXISTING_BIN" "$BINARY_PATH"
    chmod +x "$BINARY_PATH"
    return
  fi

  if [[ -n "$RELEASE_TAG" ]]; then
    # Best-effort release download. Asset naming is conventional; operators
    # can always fall back to --bin or a local cargo build.
    local os arch asset url tmp
    os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m)"
    case "$arch" in
      x86_64|amd64) arch="x86_64" ;;
      aarch64|arm64) arch="aarch64" ;;
    esac
    asset="hub-delegate-${os}-${arch}"
    # Derive owner/repo from REPO_URL when it looks like GitHub.
    local gh_path
    gh_path="$(echo "$REPO_URL" | sed -E 's#.*github\.com[:/]([^/]+/[^/.]+)(\.git)?#\1#')"
    url="https://github.com/${gh_path}/releases/download/${RELEASE_TAG}/${asset}"
    tmp="$(mktemp)"
    echo "→ Downloading release binary: $url"
    if command -v curl >/dev/null 2>&1; then
      curl -fsSL "$url" -o "$tmp"
    elif command -v wget >/dev/null 2>&1; then
      wget -qO "$tmp" "$url"
    else
      echo "ERROR: need curl or wget to download a release binary." >&2
      exit 1
    fi
    mv -f "$tmp" "$BINARY_PATH"
    chmod +x "$BINARY_PATH"
    return
  fi

  # Build from source: prefer an existing checkout that contains this script,
  # otherwise clone into a cache dir under INSTALL_ROOT.
  local script_dir repo_root src_dir
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  repo_root=""
  local candidate="$script_dir"
  local i
  for i in 1 2 3 4 5; do
    if [[ -f "$candidate/Cargo.toml" && -f "$candidate/src/bin/hub_delegate.rs" ]]; then
      repo_root="$candidate"
      break
    fi
    candidate="$(dirname "$candidate")"
  done

  if [[ -z "$repo_root" ]]; then
    src_dir="${INSTALL_ROOT}/src"
    echo "→ No local checkout found; cloning ${REPO_URL} → ${src_dir}"
    if [[ -d "$src_dir/.git" ]]; then
      git -C "$src_dir" pull --ff-only || true
    else
      rm -rf "$src_dir"
      git clone --depth 1 "$REPO_URL" "$src_dir"
    fi
    repo_root="$src_dir"
  else
    echo "→ Building from checkout: $repo_root"
  fi

  if ! command -v cargo >/dev/null 2>&1; then
    cat >&2 <<'EOF'
ERROR: cargo not found on PATH.

Install Rust (https://rustup.rs), or re-run with a prebuilt binary:
  bash scripts/install_remote.sh <HUB_HOST> --bin /path/to/hub-delegate
EOF
    exit 1
  fi

  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${INSTALL_ROOT}/target}"
  echo "→ cargo build --release --bin hub-delegate (CARGO_TARGET_DIR=$CARGO_TARGET_DIR)"
  (cd "$repo_root" && cargo build --release --bin hub-delegate)

  local built="${CARGO_TARGET_DIR}/release/hub-delegate"
  if [[ ! -f "$built" ]]; then
    echo "ERROR: build finished but binary missing: $built" >&2
    exit 1
  fi
  cp -f "$built" "$BINARY_PATH"
  chmod +x "$BINARY_PATH"
}

obtain_binary

# ── Wrapper with baked-in NATS URL ──────────────────────────────────
# Pass-through of all CLI args; only injects --nats-url if the caller
# did not already supply one.
cat > "$WRAPPER_PATH" <<EOF
#!/usr/bin/env bash
# Auto-generated by nats-hub scripts/install_remote.sh — do not edit by hand.
# Re-run the installer to point at a different hub.
set -euo pipefail
BINARY="${BINARY_PATH}"
DEFAULT_NATS_URL="${NATS_URL}"
has_nats_url=0
for arg in "\$@"; do
  case "\$arg" in
    --nats-url|--nats-url=*) has_nats_url=1 ;;
  esac
done
if [[ "\$has_nats_url" -eq 0 ]]; then
  exec "\$BINARY" --nats-url "\$DEFAULT_NATS_URL" "\$@"
else
  exec "\$BINARY" "\$@"
fi
EOF
chmod +x "$WRAPPER_PATH"

# ── PATH hint ───────────────────────────────────────────────────────
path_note=""
case ":${PATH}:" in
  *":${WRAPPER_DIR}:"*) path_note="(${WRAPPER_DIR} already on PATH)" ;;
  *)
    path_note="(add to shell rc: export PATH=\"\$HOME/bin:\$PATH\")"
    # Soft-export for the current process only so the printed examples work
    # if the user sources this script; the installed wrapper path is absolute
    # either way.
    export PATH="${WRAPPER_DIR}:${PATH}"
    ;;
esac

echo
echo "✓ nats-hub remote client installed"
echo "  binary:  ${BINARY_PATH}"
echo "  wrapper: ${WRAPPER_PATH}  ${path_note}"
echo "  hub:     ${NATS_URL}"
echo
echo "Usage:"
echo "  hub-delegate-remote --to <worker-identity> --from <you> --prompt \"...\" --verbose"
echo
echo "Examples:"
echo "  hub-delegate-remote --to hermes-worker-1 --from josh --prompt \"What is 2+2?\" --verbose"
echo "  hub-delegate-remote --to hermes-worker-1 --from josh --prompt \"ping\" --timeout 60"
echo
echo "If the hub requires auth, export before calling (never baked into the wrapper):"
echo "  export NATS_TOKEN='…'          # or NATS_USER / NATS_PASSWORD"
echo
echo "Override hub for one call:"
echo "  hub-delegate-remote --nats-url nats://other-host:4222 --to worker-1 --prompt \"hi\""
