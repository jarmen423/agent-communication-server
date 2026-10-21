#!/usr/bin/env bash
# Hosted ACS, end to end on one machine (all DEV ONLY pieces, no cloud):
#   1. scripts/dogfood_hosted.py — operator-mode tenant: acs login → agent add
#      → mcp install → delegate (echo + optional real agent) → wave → file
#      lock → backup/restore
#   2. make test-remote-mcp       — web-chat path: OAuth + stateless remote MCP
#   3. visualizer smoke           — browser path (XSS/CSP checks included)
#
# Usage: scripts/dogfood_hosted.sh [--real-worker claude|codex|hermes|grok]
# Needs: make setup && make build; scripts/dev/install_nsc.sh;
#        (cd tests/e2e_visualizer && npm ci && npx playwright install chromium)
set -uo pipefail
cd "$(dirname "$0")/.."
rc=0
echo "=== 1/3 hosted tenant journey"
scripts/dev/operator_stack.sh --nats-only .venv/bin/python scripts/dogfood_hosted.py "$@" || rc=1
echo; echo "=== 2/3 remote MCP endpoint (web chat apps)"
make --no-print-directory test-remote-mcp || rc=1
echo; echo "=== 3/3 visualizer smoke"
NO_BUILD=1 bash tests/e2e_visualizer/run_smoke.sh || rc=1
echo; (( rc == 0 )) && echo "=== dogfood_hosted: ALL PASS ===" || echo "=== dogfood_hosted: FAILURES (see above) ==="
exit "$rc"
