# W2-C handoff — production deploy artifacts + operator docs

**Wave / task:** wave-2-distributed-hub / W2-C
**Scope:** systemd units + prod NATS config example + SECURITY/JOIN/OPERATOR docs
**Status:** ✅ Complete; verified locally

## Files written

All paths relative to repo root `/home/jfrie/nats`.

| Path | LOC | Purpose |
|---|---|---|
| `config/nats-server.prod.conf.example` | 96 | Production NATS config template (TLS + token OR users + allowlist + limits) |
| `deploy/systemd/nats-hub-nats.service` | 48 | systemd unit for nats-server (user `nats`, hardened sandbox) |
| `deploy/systemd/nats-hub-server.service` | 46 | systemd unit for hub-server (user `nats-hub`, depends on NATS unit) |
| `deploy/systemd/README.md` | 132 | systemd install runbook: users, perms, enable, secrets, logrotate, backup |
| `docs/SECURITY.md` | 233 | Plain-language 4-layer model (network, authN, authZ, TLS) + ASCII diagram |
| `docs/JOIN_HUB.md` | 171 | End-user join runbook: install, two worked examples (token + creds) |
| `docs/OPERATOR_HUB.md` | 295 | Operator stand-up runbook: install, TLS, auth, firewall, units, 13-step verification checklist |
| `docs/REMOTE_AGENTS.md` | +5 | One-line cross-link appended at bottom (body untouched; W2-A owns it) |

Every markdown file has an H1 and is well under the 400-LOC ceiling (max: 295).

## Key decisions

### Token default vs users

- Shipped both options in the config; the **example file's active block is
  `users`** (Option B), with `token` commented out (Option A). Rationale:
  users is the operationally safer default because it supports per-agent
  revocation without rotating everyone. Token remains trivially easy to
  enable for small trusted teams — flip the comment.
- `SECURITY.md` explains the trade-off in plain language; `OPERATOR_HUB.md`
  §4 walks through picking one.
- The `allowed_connection_types: ["WEBSOCKET"]` restriction is on every
  shipped user so a leaked remote credential can't be replayed against the
  plain TCP listener.

### Subject allowlist shape

Default `permissions` block in the example:

- **publish allow:** `hub.send.>`, `hub.register`, `hub.presence`, `hub.events.>`
- **subscribe allow:** `channel.inbox.>`, `channel.task.>`, `channel.session.>`, `channel.wave.>`

This matches the task spec verbatim. Treated as a **ceiling**, not a target:
both `SECURITY.md` (Layer 3) and `OPERATOR_HUB.md` §4 explicitly instruct the
operator to narrow `channel.inbox.>` to `channel.inbox.<identity>` per agent
whenever possible. The widest reasonable default covers the full session /
task / wave surface without granting any admin or API subjects (`hub.api.>`
stays off-limits to remote users — it's only reachable over the loopback
connection from hub-server).

### TLS placement in NATS config

**Spec said** `websocket { cert_file; key_file }` as flat keys. **Actual
nats-server v2.14 syntax** requires them nested inside a `websocket { tls {} }`
block; flat keys produce `unknown field "cert_file"` at startup. Chose to
make the config **actually valid** rather than match the spec verbatim, and
flagged the deviation in an inline comment + this handoff. The
`handshake_timeout`, `same_origin: false`, and port `8080` from the spec are
preserved unchanged.

### Removed `http_host`

nats-server has no `http_host` key (it binds the monitoring port to the same
host as the main listener, or accepts `http: "host:port"` as a combined
form). Removed the invalid key from the template; documented the alternative
in an inline comment. Loopback binding is preserved via `host: "127.0.0.1"`.

### systemd hardening

- `User=nats` / `User=nats-hub` (separate unprivileged system users, no shell)
- `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`,
  `ProtectKernelTunables/Modules`, `ProtectControlGroups`,
  `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`, `LockPersonality`,
  `RestrictRealtime`, `RestrictSUIDSGID`
- `CapabilityBoundingSet=` (empty) and `AmbientCapabilities=` (empty) — no
  Linux capabilities needed since both daemons bind to ports ≥1024
- `ReadWritePaths` minimal: `/var/log/nats-hub /run/nats-hub` (NATS unit) and
  `/var/lib/nats-hub /var/log/nats-hub` (hub-server unit)
- `Restart=on-failure` with `RestartSec=2s` and `StartLimit{IntervalSec=60,Burst=5}`
  in `[Unit]` (correct location; placing these in `[Service]` was the
  initial bug, fixed during verification)
- `RuntimeDirectory=nats-hub` for the NATS unit so systemd creates `/run/nats-hub`
  on start with the right ownership (since `/run` is tmpfs and doesn't persist)

### Docs tone

`SECURITY.md` is deliberately written for a non-expert setting up a hub for
their team for the first time. Avoids jargon, leads each layer with an
"Idea:" one-liner, includes an ASCII diagram up top. `OPERATOR_HUB.md` is a
linear runbook a tired on-call can follow step-by-step. `JOIN_HUB.md` is
written for the team-member side, not the operator.

## Verification output

### `nats-server -t -c config/nats-server.prod.conf.example`

```
$ nats-server -t -c config/nats-server.prod.conf.example
nats-server: config/nats-server.prod.conf.example:33:5: error parsing X509 certificate/key pair: open /etc/nats/tls/nats-server.crt: no such file or directory
exit=1
```

This is **expected** — `/etc/nats/tls/*` is created during deploy (§3 of
OPERATOR_HUB.md) and does not exist on the dev box. The error is post-syntax,
post-schema — the config parses and types check out; nats-server then tries to
load the cert to finalize TLS context. To prove syntax validity I re-ran the
test with throwaway cert/key files at the referenced paths:

```
$ cp config/nats-server.prod.conf.example /tmp/prod-test.conf
$ sed -i 's|/etc/nats/tls/nats-server.crt|/tmp/t.crt|; s|/etc/nats/tls/nats-server.key|/tmp/t.key|' /tmp/prod-test.conf
$ openssl req -x509 -newkey rsa:2048 -sha256 -nodes -days 1 -keyout /tmp/t.key -out /tmp/t.crt -subj "/CN=localhost" 2>/dev/null
$ nats-server -t -c /tmp/prod-test.conf
nats-server: configuration file /tmp/prod-test.conf is valid (sha256:dd27a9cc4b42ddbf792ff5de1e0ca089bad8d1706d205af3bb5e7ffce6999197)
exit=0
```

**Syntax + schema: VALID.** The real-hub cert path is by design and resolves
during deployment.

### `systemd-analyze verify deploy/systemd/*.service`

```
$ systemd-analyze verify deploy/systemd/nats-hub-nats.service deploy/systemd/nats-hub-server.service
nats-hub-server.service: Command /usr/local/bin/hub-server is not executable: No such file or directory
exit=1
```

This is **expected** — `hub-server` is built and installed as step 1 of
`OPERATOR_HUB.md` ("Install binaries"); it doesn't exist on the dev box.
Re-ran the check after `sudo touch /usr/local/bin/hub-server && sudo chmod +x
/usr/local/bin/hub-server`:

```
$ systemd-analyze verify deploy/systemd/nats-hub-nats.service deploy/systemd/nats-hub-server.service
exit=0
```

**No warnings, no errors.** Both units parse cleanly, all directives are in
the correct sections (`StartLimitIntervalSec` and `StartLimitBurst` moved to
`[Unit]` after the first run flagged them), and all hardening keys are
recognized by systemd 259.

### Markdown sanity

```
docs/SECURITY.md          H1=# Security model — in plain language   lines=233
docs/JOIN_HUB.md          H1=# Join a hub — remote agent runbook    lines=171
docs/OPERATOR_HUB.md      H1=# Operator runbook — stand up a hub    lines=295
deploy/systemd/README.md  H1=# systemd Units — nats-hub             lines=132
```

All four have proper H1s and are well under the 400-LOC ceiling.

### REMOTE_AGENTS.md cross-link

Appended one line at the very bottom (body untouched, W2-A owns it):

```
See `docs/OPERATOR_HUB.md` for full deploy, `docs/JOIN_HUB.md` for joining, `docs/SECURITY.md` for the security model.
```

A sibling subagent (W2-A, `sa-0-5d5d79d9`) modified the body of
`REMOTE_AGENTS.md` concurrently; my append landed cleanly at the new EOF.

## Residual risks

1. **No live end-to-end run.** Verification was structural (config parses,
   units parse, markdown well-formed). No daemon was started, no
   `systemctl` was run, no real client connected. W2-E (token+WS dogfood
   scripts) owns the end-to-end live proof.
2. **TLS cert placement deviation from spec.** Spec called for flat
   `cert_file`/`key_file` inside `websocket {}`; nats-server requires a
   nested `tls {}` block. Chose valid-over-verbatim; documented inline and
   above. If the spec owner disagrees, the fix is a one-line edit but the
   config will not start on a real nats-server.
3. **`http_host` dropped.** Spec didn't mention it; the key doesn't exist
   in nats-server. If the intent was to bind monitoring independently of
   the data listener, the correct form is `http: "127.0.0.1:8222"` (a
   single `host:port` string), not a separate `http_host`. Left a comment.
4. **Self-signed cert path is friction for end users.** `OPERATOR_HUB.md`
   §3a recommends ACME; §3b documents self-signed as the fallback. End
   users joining a self-signed hub must obtain and trust the CA file
   themselves (covered in `JOIN_HUB.md`).
5. **Token mode leaks identity.** With token authN, all clients look
   identical in logs. `SECURITY.md` flags this; users[] is the
   recommended default. Operators who pick token anyway lose
   auditability.
6. **No automated config-test in CI.** The `nats-server -t` and
   `systemd-analyze verify` commands work locally; they are not wired
   into CI. A future task could add them as a pre-merge check so config
   drift is caught automatically.
7. **`hub-server --help` flag surface assumed.** The systemd unit calls
   `hub-server --db-path /var/lib/nats-hub/nats_hub.db`. This matches
   the existing CLI documented in `AGENTS.md` and `PRODUCT_VISION.md`.
   If the binary's flag set changes, the unit file must be updated in
   lockstep.
8. **Sibling-subagent concurrent edit.** `REMOTE_AGENTS.md` was modified
   by W2-A during this task. My one-line append at the bottom was
   preserved. If W2-A rewrites the file again, the cross-link should be
   re-verified.

## Out of scope (explicitly not touched)

- `src/`, any `*.py` file — not modified.
- Body of `docs/REMOTE_AGENTS.md` — not modified (only the one-line
  cross-link at the bottom).
- No `git commit`, no `systemctl` invocation, no daemon started.
- NKEY/JWT credentials-file authN is mentioned as a forward path but not
  templated; out of scope per task spec.
- No reverse-proxy (nginx/caddy) config for the monitoring endpoint; out
  of scope.
