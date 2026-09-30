#!/usr/bin/env bash
# Sync the canonical MCP server + plugin support files into every plugin.
#
# Canonical sources (edit these, never the plugin copies):
#   mcp_server/*.py      — the unified orchestrator MCP server → */server/
#   mcp_server/hooks/    — session-start hook                 → */hooks/
#   mcp_server/skills/   — agent workflow guide               → */skills/
#   nats_connect.py      — repo-root connect helper, vendored into mcp_server/
#                          and each plugin so installs stay self-contained
#
# Usage:
#   scripts/dev/sync_plugins.sh           copy canonical → plugin dirs
#   scripts/dev/sync_plugins.sh --check   exit 1 if any copy has drifted (for tests/CI)
set -euo pipefail
source "$(dirname "$0")/lib.sh"
cd "$REPO_ROOT"

CANON_DIR="mcp_server"
PLUGINS=(claude-code-plugin codex-plugin hermes-plugin)

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

# Mirror canonical dir $1 into plugin dir $2, removing stale files that have
# no canonical counterpart (e.g. old mcp_server.py). Skips dotfiles and
# __pycache__.
mirror_dir() {  # mirror_dir <src_dir> <dst_dir>
  local src_dir="$1" dst_dir="$2"
  [[ -d "$src_dir" ]] || return 0
  if [[ -d "$dst_dir" ]]; then
    while IFS= read -r -d '' f; do
      local rel="${f#"$dst_dir"/}"
      if [[ ! -f "$src_dir/$rel" ]]; then
        if (( check_only )); then
          echo "DRIFT: $f has no canonical counterpart" >&2
          drift=1
        else
          rm "$f"
          echo "removed stale: $f"
        fi
      fi
    done < <(find "$dst_dir" -type f \( -name '*.py' -o -name '*.md' \) -print0)
  fi
  while IFS= read -r -d '' f; do
    local rel="${f#"$src_dir"/}"
    sync_file "$f" "$dst_dir/$rel"
  done < <(find "$src_dir" -type f -print0)
}

# 1. Vendor nats_connect.py into the canonical dir (so it is self-contained).
sync_file "nats_connect.py" "$CANON_DIR/nats_connect.py"

# 2. Mirror canonical sources into each plugin.
for p in "${PLUGINS[@]}"; do
  # Server: flat *.py only (dir itself is canonical root).
  if [[ -d "$p/server" ]]; then
    for f in "$p/server"/*.py; do
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
    sync_file "$f" "$p/server/$(basename "$f")"
  done
  mirror_dir "$CANON_DIR/hooks" "$p/hooks"
  mirror_dir "$CANON_DIR/skills" "$p/skills"
done

if (( check_only )); then
  if (( drift )); then
    echo "plugin copies are out of sync — run scripts/dev/sync_plugins.sh" >&2
    exit 1
  fi
  echo "plugin copies in sync"
fi
exit 0
