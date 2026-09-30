# Shared helpers for scripts/dev/*. Source, don't execute.
# shellcheck shell=bash

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TOOLS_DIR="${TOOLS_DIR:-$REPO_ROOT/.tools}"
export PATH="$TOOLS_DIR/bin:$PATH"

# Build env: keep builds working on machines where libclang lacks builtin headers.
if [[ -z "${BINDGEN_EXTRA_CLANG_ARGS:-}" ]]; then
  _bg="$("$REPO_ROOT/scripts/dev/bindgen_args.sh" || true)"
  [[ -n "$_bg" ]] && export BINDGEN_EXTRA_CLANG_ARGS="$_bg"
fi
# One target dir per checkout/worktree (default ./target). Never share a target
# dir between worktrees — the crate's own artifacts collide (see CONTRIBUTING).
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
export CARGO_TARGET_DIR

# Shared compile cache across ALL checkouts/worktrees: content-addressed, so
# dependency builds (SurrealDB, RocksDB C++, tokio, …) are reused while each
# worktree still gets its own target dir. Prefer kache (Rust + C/C++ shims),
# else sccache. Opt out with NATS_HUB_NO_BUILD_CACHE=1.
if [[ -z "${RUSTC_WRAPPER:-}" && -z "${NATS_HUB_NO_BUILD_CACHE:-}" ]]; then
  if command -v kache >/dev/null 2>&1; then
    export RUSTC_WRAPPER=kache
    _shims="${KACHE_SHIMS_DIR:-$HOME/.local/lib/kache/shims}"
    [[ -d "$_shims" ]] && export PATH="$_shims:$PATH"   # caches the RocksDB C++ build
  elif command -v sccache >/dev/null 2>&1; then
    export RUSTC_WRAPPER=sccache
  fi
fi

free_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
}

# wait_port HOST PORT [TIMEOUT_SECS]
wait_port() {
  local host="$1" port="$2" timeout="${3:-20}" i=0
  while ! (exec 3<>"/dev/tcp/$host/$port") 2>/dev/null; do
    i=$((i + 1))
    if (( i > timeout * 10 )); then return 1; fi
    sleep 0.1
  done
  exec 3>&- 3<&- 2>/dev/null || true
}

require_nats_server() {
  if ! command -v nats-server >/dev/null 2>&1; then
    echo "nats-server not found — run: make setup   (or scripts/dev/install_nats_server.sh)" >&2
    exit 1
  fi
}
