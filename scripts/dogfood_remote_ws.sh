#!/usr/bin/env bash
# scripts/dogfood_remote_ws.sh
#
# W2-E alias wrapper: the ROADMAP calls the live dogfood
# "scripts/dogfood_remote_ws.sh". The actual implementation lives in
# scripts/dogfood_token_auth.sh (which exercises BOTH token auth AND the
# WebSocket remote-adapter round-trip in one self-contained script).
# This file exists so the ROADMAP path resolves; it just delegates.
#
# Usage: scripts/dogfood_remote_ws.sh [args forwarded to dogfood_token_auth.sh]

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${HERE}/dogfood_token_auth.sh" "$@"
