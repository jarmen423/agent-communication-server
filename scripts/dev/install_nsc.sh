#!/usr/bin/env bash
# Install a pinned nsc into the repo-local tool dir (.tools/bin).
# No sudo, no global changes. Verifies the release SHA256 checksum.
#
# Usage: scripts/dev/install_nsc.sh [--force]
# Env:   NSC_VERSION (default below), TOOLS_DIR (default <repo>/.tools)
set -euo pipefail

VERSION="${NSC_VERSION:-v2.15.0}"
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TOOLS_DIR="${TOOLS_DIR:-$REPO_ROOT/.tools}"
BIN="$TOOLS_DIR/bin/nsc"

if [[ -x "$BIN" && "${1:-}" != "--force" && "$("$BIN" --version)" == "nsc version ${VERSION#v}" ]]; then
  echo "[nsc] already installed: $("$BIN" --version)"
  exit 0
fi

case "$(uname -s)" in
  Linux)  os=linux ;;
  Darwin) os=darwin ;;
  *) echo "unsupported OS $(uname -s); install nsc manually: https://docs.nats.io/running-a-nats-service/introduction/installation" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64|amd64)  arch=amd64 ;;
  aarch64|arm64) arch=arm64 ;;
  *) echo "unsupported arch $(uname -m)" >&2; exit 1 ;;
esac

name="nsc-${os}-${arch}"
base="https://github.com/nats-io/nsc/releases/download/${VERSION}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "[nsc] downloading ${name}.zip"
curl -fsSL -o "$tmp/${name}.zip" "$base/${name}.zip"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS-nsc.txt"

expected="$(grep " ${name}.zip\$" "$tmp/SHA256SUMS" | awk '{print $1}')"
if command -v sha256sum >/dev/null; then
  actual="$(sha256sum "$tmp/${name}.zip" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$tmp/${name}.zip" | awk '{print $1}')"
fi
if [[ -z "$expected" || "$expected" != "$actual" ]]; then
  echo "[nsc] checksum mismatch (expected '$expected', got '$actual')" >&2
  exit 1
fi

python3 -m zipfile -e "$tmp/${name}.zip" "$tmp"
mkdir -p "$TOOLS_DIR/bin"
install -m 0755 "$tmp/nsc" "$BIN"
echo "[nsc] installed $("$BIN" --version) → $BIN"
