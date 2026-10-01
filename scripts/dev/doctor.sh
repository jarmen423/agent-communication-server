#!/usr/bin/env bash
# Check the dev toolchain and print a fix for anything missing.
# Exit code: 0 if the required pieces are present, 1 otherwise.
set -uo pipefail
source "$(dirname "$0")/lib.sh"
cd "$REPO_ROOT"

fail=0
ok()   { printf '  \033[32m✔\033[0m %-16s %s\n' "$1" "$2"; }
warn() { printf '  \033[33m!\033[0m %-16s %s\n' "$1" "$2"; }
bad()  { printf '  \033[31m✘\033[0m %-16s %s\n' "$1" "$2"; fail=1; }

echo "nats-hub doctor"
echo "required:"
if command -v cargo >/dev/null; then
  ok cargo "$(cargo --version)"
else
  bad cargo "install Rust: https://rustup.rs"
fi

# C/C++ toolchain for the RocksDB build (surrealdb-librocksdb-sys).
if command -v c++ >/dev/null || command -v clang++ >/dev/null; then
  ok "c++ compiler" "$(command -v clang++ || command -v c++)"
else
  bad "c++ compiler" "Debian/Ubuntu: sudo apt install build-essential   macOS: xcode-select --install"
fi

# libclang (bindgen). Look for the shared lib in the usual places.
libclang=""
for f in /usr/lib/llvm-*/lib/libclang*.so* /usr/lib/*-linux-gnu/libclang*.so* /usr/lib64/libclang*.so* \
         /opt/homebrew/opt/llvm/lib/libclang.dylib /Library/Developer/CommandLineTools/usr/lib/libclang.dylib \
         "${LIBCLANG_PATH:-/nonexistent}"/libclang*; do
  [[ -e "$f" ]] && { libclang="$f"; break; }
done
if [[ -n "$libclang" ]]; then
  if [[ -n "${BINDGEN_EXTRA_CLANG_ARGS:-}" ]]; then
    ok libclang "$libclang (+ BINDGEN_EXTRA_CLANG_ARGS=$BINDGEN_EXTRA_CLANG_ARGS, auto-set)"
  else
    ok libclang "$libclang"
  fi
else
  bad libclang "Debian/Ubuntu: sudo apt install clang libclang-dev   Fedora: sudo dnf install clang-devel   macOS: xcode-select --install"
fi

if command -v nats-server >/dev/null; then
  ok nats-server "$(nats-server --version) ($(command -v nats-server))"
else
  bad nats-server "run: make setup   (installs a pinned copy into .tools/bin)"
fi

if command -v python3 >/dev/null; then
  ok python3 "$(python3 --version)"
else
  bad python3 "install Python >= 3.10"
fi

if [[ -x .venv/bin/python ]] && .venv/bin/python -c 'import nats, aiohttp, nkeys, pytest' 2>/dev/null; then
  ok ".venv" "core + dev deps installed"
else
  bad ".venv" "run: make setup"
fi

echo "optional:"
for cli in claude codex hermes cursor-agent kilo opencode grok; do
  if command -v "$cli" >/dev/null; then ok "$cli" "$(command -v "$cli")"
  else warn "$cli" "not installed (only needed for that worker type)"; fi
done

echo
echo "build env: CARGO_TARGET_DIR=$CARGO_TARGET_DIR"
if (( fail )); then echo "doctor: missing required tools (see ✘ above)"; exit 1; fi
echo "doctor: all required tools present"
