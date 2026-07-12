# W1-D — NATS WebSocket TLS and Authentication

## Outcome

Completed the owned configuration and documentation work without changing the
active development behavior. WebSocket port `8080` and `no_tls: true` remain
active; all production configuration in `config/nats-server.conf` is commented
out.

## Changes

- `config/nats-server.conf`
  - Added explained, commented production `cert_file` and `key_file` settings.
  - Added explained, commented token authentication example.
  - Added explained, commented username/password example restricted with
    `allowed_connection_types: ["WEBSOCKET"]`.
- `docs/REMOTE_AGENTS.md`
  - Expanded security guidance with concrete token and per-user examples.
  - Added per-agent subject allowlists for inbox/task channels and required
    registration, presence, and event publications.
  - Added production TLS certificate generation, configuration validation,
    restart, and `nats-py` token/TLS client examples.
  - Documented that the current remote adapter does not expose token/TLS-file
    CLI options; wiring those options is outside W1-D's owned scope.

## Verification

```text
$ nats-server -t -c /home/jfrie/nats/config/nats-server.conf
nats-server: configuration file /home/jfrie/nats/config/nats-server.conf is valid (sha256:2c2c6f59facaab1df171e3537c7ae0a8210891fba3ba0b8cb36035306e75936b)
```

`git diff --check -- config/nats-server.conf docs/REMOTE_AGENTS.md` passed.

## Scope / blocker note

No blocker for the requested config/docs deliverable. End-to-end production use
of the adapter is not yet possible through its CLI because
`remote_agent_adapter.py --help` currently has no token, username/password, CA,
or TLS options. No Python files were changed, per task constraints.
