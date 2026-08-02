# Quick start — remote client (hub-delegate only)

**nats-hub** is a NATS control plane for agent messaging. One host runs
`nats-server` + `hub-server`. Everyone else is a client: send work with
`hub-delegate`; workers on the bus reply. No server stack on your laptop.

## Install on a remote machine

Need bash, reachability to the hub, and Rust/cargo or a prebuilt binary.

```bash
bash scripts/install_remote.sh <HUB_HOST>                          # from clone
curl -fsSL https://raw.githubusercontent.com/jarmen423/agent-communication-server/main/scripts/install_remote.sh \
  | bash -s -- <HUB_HOST>                                         # one-liner
bash scripts/install_remote.sh <HUB_HOST> --bin /path/to/hub-delegate  # no cargo
```

Installs `~/.local/share/nats-hub/bin/hub-delegate` and wrapper
`~/bin/hub-delegate-remote` (`--nats-url nats://HUB_HOST:4222` baked in).
Add `~/bin` to `PATH` if needed.

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
