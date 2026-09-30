#!/usr/bin/env bash
# Install a pinned nats-server into the repo-local tool dir (.tools/bin).
# No sudo, no global changes. Verifies the release SHA256 checksum.
#
# Usage: scripts/dev/install_nats_server.sh [--force]
# Env:   NATS_SERVER_VERSION (default below), TOOLS_DIR (default <repo>/.tools)
set -euo pipefail

VERSION="${NATS_SERVER_VERSION:-v2.15.0}"
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TOOLS_DIR="${TOOLS_DIR:-$REPO_ROOT/.tools}"
BIN="$TOOLS_DIR/bin/nats-server"

if [[ -x "$BIN" && "${1:-}" != "--force" ]]; then
  echo "[nats-server] already installed: $("$BIN" --version)"
  exit 0
fi

case "$(uname -s)" in
  Linux)  os=linux ;;
  Darwin) os=darwin ;;
  *) echo "unsupported OS $(uname -s); install nats-server manually: https://docs.nats.io/running-a-nats-service/introduction/installation" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64|amd64)  arch=amd64 ;;
  aarch64|arm64) arch=arm64 ;;
  *) echo "unsupported arch $(uname -m)" >&2; exit 1 ;;
esac

name="nats-server-${VERSION}-${os}-${arch}"
base="https://github.com/nats-io/nats-server/releases/download/${VERSION}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "[nats-server] downloading ${name}.tar.gz"
curl -fsSL -o "$tmp/${name}.tar.gz" "$base/${name}.tar.gz"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"

expected="$(grep " ${name}.tar.gz\$" "$tmp/SHA256SUMS" | awk '{print $1}')"
if command -v sha256sum >/dev/null; then
  actual="$(sha256sum "$tmp/${name}.tar.gz" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$tmp/${name}.tar.gz" | awk '{print $1}')"
fi
if [[ -z "$expected" || "$expected" != "$actual" ]]; then
  echo "[nats-server] checksum mismatch (expected '$expected', got '$actual')" >&2
  exit 1
fi

tar -xzf "$tmp/${name}.tar.gz" -C "$tmp"
mkdir -p "$TOOLS_DIR/bin"
install -m 0755 "$tmp/${name}/nats-server" "$BIN"
echo "[nats-server] installed $("$BIN" --version) → $BIN"
