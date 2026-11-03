#!/bin/sh
# install_remote.sh — install the nats-hub client CLIs (`acs`, `hub-delegate`
# and every other hub-* CLI) and link them into ~/.local/bin.
#
# Hosted ACS (https://acs.agentmemorylabs.com), no arguments:
#   curl -fsSL https://raw.githubusercontent.com/jarmen423/agent-communication-server/main/scripts/install_remote.sh | sh
#   acs login
# Self-hosted hub: pass its URL to also get a `hub-delegate-remote` wrapper:
#   curl -fsSL …/install_remote.sh | sh -s -- wss://hub.example.com:8443
#   bash scripts/install_remote.sh <HUB>             # from a clone
#
# <HUB> is a host (`hub.example.com`, `10.0.0.5:4222`) or a full NATS URL
# (`wss://hub.example.com:8443`, `tls://hub:4222`, `nats://hub:4222`).
#
# How the binaries are obtained (first that applies):
#   1. --bin PATH                  copy an existing hub-delegate
#   2. --from-source               cargo build from this checkout (or a clone)
#   3. default / --from-release T  download the GitHub release asset for this
#                                  platform over HTTPS, verify it against the
#                                  release's SHA256SUMS, install acs and every hub-* CLI.
#                                  If the download fails or there is no asset
#                                  for this platform, fall back to (2) unless
#                                  --release-only. A checksum MISMATCH is fatal
#                                  and never falls back.
#
# Binaries go to ~/.local/share/nats-hub/bin and are symlinked into
# ~/.local/bin (created if missing; a PATH line is printed if it isn't on
# PATH). Re-running is safe: it refreshes the binaries and its own links and
# leaves any other file in ~/.local/bin alone.
#
# The machine needs no nats-server, hub-server or SurrealDB. Auth comes at
# runtime from `acs` (hosted) or NATS_* env vars (self-host: NATS_TOKEN,
# NATS_USER/NATS_PASSWORD, NATS_CREDENTIALS_FILE, NATS_NKEY, NATS_REQUIRE_TLS);
# this script never bakes secrets.
#
# `curl … | sh` runs this file with dash on Debian/Ubuntu, but the installer
# needs bash. Only the lines up to the heredoc are POSIX sh: sh reads the
# whole quoted heredoc before running anything, then hands it to bash.

if ! command -v bash >/dev/null 2>&1; then
  echo "ERROR: install_remote.sh needs bash. Install bash, then re-run." >&2
  exit 1
fi
NATS_HUB_INSTALL_SCRIPT="$0"
export NATS_HUB_INSTALL_SCRIPT
exec bash -s -- "$@" <<'NATS_HUB_INSTALL_BODY'
set -euo pipefail

HOSTED_URL="https://acs.agentmemorylabs.com"
INSTALL_URL="https://raw.githubusercontent.com/jarmen423/agent-communication-server/main/scripts/install_remote.sh"

usage() {
  cat <<'EOF'
Usage: install_remote.sh [HUB] [options]

  No HUB: install acs + every hub-* CLI for hosted ACS, then run `acs login`.

  HUB   a self-hosted hub: host[:port], or a full URL: wss://host:port,
        tls://host:port, nats://host:port, ws://host:port. A bare host becomes
        nats://HOST:PORT (plaintext TCP; see "Transport" below). Also writes
        ~/bin/hub-delegate-remote with that URL baked in.

Options:
  --port PORT               NATS port for a bare HUB host (default: 4222)
  --nats-url URL            Bake exactly this URL into the wrapper (overrides HUB)
  --from-release TAG        Release tag to install (default: latest)
  --release-only            Fail instead of building from source if the release
                            download is unavailable
  --from-source             Skip releases; cargo build from source
  --with-acs                Also build/install acs when building from source
                            (always on without HUB)
  --release-base-url URL    Directory holding SHA256SUMS + tarballs (https:// or
                            file://). Default: the GitHub release for TAG.
  --repo URL                GitHub repo (release downloads + source clone)
                            (default: https://github.com/jarmen423/agent-communication-server)
  --bin PATH                Use an existing hub-delegate binary
  -h, --help                Show this help

Binaries: ~/.local/share/nats-hub/bin, symlinked into ~/.local/bin.

Env overrides: HUB_DELEGATE_BIN (= --bin), NATS_HUB_REPO (= --repo),
NATS_HUB_RELEASE_BASE_URL (= --release-base-url), CARGO_TARGET_DIR.

Transport: a bare host defaults to nats:// (plaintext). Use it only on a
trusted LAN/VPN or loopback; across the internet pass wss:// or tls:// (see
docs/SECURITY.md and docs/JOIN_HUB.md). Release downloads are always HTTPS.
EOF
}

HUB_ARG=""
NATS_PORT="4222"
PORT_SET=0
NATS_URL_OVERRIDE=""
REPO_URL="${NATS_HUB_REPO:-https://github.com/jarmen423/agent-communication-server}"
EXISTING_BIN="${HUB_DELEGATE_BIN:-}"
RELEASE_TAG="latest"
RELEASE_BASE_URL="${NATS_HUB_RELEASE_BASE_URL:-}"
MODE="release"          # release | source
RELEASE_ONLY=0
WITH_ACS=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --port) NATS_PORT="${2:?--port requires a value}"; PORT_SET=1; shift 2 ;;
    --nats-url) NATS_URL_OVERRIDE="${2:?--nats-url requires a value}"; shift 2 ;;
    --from-release) RELEASE_TAG="${2:?--from-release requires a tag}"; MODE="release"; shift 2 ;;
    --release-only) RELEASE_ONLY=1; shift ;;
    --from-source) MODE="source"; shift ;;
    --with-acs) WITH_ACS=1; shift ;;
    --release-base-url) RELEASE_BASE_URL="${2:?--release-base-url requires a value}"; shift 2 ;;
    --repo) REPO_URL="${2:?--repo requires a value}"; shift 2 ;;
    --bin) EXISTING_BIN="${2:?--bin requires a value}"; shift 2 ;;
    -*) echo "ERROR: unknown option: $1" >&2; usage >&2; exit 2 ;;
    *)
      if [[ -z "$HUB_ARG" ]]; then HUB_ARG="$1"; shift
      else echo "ERROR: unexpected argument: $1" >&2; usage >&2; exit 2
      fi
      ;;
  esac
done

# Self-host when a hub was named; hosted ACS (acs login) otherwise.
SELF_HOST=0
[[ -n "$HUB_ARG" || -n "$NATS_URL_OVERRIDE" ]] && SELF_HOST=1
(( SELF_HOST )) || WITH_ACS=1   # hosted needs acs, even from source

# ── Resolve the hub URL baked into the wrapper (self-host) ──────────
NATS_URL=""
if [[ -n "$NATS_URL_OVERRIDE" ]]; then
  NATS_URL="$NATS_URL_OVERRIDE"
elif [[ "$HUB_ARG" == *://* ]]; then
  NATS_URL="${HUB_ARG%/}"          # full URL: keep scheme + port as given
  if (( PORT_SET )); then
    echo "ERROR: --port conflicts with a full URL ($HUB_ARG); put the port in the URL." >&2
    exit 2
  fi
elif [[ -n "$HUB_ARG" ]]; then
  host="${HUB_ARG%%/*}"
  if [[ "$host" == *:* && "$host" != \[*\] && "${host##*:}" =~ ^[0-9]+$ ]]; then
    (( PORT_SET )) || NATS_PORT="${host##*:}"
    host="${host%:*}"
  fi
  NATS_URL="nats://${host}:${NATS_PORT}"
fi
if (( SELF_HOST )); then
  case "$NATS_URL" in
    nats://*|tls://*|ws://*|wss://*) ;;
    *) echo "ERROR: unsupported hub URL scheme: $NATS_URL (use nats://, tls://, ws:// or wss://)" >&2; exit 2 ;;
  esac
fi

INSTALL_ROOT="${HOME}/.local/share/nats-hub"
BIN_DIR="${INSTALL_ROOT}/bin"
LINK_DIR="${HOME}/.local/bin"
WRAPPER_DIR="${HOME}/bin"
BINARY_PATH="${BIN_DIR}/hub-delegate"
WRAPPER_PATH="${WRAPPER_DIR}/hub-delegate-remote"
REPO_URL="${REPO_URL%.git}"
REPO_URL="${REPO_URL%/}"
mkdir -p "$BIN_DIR"

if (( SELF_HOST )); then
  echo "→ Hub target:  ${NATS_URL}"
else
  echo "→ Hosted ACS:  ${HOSTED_URL} (self-host: pass your hub URL, see --help)"
fi
echo "→ Install dir: ${INSTALL_ROOT}"

# ── Helpers ─────────────────────────────────────────────────────────
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
  else echo "ERROR: need sha256sum or shasum to verify downloads." >&2; exit 1
  fi
}

# fetch <url> <dest>: https:// (TLS only, also for redirects) or file://.
fetch() {
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

# Rust target triple of the release asset for this machine ("" = none published).
release_target() {
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64|Linux-amd64) echo "x86_64-unknown-linux-gnu" ;;
    Linux-aarch64|Linux-arm64) echo "aarch64-unknown-linux-gnu" ;;
    Darwin-arm64|Darwin-aarch64) echo "aarch64-apple-darwin" ;;
    *) echo "" ;;
  esac
}

# Returns 0 on success, 1 if the release is unavailable (caller may fall back).
# Exits non-zero on a checksum mismatch or a malformed archive.
install_from_release() {
  local target base tmp line asset expected actual dir f n=0
  target="$(release_target)"
  if [[ -z "$target" ]]; then
    echo "  no release binaries are published for $(uname -s)/$(uname -m)" >&2
    return 1
  fi
  if [[ -n "$RELEASE_BASE_URL" ]]; then
    base="${RELEASE_BASE_URL%/}"
  elif [[ "$RELEASE_TAG" == "latest" ]]; then
    base="${REPO_URL}/releases/latest/download"
  else
    base="${REPO_URL}/releases/download/${RELEASE_TAG}"
  fi
  tmp="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'" EXIT

  echo "→ Release: ${base} (target ${target})"
  if ! fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS"; then
    echo "  could not download SHA256SUMS from $base" >&2
    return 1
  fi
  line="$(grep -E "^[0-9a-f]{64}[[:space:]]+\*?nats-hub-[A-Za-z0-9._+-]+-${target}\.tar\.gz\$" "$tmp/SHA256SUMS" | head -n1 || true)"
  if [[ -z "$line" ]]; then
    echo "  release has no asset for ${target}" >&2
    return 1
  fi
  expected="${line%%[[:space:]]*}"
  asset="${line##*[[:space:]]}"
  asset="${asset#\*}"
  if ! fetch "$base/$asset" "$tmp/$asset"; then
    echo "  could not download $asset" >&2
    return 1
  fi
  actual="$(sha256_of "$tmp/$asset")"
  if [[ "$actual" != "$expected" ]]; then
    echo "ERROR: checksum mismatch for $asset" >&2
    echo "  expected $expected (SHA256SUMS)" >&2
    echo "  got      $actual" >&2
    echo "Refusing to install. Not falling back to a source build." >&2
    exit 1
  fi
  echo "  ✓ sha256 verified: $asset"

  mkdir -p "$tmp/x"
  tar -xzf "$tmp/$asset" -C "$tmp/x"
  dir="$tmp/x/${asset%.tar.gz}"
  if [[ ! -x "$dir/hub-delegate" ]]; then
    echo "ERROR: $asset does not contain an executable hub-delegate" >&2
    exit 1
  fi
  for f in "$dir"/acs "$dir"/hub-*; do
    [[ -f "$f" && -x "$f" ]] || continue
    cp -f "$f" "$BIN_DIR/"
    chmod +x "$BIN_DIR/$(basename "$f")"
    n=$((n + 1))
  done
  echo "  installed $n CLI(s) into $BIN_DIR"
  return 0
}

install_from_source() {
  local script_dir="" repo_root="" candidate src_dir i
  if [[ -f "${NATS_HUB_INSTALL_SCRIPT:-}" ]]; then
    script_dir="$(cd "$(dirname "$NATS_HUB_INSTALL_SCRIPT")" && pwd)"
  fi
  candidate="$script_dir"
  for i in 1 2 3 4 5; do
    [[ -n "$candidate" ]] || break
    if [[ -f "$candidate/Cargo.toml" && -f "$candidate/crates/client/src/bin/hub_delegate.rs" ]]; then
      repo_root="$candidate"
      break
    fi
    candidate="$(dirname "$candidate")"
  done

  if ! command -v cargo >/dev/null 2>&1; then
    cat >&2 <<'EOF'
ERROR: cargo not found on PATH, and no release binary was installed.

Install Rust (https://rustup.rs), or re-run with a prebuilt binary:
  bash scripts/install_remote.sh <HUB> --bin /path/to/hub-delegate
EOF
    exit 1
  fi

  if [[ -z "$repo_root" ]]; then
    src_dir="${INSTALL_ROOT}/src"
    echo "→ No local checkout found; cloning ${REPO_URL} → ${src_dir}"
    if [[ -d "$src_dir/.git" ]]; then
      git -C "$src_dir" pull --ff-only || true
    else
      rm -rf "$src_dir"
      git clone --depth 1 "${REPO_URL}.git" "$src_dir"
    fi
    repo_root="$src_dir"
  else
    echo "→ Building from checkout: $repo_root"
  fi

  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${INSTALL_ROOT}/target}"
  local build_args=(build --release -p nats-hub-client --bin hub-delegate)
  (( WITH_ACS )) && build_args+=(-p acs --bin acs)
  echo "→ cargo ${build_args[*]} (CARGO_TARGET_DIR=$CARGO_TARGET_DIR)"
  (cd "$repo_root" && source scripts/dev/lib.sh && cargo "${build_args[@]}")

  local built="${CARGO_TARGET_DIR}/release/hub-delegate"
  if [[ ! -f "$built" ]]; then
    echo "ERROR: build finished but binary missing: $built" >&2
    exit 1
  fi
  cp -f "$built" "$BINARY_PATH"
  chmod +x "$BINARY_PATH"
  if (( WITH_ACS )); then
    cp -f "${CARGO_TARGET_DIR}/release/acs" "$BIN_DIR/acs"
    chmod +x "$BIN_DIR/acs"
  fi
}

# link_one <target> <name>: symlink LINK_DIR/<name> → <target>. Our own (or
# dangling) links are refreshed; anything else is left alone with a note.
LINKED=0
LEFT_ALONE=""
link_one() {
  local target="$1" name="$2" link="$LINK_DIR/$2" current
  if [[ -L "$link" ]]; then
    current="$(readlink "$link")"
    [[ "$current" == "$target" ]] && { LINKED=$((LINKED + 1)); return 0; }
    if [[ -e "$link" && "$current" != "$BIN_DIR"/* && "$current" != "$WRAPPER_PATH" ]]; then
      LEFT_ALONE="$LEFT_ALONE $name"
      echo "  ! left alone: $link → $current (not from this installer)" >&2
      return 0
    fi
    rm -f "$link"
  elif [[ -e "$link" ]]; then
    LEFT_ALONE="$LEFT_ALONE $name"
    echo "  ! left alone: $link (an existing file, not from this installer)" >&2
    return 0
  fi
  ln -s "$target" "$link"
  LINKED=$((LINKED + 1))
}

link_bins() {
  local f
  mkdir -p "$LINK_DIR"
  for f in "$BIN_DIR"/acs "$BIN_DIR"/hub-*; do
    [[ -f "$f" && -x "$f" ]] || continue
    link_one "$f" "$(basename "$f")"
  done
  (( SELF_HOST )) && link_one "$WRAPPER_PATH" "hub-delegate-remote"
  return 0
}

# ── Obtain the binaries ─────────────────────────────────────────────
if [[ -n "$EXISTING_BIN" ]]; then
  if [[ ! -f "$EXISTING_BIN" ]]; then
    echo "ERROR: --bin path not found: $EXISTING_BIN" >&2
    exit 1
  fi
  echo "→ Using existing binary: $EXISTING_BIN"
  cp -f "$EXISTING_BIN" "$BINARY_PATH"
  chmod +x "$BINARY_PATH"
  SOURCE_DESC="--bin $EXISTING_BIN"
elif [[ "$MODE" == "source" ]]; then
  install_from_source
  SOURCE_DESC="source build"
elif install_from_release; then
  SOURCE_DESC="release ${RELEASE_TAG} (sha256 verified)"
elif (( RELEASE_ONLY )); then
  echo "ERROR: release install failed and --release-only was given." >&2
  exit 1
else
  echo "→ Falling back to building from source"
  install_from_source
  SOURCE_DESC="source build (release unavailable)"
fi

# ── Self-host: wrapper with the hub URL baked in ────────────────────
# Pass-through of all CLI args; only injects --nats-url if the caller
# did not already supply one.
if (( SELF_HOST )); then
  mkdir -p "$WRAPPER_DIR"
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
fi

link_bins

# ── Summary ─────────────────────────────────────────────────────────
echo
echo "✓ nats-hub client installed"
echo "  from:     ${SOURCE_DESC}"
echo "  binaries: ${BIN_DIR}"
echo "  linked:   ${LINKED} into ${LINK_DIR}"
if (( SELF_HOST )); then
  echo "  wrapper:  ${WRAPPER_PATH}"
  echo "  hub:      ${NATS_URL}"
elif [[ ! -x "$BIN_DIR/acs" ]]; then
  echo "  ! acs is not installed (--bin installs hub-delegate only); re-run without --bin"
fi

ON_PATH=0
case ":${PATH}:" in
  *":${LINK_DIR}:"*|*":${LINK_DIR}/:"*) ON_PATH=1 ;;
esac
if (( ON_PATH )); then
  # A different acs/hub-delegate earlier on PATH would shadow this install.
  for name in acs hub-delegate; do
    [[ -L "$LINK_DIR/$name" ]] || continue
    found="$(command -v "$name" 2>/dev/null || true)"
    if [[ -n "$found" && "$found" != "$LINK_DIR/$name" ]]; then
      echo "  ! $found comes before ${LINK_DIR} on PATH and hides this $name"
    fi
  done
else
  case "${SHELL:-}" in
    */zsh) rc="~/.zshrc"; path_line='export PATH="$HOME/.local/bin:$PATH"' ;;
    */fish) rc="~/.config/fish/config.fish"; path_line='fish_add_path $HOME/.local/bin' ;;
    */bash) rc="~/.bashrc"; [[ "$(uname -s)" == Darwin ]] && rc="~/.bash_profile"
            path_line='export PATH="$HOME/.local/bin:$PATH"' ;;
    *) rc="~/.profile"; path_line='export PATH="$HOME/.local/bin:$PATH"' ;;
  esac
  echo
  echo "  ! ${LINK_DIR} is not on your PATH. Run this now and add it to ${rc}:"
  echo "      ${path_line}"
fi

if (( SELF_HOST )); then
  hub_host="${NATS_URL#*://}"; hub_host="${hub_host%%[:/]*}"
  case "$NATS_URL" in
    nats://*|ws://*)
      case "$hub_host" in
        127.*|localhost|::1|\[::1\]) ;;
        *)
          echo
          echo "  ⚠ ${NATS_URL%%://*}:// is plaintext: tokens and messages cross the network unencrypted."
          echo "    Fine on a trusted LAN/VPN. Otherwise re-run with wss://… or tls://… (docs/SECURITY.md)."
          ;;
      esac
      ;;
  esac
  echo
  echo "Next, on your hub (${NATS_URL}):"
  echo "  export NATS_TOKEN='…'   # or NATS_USER + NATS_PASSWORD, or NATS_CREDENTIALS_FILE"
  echo "  hub-delegate-remote --to <worker-identity> --from <you> --prompt \"…\" --verbose"
  echo "  (or export NATS_URL=${NATS_URL} NATS_HUB_IDENTITY=<you> and use any hub-* CLI)"
  echo "Hosted ACS instead: run \`acs login\` (no hub URL needed)."
else
  echo
  echo "Next, sign in to hosted ACS (${HOSTED_URL}):"
  echo "  acs login                     # approve this device in your browser"
  echo "  acs agent add <id>            # an identity for this machine (--role worker for a worker)"
  echo "  acs up                        # keeps its credentials fresh"
  echo "  acs exec <id> -- hub-agents   # any hub-* CLI, as <id>, on your hub"
  echo "Then connect an agent: https://github.com/jarmen423/agent-communication-server#hosted-quick-start"
  echo "Self-hosting a hub? Re-run with its URL: curl -fsSL ${INSTALL_URL} | sh -s -- wss://hub.example.com:8443"
fi
NATS_HUB_INSTALL_BODY
