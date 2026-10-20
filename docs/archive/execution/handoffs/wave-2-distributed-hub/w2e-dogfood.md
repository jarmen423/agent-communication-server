# W2-E — Token auth + WebSocket remote adapter round-trip (local dogfood)

> **Partial residual closed in W3:** Residual #1 (“hub-server does not thread token”)
> was fixed in wave-3 (`ControlPlane` / query API / `ApiClient` use
> `HubConnectOptions::from_env`). Full-stack token + `wss://` is proven by
> `scripts/dogfood_wss_tls.sh`. This handoff remains valid as the **ws:// +
> split-auth** dogfood design note.

## Outcome

**PASS.** Self-contained bash dogfood proves the end-to-end round-trip:

```
hub-delegate (Rust, anonymous TCP)
   └─▶ hub-server (Rust router, anonymous TCP)
          └─▶ nats-server
                 ├─ TCP 14222 anonymous (loopback, trusted control plane)
                 └─ WS  18080 **token-gated** ← security boundary under test
                        ↑
                 remote_agent_adapter.py (Python, ws:// + --token)
                 identity=dogfood-remote, backend=shell `echo`
```

`hub-delegate --to dogfood-remote --prompt "ping-wave2e"` returns exit 0 with
the full event sequence (`started → working → progress → completed`) and the
echoed payload `ping-wave2e` in stdout.

A negative control first proves the WS listener rejects every connection that
does not present the token, so the only way the adapter could have joined the
bus is by presenting `dogfood-token-wave2e`. That is the security claim.

## Files

| Path | Change |
|------|--------|
| `scripts/dogfood_token_auth.sh` | **NEW** — self-contained dogfood (mktemp dir, NATS conf, nats-server, hub-server, adapter, hub-delegate, negative control, cleanup trap). Executable. |
| `scripts/dogfood_remote_ws.sh` | **NEW** — thin alias wrapper (`exec dogfood_token_auth.sh "$@"`) so the ROADMAP-named path resolves. Executable. |

WRITE-SCOPE respected: no edits to `src/`, `packaging/`, discord, or deploy units.

## Command

```bash
CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats \
  bash scripts/dogfood_token_auth.sh
# (scripts/dogfood_remote_ws.sh is an alias for the same thing)
```

## Run evidence (single real run, 2026-07-19)

Final RC: `0`. Excerpt of the relevant stdout:

```
=== W2-E dogfood: token auth + WS remote adapter round-trip ===
TMP_DIR=/data/tmp/nats-dogfood-oA2ZX4
TCP_PORT=14222 (anonymous, loopback)  WS_PORT=18080 (token-gated)  HTTP_PORT=18222
[conf] written -> /data/tmp/nats-dogfood-oA2ZX4/nats.conf  (TCP anonymous, WS requires token)
[start] nats-server pid=881831
[wait] nats-server(TCP) on 14222 listening (after 1s)
[wait] nats-server(WS) on 18080 listening (after 0s)
=== negative control: WS connect WITHOUT token (expect rejection) ===
nats.errors.Error: nats: 'Authorization Violation'
OK-rejected: Error: nats: 'Authorization Violation'
[neg] PASS — WS without token is rejected
[start] hub-server pid=881865
[start] hub-server still alive, assuming subscribed
[start] remote_agent_adapter pid=882487 (ws:// + token)
[wait] adapter connected after 2s
=== round-trip: hub-delegate --to dogfood-remote --prompt ping-wave2e ===
[delegate] exit=0
--- delegate output ---
[event] dogfood-remote — started
[status] dogfood-remote — working
[event] dogfood-remote — progress
[event] dogfood-remote — completed
ping-wave2e
--- end delegate output ---

=== adapter.log tail (evidence) ===
[remote-adapter] identity=dogfood-remote backend=shell nats_url=ws://127.0.0.1:18080
[remote:dogfood-remote] connected to NATS as dogfood-remote
[remote:dogfood-remote] subscribed to channel.inbox.dogfood-remote (oneshot + sessions)
[remote:dogfood-remote] ready
[remote:dogfood-remote] oneshot on task.8405679d: ping-wave2e...
[remote-adapter] [shell-backend] exec: echo ping-wave2e

=== nats.log tail (evidence) ===
[881831] [INF] Listening for websocket clients on ws://0.0.0.0:18080
[881831] [INF] Listening for client connections on 0.0.0.0:14222
[881831] [INF] Server is ready
[881831] [ERR] 127.0.0.1:53244 - wid:6 - authentication error       ← negative-control probe rejected
[881831] [ERR] 127.0.0.1:53244 - wid:6 - read error: authentication error

=== hub-server.log tail (evidence) ===
[hub-server] control plane connected, entering routing loop
INFO nats_hub::router: control plane subscribed to hub.send.>, hub.register, hub.presence

==================== PASS ====================
```

Verified twice (once via `dogfood_token_auth.sh`, once via the
`dogfood_remote_ws.sh` alias). No leaked processes or ports after either run.

## Design notes / threat-model framing

The script uses **split-authorization** (per-listener):
- TCP listener is anonymous, bound to loopback — the trusted control plane
  (hub-server, hub-delegate) lives on the same host.
- WS listener requires the token — this is the boundary remote agents cross.

This mirrors the deployment pattern documented in
`config/nats-server.conf` + `docs/REMOTE_AGENTS.md` (trusted control network
vs. untrusted remote-agent network). The token is the only thing standing
between an untrusted remote machine and the bus, and the negative control
proves it is actually enforced on the WS listener.

The fixed token in the script (`dogfood-token-wave2e`) is a loopback fixture,
not a real secret. No tokens are committed anywhere else; the NATS conf the
script writes lives under `/data/tmp/nats-dogfood-*` and is `rm -rf`'d on exit.

## Residual risks / known gaps

1. **~~hub-server daemon does not yet thread a token through to NATS.~~ CLOSED in W3.**
   `ControlPlane::connect`, query API listener, and `ApiClient` all use
   `HubConnectOptions::from_env()`. Set `NATS_TOKEN` (or user/creds) for the
   hub process. See `scripts/dogfood_wss_tls.sh`.

2. **Token is shared-secret, not per-agent.** The prod example in
   `config/nats-server.prod.conf.example` sketches `authorization { users = [...] }` with
   per-agent credentials and `allowed_connection_types: ["WEBSOCKET"]`. The
   dogfood uses a single shared token for simplicity; the nats_connect +
   remote_agent_adapter surfaces already support `--user/--password`,
   `--credentials-file`, and `--nkeys-seed`, so upgrading to per-agent creds
   is a script-only change.

3. **~~No TLS.~~ Separate dogfood:** `scripts/dogfood_wss_tls.sh` covers
   `wss://` + example CA. This script (`dogfood_token_auth.sh`) stays
   loopback `ws://` + split-auth for a faster smoke path.

4. **Shell backend only.** The round-trip uses `--backend shell --execute echo`.
   Named backends (kilo, opencode) are not exercised here; they have their
   own integration paths under `worker_backends/`.

5. **Adapter stdout buffering.** Python's stdout is unbuffered
   (`PYTHONUNBUFFERED=1` + `python3 -u`) so the script's grep-on-logfile
   readiness check sees the "connected to NATS" banner promptly. Without
   `-u` the banner can take ~15s to flush, which is a footgun for any
   future caller that waits on the adapter's stdout.
