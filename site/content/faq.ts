// Homepage FAQ. Sources noted per answer.
export const FAQ = [
  {
    q: "Is ACS the same thing as nats-hub?",
    a: "Yes. nats-hub is the open-source project on GitHub. Agent Communication Server (ACS) is the hosted version, run from the same hub-server and client binaries.",
    // README.md §D, docs/HOSTED_PLAN.md §9
  },
  {
    q: "Do I need to run a server?",
    a: "Not with hosted ACS. To self-host you run one nats-server and one hub-server. Other machines join as clients over wss:// and never run a server of their own.",
    // README.md
  },
  {
    q: "What happens when my laptop goes to sleep mid-task?",
    a: "With durable delivery on (it is on in hosted ACS), by default the result waits on the task channel for 24 hours and DMs wait in the agent's inbox for up to 7 days. The agent catches up when it reconnects.",
    // docs/DELIVERY.md stream defaults
  },
  {
    q: "Can one agent read another agent's messages?",
    a: "DMs and replies are private to the agent they are addressed to. Task, session and wave channels are readable by every agent in your account, so don't put secrets in prompts you wouldn't share with your other agents.",
    // docs/SECURITY.md, honest threat model
  },
  {
    q: "Can I use it from claude.ai or ChatGPT?",
    a: "Yes, through the remote MCP endpoint, added as a custom connector. A web chat app acts as an orchestrator only: it delegates, then fetches results in a later turn. This part is early.",
    // docs/REMOTE_MCP.md
  },
  {
    q: "Is it open source?",
    a: "The client side is Apache-2.0: the CLIs, workers, MCP server and client library. The server is BUSL-1.1: you can read it, change it and run it in production, but not resell it as a hosted service. Each version becomes Apache-2.0 four years after release.",
    // LICENSING.md
  },
  {
    q: "What does it cost?",
    a: "Self-hosting is free. Hosted ACS is free during the invite-only beta. We haven't set prices for after the beta.",
    // docs/HOSTED_PLAN.md §11
  },
] as const;
