#!/usr/bin/env bash
# Reclaim disk from finished work. Safe by default: only removes git worktrees
# whose HEAD is already contained in origin/main (i.e. merged), plus their
# target dirs, then trims the shared compile cache.
#
# Usage: scripts/dev/prune.sh [--dry-run]
set -euo pipefail
source "$(dirname "$0")/lib.sh"
cd "$REPO_ROOT"

dry=0; [[ "${1:-}" == "--dry-run" ]] && dry=1
run() { if (( dry )); then echo "  would: $*"; else "$@"; fi; }

git fetch -q origin main || echo "warning: could not fetch origin/main; using local ref"

echo "==> merged worktrees"
main_wt="$(git rev-parse --show-toplevel)"
git worktree list --porcelain | awk '/^worktree /{print $2}' | while read -r wt; do
  [[ "$wt" == "$main_wt" ]] && continue
  head="$(git -C "$wt" rev-parse HEAD 2>/dev/null || true)"
  [[ -z "$head" ]] && continue
  if git merge-base --is-ancestor "$head" origin/main 2>/dev/null; then
    if [[ -n "$(git -C "$wt" status --porcelain 2>/dev/null)" ]]; then
      echo "  skip (uncommitted changes): $wt"; continue
    fi
    echo "  remove: $wt ($(du -sh "$wt" 2>/dev/null | cut -f1))"
    run git worktree remove --force "$wt"
  else
    echo "  keep (not merged): $wt"
  fi
done
run git worktree prune

if command -v kache >/dev/null 2>&1; then
  echo "==> kache gc"
  run kache gc
fi
echo "==> disk: $(df -h "$REPO_ROOT" | awk 'NR==2 {print $4 " free of " $2}')"
