// Every plan name, price and limit on the site lives here. Change copy here, not in the layout.
//
// Hosted beta limits are the beta stack's defaults in agentmemorylabs/acs-private
// (AGENTS.md config table, config.example.toml, docs/H-3-BETA.md "Capacity"),
// read on 2026-11-03. Confirm them against the deployed config before launch.

export type Plan = {
  id: "hosted-beta" | "team";
  name: string;
  price: string;
  priceNote: string;
  summary: string;
  features: string[];
  limits?: { label: string; value: string }[];
  cta: { label: string; href: "invite" | "notify" };
  highlight?: boolean;
};

export const PLANS: Plan[] = [
  {
    id: "hosted-beta",
    name: "Hosted beta",
    price: "Free",
    priceNote: "During the invite-only beta",
    summary: "We run your hub. Sign in each machine with acs and connect web chat apps over remote MCP.",
    features: [
      "Your own isolated NATS account and hub-server",
      "acs connector: device sign-in, credential renewal, MCP install",
      "Durable delivery on by default",
      "Remote MCP endpoint for claude.ai, ChatGPT and Grok (early)",
    ],
    limits: [
      { label: "Agent identities", value: "500 per account, 50 per device" },
      { label: "Connections at once", value: "64" },
      { label: "Durable message storage", value: "4 GB" },
      { label: "Message history", value: "Kept 30 days" },
    ],
    cta: { label: "Request an invite", href: "invite" },
    highlight: true,
  },
  {
    id: "team",
    name: "Team",
    price: "Coming later",
    priceNote: "Not available yet",
    summary: "Shared accounts for teams, with billing. We haven't set the price or the unit yet.",
    features: ["Everything in the hosted beta", "Team accounts and a web dashboard", "Billing"],
    cta: { label: "Tell me when it launches", href: "notify" },
  },
];

/** Where the Team plan's "tell me" goes. Same inbox as invites until there is a list. */
export const TEAM_NOTIFY_SUBJECT = "Tell me when ACS Team launches";

export const PRICING_FAQ = [
  {
    q: "Why is there no price yet?",
    a: "We haven't decided what to charge for: seats, agent identities or usage. Rather than guess, the beta is free and the Team plan is listed without a price.",
  },
  {
    q: "Can I self-host instead?",
    a: "Not as a supported offering yet. The source is public on GitHub (client Apache-2.0, server BUSL-1.1), and we'll add a plan here when self-hosting is ready.",
  },
  {
    q: "What are the beta limits for?",
    a: "They keep one account from crowding out the others on shared infrastructure during the beta. They may change, and we'll say so here.",
  },
  {
    q: "What counts as an agent identity?",
    a: "One name on your hub with its own credential, such as claude-1 or box-1. Each web chat connector install is one identity too.",
  },
] as const;
