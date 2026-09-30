#!/usr/bin/env bash
# Sync the canonical MCP server into every plugin's server/ directory.
#
# Canonical sources (edit these, never the plugin copies):
#   mcp_server/*.py      — the unified orchestrator MCP server
#   nats_connect.py      — repo-root connect helper, vendored into mcp_server/
#                          and each plugin so installs stay self-contained
#
# Usage:
#   scripts/dev/sync_plugins.sh           copy canonical → mcp_server/nats_connect.py → */server/
#   scripts/dev/sync_plugins.sh --check   exit 1 if any copy has drifted (for tests/CI)
set -euo pipefail
source "$(dirname "$0")/lib.sh"
cd "$REPO_ROOT"

CANON_DIR="mcp_server"
PLUGIN_SERVERS=(
  "claude-code-plugin/server"
  "codex-plugin/server"
  "hermes-plugin/server"
)

check_only=0
[[ "${1:-}" == "--check" ]] && check_only=1

drift=0
sync_file() {  # sync_file <src> <dst>
  local src="$1" dst="$2"
  if [[ -f "$dst" ]] && cmp -s "$src" "$dst"; then
    return 0
  fi
  if (( check_only )); then
    echo "DRIFT: $dst != $src" >&2
    drift=1
  else
    mkdir -p "$(dirname "$dst")"
    cp "$src" "$dst"
    echo "synced: $dst"
  fi
}

# 1. Vendor nats_connect.py into the canonical dir (so it is self-contained).
sync_file "nats_connect.py" "$CANON_DIR/nats_connect.py"

# 2. Mirror the canonical dir into each plugin's server/.
for dir in "${PLUGIN_SERVERS[@]}"; do
  # Remove stale copies that are no longer canonical (e.g. old mcp_server.py).
  if [[ -d "$dir" ]]; then
    for f in "$dir"/*.py; do
      [[ -e "$f" ]] || continue
      base="$(basename "$f")"
      if [[ ! -f "$CANON_DIR/$base" ]]; then
        if (( check_only )); then
          echo "DRIFT: $f has no canonical counterpart" >&2
          drift=1
        else
          rm "$f"
          echo "removed stale: $f"
        fi
      fi
    done
  fi
  for f in "$CANON_DIR"/*.py; do
    sync_file "$f" "$dir/$(basename "$f")"
  done
done

if (( check_only )); then
  if (( drift )); then
    echo "plugin server copies are out of sync — run scripts/dev/sync_plugins.sh" >&2
    exit 1
  fi
  echo "plugin server copies in sync"
fi
exit 0
