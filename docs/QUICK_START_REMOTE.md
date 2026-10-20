# Quick start — remote client (hub-delegate only)

**nats-hub** is a NATS control plane for agent messaging. One host runs
`nats-server` + `hub-server`. Everyone else is a client: send work with
`hub-delegate`; workers on the bus reply. No server stack on your laptop.

## Install on a remote machine

You need bash, curl (or wget), and a network path to the hub. Rust is only
needed as a fallback.

```bash
curl -fsSL https://raw.githubusercontent.com/jarmen423/agent-communication-server/main/scripts/install_remote.sh \
  | bash -s -- wss://hub.example.com:8080                            # one-liner
bash scripts/install_remote.sh wss://hub.example.com:8080            # from a clone
bash scripts/install_remote.sh <HUB> --from-release v0.2.0           # pin a release
bash scripts/install_remote.sh <HUB> --bin /path/to/hub-delegate     # bring your own binary
bash scripts/install_remote.sh <HUB> --from-source                   # cargo build
```

By default the installer downloads the latest release for your platform
(linux x86_64/arm64, macOS arm64) over HTTPS and verifies it against
`SHA256SUMS`. A mismatch aborts the install. If there is no release or no
asset for your platform, it builds `hub-delegate` from source instead;
`--release-only` turns that fallback off.

It installs every `hub-*` CLI into `~/.local/share/nats-hub/bin/`, plus a
wrapper `~/bin/hub-delegate-remote` with your hub URL baked in. Add `~/bin` to
`PATH` if needed.

**Hub URL:** pass the full URL your operator gave you (`wss://…` or `tls://…`).
A bare host becomes `nats://HOST:4222`, which is **plaintext**. The installer
warns about that unless the host is loopback. Use plaintext only on a trusted
LAN or VPN.

## Delegate a task

A worker must already be online on the hub:

```bash
export NATS_TOKEN='…'   # only if the hub requires auth
hub-delegate-remote --to hermes-worker-1 --from josh \
  --prompt "What is 2+2?" --verbose
```

Expect progress events plus the worker reply (`--timeout 120` default;
`--no-wait` = fire-and-forget).

More: [JOIN_HUB.md](JOIN_HUB.md), [REMOTE_INSTALL.md](REMOTE_INSTALL.md),
[OPERATOR_HUB.md](OPERATOR_HUB.md).
