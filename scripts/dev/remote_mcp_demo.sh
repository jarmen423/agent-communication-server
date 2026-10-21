#!/usr/bin/env bash
# Remote MCP demo: operator stack (tenant T_A, durable bound hub-server),
# echo worker, DEV ONLY stub control plane, remote MCP endpoint. Prints the
# URL to paste into claude.ai → Customize → Connectors → Add custom connector.
#
# Usage: scripts/dev/remote_mcp_demo.sh [--no-tunnel]
#   Requires: make setup && make build && scripts/dev/install_nsc.sh
#   claude.ai needs public https URLs. With `cloudflared` on PATH the script
#   opens two quick tunnels (control plane + endpoint). Or bring your own:
#   PUBLIC_ISSUER_URL=https://cp.example PUBLIC_MCP_URL=https://mcp.example/mcp
#   forwarding to the printed local ports (CP_PORT / MCP_PORT to pin them).
# Stop with Ctrl-C. Everything lives in the operator stack's temp dir.
set -euo pipefail
source "$(dirname "$0")/lib.sh"

if [[ "${1:-}" != --inside ]]; then
    exec "$REPO_ROOT/scripts/dev/operator_stack.sh" "$0" --inside "$@"
fi
shift
tunnel=1
[[ "${1:-}" == --no-tunnel ]] && tunnel=0

PY="$REPO_ROOT/.venv/bin/python"
cp_port="${CP_PORT:-$(free_port)}"
mcp_port="${MCP_PORT:-$(free_port)}"
demo="$OPERATOR_DIR/remote-mcp"
mkdir -p "$demo"
pids=()
stop() {
    for pid in "${pids[@]}"; do kill -TERM "$pid" 2>/dev/null || true; done
    for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
}
trap stop EXIT INT TERM

quick_tunnel() {  # quick_tunnel NAME PORT → prints the https URL
    local log="$demo/tunnel-$1.log" url="" i
    cloudflared tunnel --no-autoupdate --url "http://127.0.0.1:$2" >"$log" 2>&1 &
    pids+=("$!")
    for i in $(seq 1 300); do
        url="$(grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' "$log" | head -1 || true)"
        [[ -n "$url" ]] && break
        sleep 0.1
    done
    [[ -n "$url" ]] || { echo "cloudflared tunnel for $1 did not come up (see $log)" >&2; exit 1; }
    echo "$url"
}

issuer="${PUBLIC_ISSUER_URL:-}"
resource="${PUBLIC_MCP_URL:-}"
if [[ -z "$issuer" || -z "$resource" ]]; then
    if (( tunnel )) && command -v cloudflared >/dev/null 2>&1; then
        issuer="$(quick_tunnel cp "$cp_port")"
        resource="$(quick_tunnel mcp "$mcp_port")/mcp"
    else
        (( tunnel )) && echo "cloudflared not found: local-only demo (claude.ai can't reach it)" >&2
        issuer="http://127.0.0.1:$cp_port"
        resource="http://127.0.0.1:$mcp_port/mcp"
    fi
fi

"$PY" -m stub_control_plane --port "$cp_port" --issuer "$issuer" --resource "$resource" \
    --nsc-home "$OPERATOR_DIR/nsc" --tenant T_A --state-file "$demo/state.json" \
    --service-key-file "$demo/service.key" --allow-loopback-redirects \
    >"$demo/stub-cp.log" 2>&1 &
pids+=("$!")
wait_port 127.0.0.1 "$cp_port" 20
"$PY" -m mcp_remote --port "$mcp_port" --resource-url "$resource" --issuer "$issuer" \
    --cp-url "http://127.0.0.1:$cp_port" --nats-url "$NATS_URL" \
    --service-key-file "$demo/service.key" >"$demo/mcp-remote.log" 2>&1 &
pids+=("$!")
NATS_CREDENTIALS_FILE="$OPERATOR_CREDS_DIR/echo-1.creds" \
    "$PY" "$REPO_ROOT/echo_worker.py" --identity echo-1 --nats-url "$NATS_URL" \
    >"$demo/echo-1.log" 2>&1 &
pids+=("$!")
wait_port 127.0.0.1 "$mcp_port" 20

cat <<EOF

  ACS remote MCP demo (stub control plane is DEV ONLY)

  Connector URL:   $resource
  Authorization:   $issuer   (auto-approves the dev user; mints claude-web-<n>)
  Worker:          echo-1 (reverses the prompt)
  NATS:            $NATS_URL (operator mode, tenant T_A)
  Logs:            $demo

  claude.ai: Customize → Connectors → "+" → Add custom connector → paste the
  connector URL → Connect. Then ask: "delegate 'hello' to echo-1 and get the result".

  Ctrl-C to stop.
EOF
wait "${pids[@]}"
