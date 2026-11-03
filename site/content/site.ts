// Site-wide constants: one place for every outbound URL.

/**
 * Where "Request an invite" goes. There is no waitlist backend yet.
 * PLACEHOLDER: replace the address before launch.
 */
export const INVITE_REQUEST_URL =
  "mailto:invites@example.com?subject=ACS%20invite%20request&body=Which%20agents%20do%20you%20want%20to%20connect%3F";

/** Production origin, used for canonical URLs, the sitemap and Open Graph. */
export const SITE_URL = process.env.NEXT_PUBLIC_SITE_URL ?? "https://acs.agentmemorylabs.com";

export const HOSTED_URL = "https://acs.agentmemorylabs.com";
export const SIGN_IN_URL = `${HOSTED_URL}/signin`;
export const TERMS_URL = `${HOSTED_URL}/terms`;
export const PRIVACY_URL = `${HOSTED_URL}/privacy`;

export const GITHUB_REPO = "jarmen423/agent-communication-server";
export const GITHUB_URL = `https://github.com/${GITHUB_REPO}`;
export const GITHUB_BLOB = `${GITHUB_URL}/blob/main`;
export const GITHUB_TREE = `${GITHUB_URL}/tree/main`;
export const LICENSING_URL = `${GITHUB_BLOB}/LICENSING.md`;

export const SITE_NAME = "Agent Communication Server";
export const SITE_SHORT = "ACS";
export const SITE_DESCRIPTION =
  "Agent Communication Server is a message hub for coding agents. Delegate a task to Claude Code, Codex, Cursor or any CLI agent on any machine, get one structured result back, and watch every message.";

export const NAV = [
  { href: "/docs", label: "Docs" },
  { href: "/pricing", label: "Pricing" },
  { href: GITHUB_URL, label: "GitHub" },
] as const;
