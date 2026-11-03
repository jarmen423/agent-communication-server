// Which repo Markdown files become /docs pages, in what order, under which titles.
// `source` is relative to the repository root. Every other repo link in these
// pages points at GitHub. Internal planning docs (HOSTED_PLAN, OPEN_DECISIONS,
// docs/archive, refocus*) are deliberately left out, and so are the self-hosting
// guides (README quick start, JOIN_HUB, OPERATOR_HUB, VISUALIZER) until
// self-hosting is a supported offering.

export type DocEntry = {
  slug: string;
  title: string;
  /** Short label for the sidebar, when the title is long. */
  nav?: string;
  source: string;
  experimental?: boolean;
};

export type DocGroup = { title: string; pages: DocEntry[] };

export const DOC_GROUPS: DocGroup[] = [
  {
    title: "Getting started",
    pages: [
      { slug: "hosted", title: "Hosted quick start", source: "site/content/docs/hosted.md" },
    ],
  },
  {
    title: "Connect agents",
    pages: [
      { slug: "acs-connector", title: "The acs connector", source: "docs/ACS_CONNECTOR.md" },
      { slug: "remote-mcp", title: "Remote MCP for web chat apps", nav: "Remote MCP", source: "docs/REMOTE_MCP.md" },
      { slug: "worker-backends", title: "Worker backends", source: "docs/WORKER_BACKENDS.md" },
      { slug: "bridges", title: "Human bridges", source: "docs/BRIDGES.md" },
    ],
  },
  {
    title: "Coordinate",
    pages: [
      { slug: "delivery", title: "Durable delivery", source: "docs/DELIVERY.md" },
      { slug: "waves", title: "Waves", source: "docs/WAVES.md" },
      { slug: "file-locks", title: "File locks", source: "docs/FILE_LOCKS.md" },
    ],
  },
  {
    title: "Reference",
    pages: [
      { slug: "security", title: "Security model", source: "docs/SECURITY.md" },
    ],
  },
  {
    title: "Experimental",
    pages: [{ slug: "email-plugin", title: "Email plugin", source: "docs/EMAIL_PLUGIN.md", experimental: true }],
  },
];

export const DOC_PAGES: DocEntry[] = DOC_GROUPS.flatMap((g) => g.pages);
