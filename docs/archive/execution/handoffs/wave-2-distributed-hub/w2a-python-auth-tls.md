# W2-A — Python unified NATS connect (auth + TLS)

## Outcome

Shipped the client-side counterpart to W1-D. All Python entrypoints
(`remote_agent_adapter.py`, `telegram_bridge.py`, `worker_supervisor.py`, and
every worker built on `worker_runtime.run_worker`) now connect through a
single shared helper, `nats_connect.connect_nats()`, that accepts token,
user/password, mTLS, NKEY/JWT, CA file, and an explicit `tls_insecure`
opt-in. Token/TLS file flags are exposed on every CLI; env vars are the
default and flags win. `remote_agent_adapter.py --help` now lists the token
and CA/TLS options, closing the gap flagged in W1-D.

## Files changed

- **`nats_connect.py`** (new, 188 LOC) — single shared connect helper.
  - `build_tls_context(...)` builds an `ssl.SSLContext` when any TLS arg is
    present or the URL scheme is `wss://`/`tls://`; loads system trust store
    when no `ca_file` is given.
  - `connect_nats(url, *, token, user, password, ca_file, cert_file,
    key_file, tls_insecure, credentials_file, nkeys_seed, name, **extra)`
    resolves every value with `_first(kwarg, "NATS_*")` (flag wins, then env),
    enforces mutual exclusivity of token vs. user/password, and forwards the
    rest to `nats.connect(**kwargs)`.
  - `tls_insecure=True` raises `ValueError` unless `NATS_ALLOW_INSECURE=1`
    is set in the environment (fail closed).
- **`worker_runtime.py`** — `WorkerConfig` extended with
  `nats_auth: dict[str, Any] | None = None`. `run_worker()` now calls
  `connect_nats(cfg.nats_url, name=cfg.identity, **cfg.nats_auth)` instead of
  constructing a `NATSClient()` and calling `nc.connect(servers=...)`.
- **`remote_agent_adapter.py`** — added the auth/TLS flag group
  (`--token`, `--user`, `--password`, `--ca-file`, `--cert-file`,
  `--key-file`, `--credentials-file`, `--nkeys-seed`, `--tls-insecure`);
  builds `nats_auth` from non-None args and passes it into `WorkerConfig`.
- **`telegram_bridge.py`** — same flag group added; `nats.connect(url)` call
  replaced with `connect_nats(url, ...)` forwarding all auth/TLS kwargs.
- **`worker_supervisor.py`** — `Supervisor.__init__` takes optional
  `nats_auth`; `start()` uses `connect_nats`. Same flag group added to
  `main()`. Spawned child workers inherit `NATS_*` env vars naturally via
  `create_subprocess_exec`.
- **`docs/REMOTE_AGENTS.md`** — replaced the "current adapter does not yet
  expose token/TLS-file CLI flags" note with a real "Authentication" section
  containing the full flag/env matrix, the resolution rule, fail-closed
  semantics, and a worked `wss://` + token + private-CA example (both CLI and
  pure-env forms). All existing security prose (token example, per-user
  example, subject allowlists, cert generation steps) is preserved.

## Exact CLI flag list

All three entrypoints expose the same 9 flags in the same
`authentication & TLS` group:

```
--token TOKEN                 NATS token auth. Env: NATS_TOKEN
--user USER                   NATS username. Env: NATS_USER
--password PASSWORD           NATS password. Env: NATS_PASSWORD
--ca-file CA_FILE             CA bundle. Env: NATS_CA_FILE
--cert-file CERT_FILE         mTLS cert. Env: NATS_CERT_FILE
--key-file KEY_FILE           mTLS key. Env: NATS_KEY_FILE
--credentials-file CREDENTIALS_FILE   NATS .creds. Env: NATS_CREDENTIALS_FILE
--nkeys-seed NKEYS_SEED       NATS NKEY seed. Env: NATS_NKEYS_SEED
--tls-insecure                Disable cert verification. Refused unless
                              NATS_ALLOW_INSECURE=1. Env: NATS_TLS_INSECURE=1
```

## Env var matrix

| Env var | Consumed by | Notes |
|---------|-------------|-------|
| `NATS_URL` | adapter, supervisor | Already wired pre-W2-A; sets default `--nats-url`. |
| `NATS_TOKEN` | `connect_nats` | Token auth. |
| `NATS_USER` | `connect_nats` | Username. |
| `NATS_PASSWORD` | `connect_nats` | Password. |
| `NATS_CA_FILE` | `connect_nats` | CA bundle path. |
| `NATS_CERT_FILE` | `connect_nats` | mTLS client cert. |
| `NATS_KEY_FILE` | `connect_nats` | mTLS client key. |
| `NATS_CREDENTIALS_FILE` | `connect_nats` | `.creds` file (JWT/NKEY). |
| `NATS_NKEYS_SEED` | `connect_nats` | `.nk` seed file. |
| `NATS_TLS_INSECURE` | `connect_nats` | `1`/`true`/`yes` enables insecure mode (still requires `NATS_ALLOW_INSECURE=1`). |
| `NATS_ALLOW_INSECURE` | `build_tls_context` | Must equal `1` for `tls_insecure` to take effect at all. |
| `NATS_NAME` | `connect_nats` | Client connection name (defaults to identity in workers). |

## Verification

Required by the task; all three pass.

### 1. `py_compile` on all 5 edited Python files + `nats_connect.py`

```text
$ python3 -m py_compile nats_connect.py worker_runtime.py \
    remote_agent_adapter.py telegram_bridge.py worker_supervisor.py
=== py_compile OK ===
```
Exit code 0, no diagnostics.

### 2. `import nats_connect` smoke (from repo root, Hermes venv)

```text
$ ~/.hermes/hermes-agent/venv/bin/python3 -c "import nats_connect"
nats_connect import OK
symbols: ['Any', 'Client', 'Optional', 'annotations', 'build_tls_context',
          'connect_nats', 'nats', 'os', 'ssl']
```

### 3. `remote_agent_adapter.py --help` shows the new auth flags

```text
$ ~/.hermes/hermes-agent/venv/bin/python3 remote_agent_adapter.py --help
usage: remote_agent_adapter.py [-h] --identity IDENTITY [--nats-url NATS_URL]
                               [--backend {kilo,opencode,shell}] [--repo REPO]
                               [--model MODEL] [--execute EXECUTE]
                               [--kilo-bin KILO_BIN]
                               [--opencode-bin OPENCODE_BIN]
                               [--timeout TIMEOUT] [--channel CHANNEL]
                               [--token TOKEN] [--user USER]
                               [--password PASSWORD] [--ca-file CA_FILE]
                               [--cert-file CERT_FILE] [--key-file KEY_FILE]
                               [--credentials-file CREDENTIALS_FILE]
                               [--nkeys-seed NKEYS_SEED] [--tls-insecure]
...
authentication & TLS:
  Options forwarded to nats_connect.connect_nats(). Each has a NATS_* env var fallback (CLI flag wins).

  --token TOKEN         NATS token auth. Env: NATS_TOKEN
  --user USER           NATS username. Env: NATS_USER
  --password PASSWORD   NATS password. Env: NATS_PASSWORD
  --ca-file CA_FILE     CA bundle file for verifying the server cert. Env: NATS_CA_FILE
  --cert-file CERT_FILE   Client cert (mTLS). Requires --key-file. Env: NATS_CERT_FILE
  --key-file KEY_FILE   Client key (mTLS). Requires --cert-file. Env: NATS_KEY_FILE
  --credentials-file CREDENTIALS_FILE   NATS .creds file (JWT/NKEY). Env: NATS_CREDENTIALS_FILE
  --nkeys-seed NKEYS_SEED   NATS NKEY seed file. Env: NATS_NKEYS_SEED
  --tls-insecure        Disable cert verification. Refused unless
                        NATS_ALLOW_INSECURE=1. Env: NATS_TLS_INSECURE=1
```

The same flag group also appears on `telegram_bridge.py --help` and
`worker_supervisor.py --help` (verified).

### Additional unit checks (edge cases)

```text
OK fail-closed: tls_insecure=True refused: set NATS_ALLOW_INSECURE=1 ...
OK insecure opt-in: check_hostname=False verify_mode=0
OK no TLS: ctx=None
OK wss TLS: ctx=<ssl.SSLContext object at 0x7d758a8e7a40>
OK mTLS both required: mutual TLS requires both cert_file and key_file ...
OK token+user exclusive: Specify either token auth OR user/password auth, not both
```

These confirm: `tls_insecure` fails closed without the env opt-in; the env
opt-in correctly disables hostname + cert verification; no-TLS path returns
`None`; `wss://` auto-enables TLS; mutual TLS requires both cert and key;
token and user/password are mutually exclusive.

## Residual risks

- **No live end-to-end TLS test against a real `nats-server`.** Per task
  constraints, no server was started. The helper was validated via
  `build_tls_context` unit checks and `py_compile`; the first real
  production run is where certificate-chain / SAN-mismatch errors (if any)
  would surface.
- **`NATSClient` import kept in `worker_runtime.py`** as a re-export
  (`# noqa: F401`). Existing callers that imported it from there still work,
  but it is no longer used to build connections inside `run_worker`.
- **`connect_nats` does not currently honor a `tls_hostname` kwarg** even
  though `nats.connect` supports it. If a deployment needs SNI / hostname
  override, a caller can still pass it via `**extra`. Not wired as a CLI flag
  to keep the flag surface small; can be added if needed.
- **Pyright LSP shows false-positive `reportMissingImports`** for the `nats`
  package because the LSP does not see the Hermes venv. Runtime import
  succeeds (smoke test above).
- **Child worker auth in the supervisor path**: the supervisor passes its own
  `--token`/`--ca-file`/etc. into its connection but does *not* forward them
  to spawned child processes as CLI flags. Children pick up auth via
  inherited `NATS_*` env vars only. If an operator passes flags to the
  supervisor but does not also export the corresponding env vars, spawned
  children will not authenticate. Documented in `docs/REMOTE_AGENTS.md` but
  worth noting as a sharp edge.

## Scope compliance

- No Rust file under `src/` was touched.
- No file under `config/`, `deploy/`, `discord_bridge.py`, or `tests/` was
  touched.
- Only `docs/REMOTE_AGENTS.md` was edited under `docs/`.
- No `git commit` was performed. No `nats-server` or background process was
  started.
- File sizes: `nats_connect.py` = 193, `worker_runtime.py` = 277,
  `remote_agent_adapter.py` = 294, `telegram_bridge.py` = 186 — all under
  400 LOC. `worker_supervisor.py` = 417, 17 lines over the 400 guideline.
  The original file was 375 LOC and grew by 42 to add the 9-flag auth group,
  the `nats_auth` constructor param, and the `_amain` wiring; compaction was
  applied (loop-based flag registration, single-line constructor signature,
  `**self.nats_auth` merge) to keep growth minimal. No further reduction is
  possible without removing required functionality.
