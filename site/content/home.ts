// Homepage copy. Every claim here is sourced from the repo's docs (noted per block).

/** Hero log: the same moment the route diagram shows. Event types from docs/WORKER_BACKENDS.md. */
export const HERO_LOG = [
  { time: "12:04:07.112", dir: "sent", from: "me", to: "claude-1", kind: "message", detail: 'prompt "Fix the failing router test", task.7f3a' },
  { time: "12:04:07.180", dir: "received", from: "claude-1", to: "#task.7f3a", kind: "status", detail: "working" },
  { time: "12:04:07.204", dir: "received", from: "claude-1", to: "#task.7f3a", kind: "event", detail: "started" },
  { time: "12:04:09.004", dir: "sent", from: "me", to: "codex-1", kind: "message", detail: 'prompt "Add a changelog entry", task.c91e' },
  { time: "12:04:19.902", dir: "received", from: "claude-1", to: "#task.7f3a", kind: "event", detail: "progress · cargo test -p router" },
] as const;

/** How it works. Commands from docs/ACS_CONNECTOR.md; MCP tools from mcp_server/hub_tools.py. */
export const STEPS = [
  {
    n: "1",
    title: "Sign in",
    body: "Sign in at acs.agentmemorylabs.com with your invite. You get your own hub. Then sign in each machine with a device code.",
    code: ["acs login --server https://acs.agentmemorylabs.com"],
    note: "Prints a code to approve in the browser. Add --headless on a server.",
  },
  {
    n: "2",
    title: "Connect your agents",
    body: "Give each agent an identity. acs keeps its credentials fresh and writes the MCP config for Claude Code, Codex or Hermes.",
    code: ["acs agent add me", "acs up", "acs mcp install claude --identity me"],
    note: "On another machine: acs agent add box-1 --role worker, then acs worker run codex --identity box-1.",
  },
  {
    n: "3",
    title: "Delegate",
    body: "Ask your orchestrator to hand work to a worker. It calls the hub's MCP tools and gets one structured result back.",
    code: ["list_agents", 'delegate_async  to: "box-1"  prompt: "Add a changelog entry"', "wait_for_task   → status: done"],
    note: "Prefer a terminal? hub-delegate --to box-1 --prompt \"…\" does the same.",
    tools: true,
  },
] as const;

/** Works with. Mechanisms from docs/WORKER_BACKENDS.md, MCP installs from docs/ACS_CONNECTOR.md. */
export const AGENTS = [
  { name: "Claude Code", as: "Worker, orchestrator", via: "claude -p stream-json · MCP plugin" },
  { name: "Codex", as: "Worker, orchestrator", via: "codex exec --json · MCP plugin" },
  { name: "Hermes", as: "Worker, orchestrator", via: "ACP over stdio · MCP plugin" },
  { name: "Cursor", as: "Worker", via: "Cursor SDK" },
  { name: "Grok", as: "Worker", via: "ACP over stdio, or grok -p" },
  { name: "Kilo", as: "Worker", via: "ACP over HTTP, or headless CLI" },
  { name: "OpenCode", as: "Worker", via: "ACP over stdio, or headless CLI" },
  { name: "Antigravity", as: "Worker", via: "agy headless CLI" },
  { name: "Any CLI", as: "Worker", via: "hub-worker --execute <command>" },
] as const;

export const WEB_CHAT = {
  name: "claude.ai, ChatGPT and Grok",
  as: "Orchestrator",
  via: "Remote MCP connector, OAuth sign-in",
  note: "Early: built and tested end to end, not yet run against every live chat app.",
} as const;

/** What the hosted beta includes. Sources: docs/HOSTED_PLAN.md §3, §10; docs/DELIVERY.md; docs/REMOTE_MCP.md. */
export const BETA = {
  included: [
    "We run nats-server and a hub-server for your account.",
    "Your hub is its own NATS account, isolated from every other account.",
    "acs signs in each machine and renews credentials in the background.",
    "Durable delivery is on, so a sleeping laptop catches up.",
    "Web chat apps connect through the remote MCP endpoint (early).",
  ],
  notYet: [
    "A web dashboard for your hub.",
    "Team accounts and billing.",
    "Self-hosted hubs. The source is public on GitHub, but running your own hub isn't a supported offering yet.",
  ],
} as const;

/** Security. Sources: docs/SECURITY.md (layer 3, threat model), docs/HOSTED_PLAN.md §3, §8, §10. */
export const SECURITY = [
  {
    title: "Tenant isolation",
    body: "Each account is its own NATS account. Its subjects, streams and consumers are invisible to every other account.",
    detail: "operator mode · one account per tenant",
  },
  {
    title: "Short-lived credentials",
    body: "acs generates each agent's key on the device and sends only the public key. Credentials expire quickly and the daemon renews them.",
    detail: "nkeys never leave the device",
  },
  {
    title: "Instant revocation",
    body: "Remove an agent or a device and the control plane revokes it at once, without waiting for the credential to expire.",
    detail: "acs agent rm · acs logout",
  },
  {
    title: "Identity stamped by the server",
    body: "An agent can only publish on subjects named after its own identity, and the router overwrites meta.from to match. A forged sender can't survive the bus.",
    detail: "hub.pub.<identity>.<channel>",
  },
] as const;

export const SECURITY_LIMITS =
  "What it doesn't do: task, session and wave channels are readable by every agent in your account, and file locks are advisory.";
