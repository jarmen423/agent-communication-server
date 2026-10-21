#!/usr/bin/env bash
# Isolated operator/JWT stack. Usage: operator_stack.sh [--nats-only] <command...>
# Requires make setup/build and install_nsc.sh. No global nsc state is touched.
set -euo pipefail
source "$(dirname "$0")/lib.sh"
umask 077
PYTHON_BIN="${PYTHON_BIN:-$REPO_ROOT/.venv/bin/python}"
NSC_BIN="${NSC_BIN:-$REPO_ROOT/.tools/bin/nsc}"
HUB_ADMIN_BIN="${HUB_ADMIN_BIN:-$CARGO_TARGET_DIR/debug/hub-admin}"
HUB_SERVER_BIN="${HUB_SERVER_BIN:-$CARGO_TARGET_DIR/debug/hub-server}"
require_nats_server
[[ -x "$NSC_BIN" ]] || { echo 'Run scripts/dev/install_nsc.sh first' >&2; exit 1; }
[[ -x "$HUB_ADMIN_BIN" ]] || { echo 'Run make build first' >&2; exit 1; }
nats_only=0
if [[ "${1:-}" == --nats-only ]]; then nats_only=1; shift; fi
[[ $# -gt 0 ]] || { echo 'Usage: operator_stack.sh [--nats-only] <command...>' >&2; exit 2; }
export OPERATOR_DIR="$(mktemp -d "${TMPDIR:-/tmp}/hub-operator-XXXXXX")"
export NSC_BIN HUB_ADMIN_BIN
export NATS_URL="nats://127.0.0.1:$(free_port)"
export OPERATOR_PORT="${NATS_URL##*:}"
export OPERATOR_CREDS_DIR="$OPERATOR_DIR/creds/T_A"
pids=()
cleanup() {
    local rc=$?
    for pid in "${pids[@]}"; do kill -TERM "$pid" 2>/dev/null || true; done
    for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
    if [[ $rc != 0 || -n "${KEEP_LOGS:-}" ]]; then
        echo "Operator logs/keys retained (private): $OPERATOR_DIR" >&2
        tail -15 "$OPERATOR_DIR"/*.log >&2 || true
    else rm -rf "$OPERATOR_DIR"; fi
}
trap cleanup EXIT
"$PYTHON_BIN" - <<'PY'
import json, os, pathlib, subprocess
root = pathlib.Path(os.environ['OPERATOR_DIR'])
nsc = [os.environ['NSC_BIN'], '-H', str(root / 'nsc')]
def run(*args):
    result = subprocess.run([*nsc, *args], text=True, capture_output=True)
    if result.returncode:
        raise RuntimeError(f'nsc {args}: {result.stdout} {result.stderr}')
    return result.stdout
run('add', 'operator', '--name', 'H0', '--sys')
users = {'hub-server': 'hub-server', 'bob': 'orchestrator', 'carol': 'worker',
         'echo-1': 'worker', 'slow-1': 'worker', 'sup-1': 'service', 'admin-1': 'admin',
         'rotate-rust': 'worker', 'rotate-python': 'worker', 'rotate-mcp': 'orchestrator'}
for account in ('T_A', 'T_B'):
    run('add', 'account', '--name', account)
    # Three hub streams reserve 1 GiB each. Quota must exceed their sum.
    run('edit', 'account', '--name', account, '--js-disk-storage', '4G',
        '--js-mem-storage', '64M', '--js-streams', '8', '--js-consumer', '64', '--conns', '64')
    for role in ('worker', 'orchestrator', 'admin', 'service', 'hub-server'):
        policy = json.loads(subprocess.check_output(
            [os.environ['HUB_ADMIN_BIN'], 'permission-template', '--role', role]))
        args = ['edit', 'signing-key', '--account', account, '--sk', 'generate', '--role', role]
        for kind in ('pub', 'sub'):
            for subject in policy[kind]['allow']:
                args.extend([f'--allow-{kind}', subject])
        if 'resp' in policy:
            args.extend([f"--allow-pub-response={policy['resp']['max']}", '--response-ttl', '2s'])
        run(*args)
    dest = root / 'creds' / account
    dest.mkdir(parents=True)
    for user, role in users.items():
        run('add', 'user', '--account', account, '--name', user, '-K', role)
        run('generate', 'creds', '--account', account, '--name', user,
            '--output-file', str(dest / f'{user}.creds'))
run('generate', 'config', '--nats-resolver', '--config-file', str(root / 'resolver.conf'))
config = (root / 'resolver.conf').read_text()
# nsc's resolver path is relative by default; keep all state under this stack.
config = config.replace("dir: './jwt'", f'dir: "{root}/jwt"')
(root / 'resolver.conf').write_text(config)
(root / 'nats.conf').write_text(
    f'host: 127.0.0.1\nport: {os.environ["OPERATOR_PORT"]}\n'
    'include resolver.conf\n'
    f'jetstream {{ store_dir: "{root}/js", max_file_store: 9G, max_memory_store: 256M }}\n')
PY
nats-server -t -c "$OPERATOR_DIR/nats.conf"
nats-server -c "$OPERATOR_DIR/nats.conf" >"$OPERATOR_DIR/nats.log" 2>&1 &
pids+=("$!")
wait_port 127.0.0.1 "$OPERATOR_PORT"
"$NSC_BIN" -H "$OPERATOR_DIR/nsc" push --all --account-jwt-server-url "$NATS_URL" >"$OPERATOR_DIR/push.log" 2>&1
# Auth from the caller must never leak into the throwaway stack.
unset NATS_USER NATS_PASSWORD NATS_TOKEN NATS_NKEY NATS_CREDENTIALS_FILE
if (( ! nats_only )); then
    for tenant in T_A T_B; do
        NATS_CREDENTIALS_FILE="$OPERATOR_DIR/creds/$tenant/hub-server.creds" \
        NATS_USER=hub-server NATS_HUB_IDENTITY=hub-server "$HUB_SERVER_BIN" --nats-url "$NATS_URL" \
            --db-path "$OPERATOR_DIR/db-$tenant" --durable --require-bound-identity \
            --api-admin admin-1 >"$OPERATOR_DIR/hub-$tenant.log" 2>&1 &
        pids+=("$!")
    done
    sleep 2
    for pid in "${pids[@]}"; do kill -0 "$pid"; done
fi
export NATS_HUB_OPERATOR_TEST_STACK=1
cd "$REPO_ROOT"
"$@"
