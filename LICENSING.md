# Licensing

nats-hub is licensed **by path**, the way Tailscale splits its client and
control server:

- **Client side: [Apache License 2.0](LICENSES/Apache-2.0.txt).** This is
  everything you install to connect agents to a hub.
- **Server side: [Business Source License 1.1](LICENSES/BUSL-1.1.txt)
  (BUSL-1.1).** This is `hub-server`, `hub-admin`, and the code and assets
  only they run.

Copyright © 2026 Agent Memory Labs. The machine-readable form of the table
below is [`REUSE.toml`](REUSE.toml) ([REUSE](https://reuse.software) spec 3.x).
This page explains it in plain language. The license texts in
[`LICENSES/`](LICENSES/) are what actually govern.

## Which paths are under which license

| Paths | License |
|---|---|
| `mcp_server/`, the generated plugin copies `claude-code-plugin/`, `codex-plugin/`, `hermes-plugin/`, and the marketplace manifests `.claude-plugin/`, `.agents/plugins/` | Apache-2.0 |
| Python workers and bridges: `worker_runtime.py`, `worker_events.py`, `worker_backends/`, every `*_worker.py`, `worker_supervisor.py`, `remote_agent_adapter.py`, `hub_delivery.py`, `nats_connect.py`, `telegram_bridge.py`, `discord_bridge.py` | Apache-2.0 |
| Installers: `packaging/remote/`, `scripts/install_remote.sh` | Apache-2.0 |
| Rust client: `src/lib.rs` (crate root), `src/protocol.rs`, `src/client.rs`, `src/client/**`, `src/connect_opts.rs`, `src/query_api_client.rs`, `src/events/**`, `src/tui/**` | Apache-2.0 |
| Client CLIs: `src/bin/**` except the two server binaries below (`hub-delegate`, `hub-publish`, `hub-observe`, `hub-interact`, `hub-register`, `hub-agents`, `hub-worker`, `hub-history`, `hub-session`, `hub-watch`, `hub-wave`, `hub-thread`, `hub-stats`, `hub-tui`) | Apache-2.0 |
| `config/provider_models.json` (model catalog read by the Python workers) | Apache-2.0 |
| **Every other file under `src/**`**, which today means `src/router.rs`, `src/router/**`, `src/query_api.rs`, `src/query_api/**`, `src/orchestrator/**`, `src/storage/**`, `src/analytics/**` (including the live metrics), `src/ws_bridge/**`, `src/wave/**`, `src/delivery/**` | **BUSL-1.1** |
| **Server binaries:** `src/bin/hub_server.rs`, `src/bin/hub_admin.rs`, `src/bin/hub_admin/**` | **BUSL-1.1** |
| **Server assets and deployment:** `visualizer/**` (except the pet sprites below), `deploy/**`, `config/**` (except `provider_models.json`) | **BUSL-1.1** |
| `visualizer/pets/**` (third-party Petdex sprites) | Not ours. See [below](#third-party-code-and-assets) |
| Everything else: docs, tests, dev and dogfood scripts, CI, `packaging/release/`, build files (`Cargo.toml`, `Makefile`, `requirements*.txt`), planning notes | Apache-2.0 |

A new Rust file under `src/` is BUSL-1.1 unless it's added to the client
list in `REUSE.toml`. A new file anywhere else is Apache-2.0.

**Release tarballs.** `hub-server` and `hub-admin` are BUSL-1.1. Every other
`hub-*` binary and the Python remote-worker bundle are Apache-2.0, subject to
the caveat in the next section. Tarballs ship `LICENSE`, `NOTICE`, this file
and `LICENSES/`.

### Until the crate split (H-1)

The Rust code is still one crate (`nats_hub`). Its `Cargo.toml` therefore
declares `Apache-2.0 AND BUSL-1.1`, and the client side still uses a few
BUSL-1.1 files:

- `src/client/durable.rs` and `src/client/catchup.rs` use `src/delivery/`
  (`attach_inbox`, the client stream names, `inbox_consumer_name`, `header`);
- `src/query_api_client.rs` uses `query_api::{subject, subject_bound, ApiResponse}`;
- `hub-agents`, `hub-history`, `hub-thread`, `hub-session`, `hub-wave` and
  `hub-tui` use the record types in `src/storage/types.rs`;
- `hub-wave` uses `src/wave/validate.rs`, and `hub-tui` uses `analytics::DataPoint`.

So the client binaries built from this crate today contain some BUSL-1.1
code. The Additional Use Grant covers ordinary use of them, including in
production. The H-1 client/server crate split (see
[`docs/HOSTED_PLAN.md`](docs/HOSTED_PLAN.md)) moves these shared pieces into
the Apache-2.0 client crate.

## What BUSL-1.1 means here

The parameters are at the top of [`LICENSES/BUSL-1.1.txt`](LICENSES/BUSL-1.1.txt):

- **Licensor:** Agent Memory Labs.
- **Additional Use Grant:** you may make production use of the server,
  including within your organization, provided that you don't offer it, or a
  service substantially derived from it, to third parties as a hosted or
  managed service that lets them use its functionality.
- **Change Date:** four years after a version is first publicly released.
- **Change License:** Apache-2.0.

In practice:

- **You can self-host freely.** Run `hub-server` for yourself, your team or
  your whole company, in production, at no cost. You can read, modify, fork
  and redistribute the code under the same license.
- **You can't sell it as a hosted service.** Running nats-hub (or something
  substantially derived from it) as a hosted or managed hub for other people
  or other organizations needs a commercial license from Agent Memory Labs.
- **Each version becomes Apache-2.0 after four years.** The clock runs per
  version, from that version's first public release.

This summary is not legal advice. If it and the license text disagree, the
license text wins.

## Third-party code and assets

Dependencies keep their own licenses. The one that matters most is the
embedded **SurrealDB** engine (the `surrealdb` crate with `kv-rocksdb`, in
the default `storage-surreal` build). It is under **SurrealDB's own BSL 1.1**,
whose Additional Use Grant forbids offering SurrealDB itself as a database
service. nats-hub keeps SurrealDB behind the `Storage` trait and never exposes
it as a database to users. A hosted nats-hub offers a message hub, not
database access. The `no-storage` build doesn't include SurrealDB. Details:
[`docs/DATABASE_PLAN.md`](docs/DATABASE_PLAN.md) and
[`docs/PORTABILITY.md`](docs/PORTABILITY.md).

The pet sprites under `visualizer/pets/` were copied from Petdex pet installs.
Agent Memory Labs didn't author them and doesn't license them. Their terms
haven't been verified yet
([`LICENSES/LicenseRef-Petdex-Unverified.txt`](LICENSES/LicenseRef-Petdex-Unverified.txt)).

## Contributions

External pull requests can't be accepted until there is a Contributor License
Agreement. Contributions go into both the Apache-2.0 and the BUSL-1.1 parts,
and the Licensor needs the right to relicense the BUSL-1.1 parts (Change
License, commercial licenses). The CLA is future work. Until it exists, please
open an issue instead of a PR.

## Checking the declarations

- `uvx reuse lint` (or `pip install reuse && reuse lint`) checks that every file
  has a license and copyright.
- `tests/python/test_licensing.py` checks that `REUSE.toml`, the license texts
  (pinned SPDX `license-list-data` v3.29.0, SHA-256 verified), `Cargo.toml`, the
  plugin manifests and this file agree.
