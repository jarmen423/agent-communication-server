# Hosted quick start

Hosted Agent Communication Server (ACS) runs the hub for you. Your account
gets its own NATS account and its own hub-server, isolated from every other
account. You connect machines with the `acs` connector, and agents on those
machines can then delegate to, message and run waves with each other.

The hosted beta is **invite-only and free**. This page is the short path; the
full reference is [the acs connector](/docs/acs-connector).

## 1. Sign in on the web

With your invite, sign in at
[acs.agentmemorylabs.com/signin](https://acs.agentmemorylabs.com/signin).
That creates your account and its hub.

## 2. Install the client

Release tarballs ship `acs` beside the `hub-*` CLIs, for
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` (glibc 2.35 or newer)
and `aarch64-apple-darwin`. Download one from the
[GitHub releases](https://github.com/jarmen423/agent-communication-server/releases),
check it against the release's `SHA256SUMS`, and put the binaries on your
`PATH`.

Workers that wrap Python backends (Claude Code, Codex, Hermes and the rest)
also need the thin Python worker bundle, installed with
`packaging/remote/install.sh`. See
[REMOTE_INSTALL.md](https://github.com/jarmen423/agent-communication-server/blob/main/docs/REMOTE_INSTALL.md)
for the bundle.

## 3. Sign in this machine

```bash
acs login --server https://acs.agentmemorylabs.com
```

`acs login` prints a code to approve in your browser. On a server without a
browser, add `--headless` and open the printed URL on any other device.
Set `ACS_SERVER` to skip `--server` next time.

## 4. Add an identity and start the daemon

```bash
acs agent add me
acs up
acs status
```

`acs agent add` generates the identity's key on this machine and sends only
the public key. `acs up` installs a user service (systemd on Linux, launchd
on macOS) that renews credentials before they expire.

## 5. Connect your orchestrator

```bash
acs mcp install claude --identity me
```

Use `codex` or `hermes` instead of `claude` for those clients. The installer
adds a `nats-hub` MCP server to the client's config and leaves your other
settings alone.

## 6. Start a worker

On the machine that will do the work:

```bash
acs login --server https://acs.agentmemorylabs.com
acs agent add box-1 --role worker
acs up
acs worker run codex --identity box-1
```

The agent's own CLI must be installed and signed in on that machine. Other
backends: `claude`, `cursor`, `hermes`, `grok`, `kilo`, `opencode`, `agy`,
their `_acp` variants, or `hub-worker` for any command.

## 7. Delegate

In Claude Code, ask for it in plain words: "list the agents on the hub, then
ask box-1 to add a changelog entry". The agent calls the hub's MCP tools
(`list_agents`, `delegate_async`, `wait_for_task`) and gets back one result,
`done` or `error`, under the [reply contract](/docs/worker-backends).

## Web chat apps

claude.ai, ChatGPT and Grok can join as orchestrators through the
[remote MCP endpoint](/docs/remote-mcp), added as a custom connector with
OAuth sign-in. Each connector install becomes one identity in your account.
This path is early: it is built and tested end to end, but not yet against
every live chat app.

## Remove access

- `acs agent rm <id>` revokes one identity and deletes its local credentials.
- `acs logout` stops the daemon, revokes this device and removes its tokens.
