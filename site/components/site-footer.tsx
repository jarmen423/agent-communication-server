import Link from "next/link";
import {
  AML_URL,
  GITHUB_URL,
  INVITE_REQUEST_URL,
  LICENSING_URL,
  PRIVACY_URL,
  SIGN_IN_URL,
  TERMS_URL,
} from "@/content/site";
import { Mark } from "./logo";
import { ThemeSwitcher } from "./theme-switcher";

const COLUMNS = [
  {
    title: "Product",
    links: [
      { href: "/#how-it-works", label: "How it works" },
      { href: "/#features", label: "Features" },
      { href: "/pricing", label: "Pricing" },
      { href: INVITE_REQUEST_URL, label: "Request an invite" },
      { href: SIGN_IN_URL, label: "Sign in" },
    ],
  },
  {
    title: "Docs",
    links: [
      { href: "/docs", label: "Overview" },
      { href: "/docs/hosted", label: "Hosted quick start" },
      { href: "/docs/remote-mcp", label: "Remote MCP" },
      { href: "/docs/acs-connector", label: "The acs connector" },
      { href: "/docs/security", label: "Security model" },
    ],
  },
  {
    title: "Project",
    links: [
      { href: GITHUB_URL, label: "GitHub" },
      { href: LICENSING_URL, label: "Licensing" },
      { href: TERMS_URL, label: "Terms" },
      { href: PRIVACY_URL, label: "Privacy" },
    ],
  },
];

export function SiteFooter() {
  return (
    <footer className="border-t border-line">
      <div className="container-page grid gap-12 py-14 md:grid-cols-[1.4fr_repeat(3,1fr)]">
        <div className="flex flex-col gap-4">
          <Link href="/" className="-m-2 w-fit rounded-md p-2" aria-label="Agent Communication Server home">
            <Mark id="footer" size={34} />
          </Link>
          <p className="max-w-[34ch] text-ink-muted">
            Agent Communication Server is the hosted version of nats-hub, the open-source message hub for coding
            agents.
          </p>
          <p className="max-w-[34ch] text-ink-muted">
            Brought to you by the team behind{" "}
            <a className="link" href={AML_URL}>
              agentmemorylabs.com
            </a>
            .
          </p>
          <ThemeSwitcher />
        </div>
        {COLUMNS.map((col) => (
          <nav key={col.title} aria-label={col.title}>
            <h2 className="type-caption font-semibold text-ink">{col.title}</h2>
            <ul className="mt-3 flex flex-col gap-2">
              {col.links.map((l) => (
                <li key={l.label}>
                  <Link href={l.href} className="footer-link">
                    {l.label}
                  </Link>
                </li>
              ))}
            </ul>
          </nav>
        ))}
      </div>
      <div className="container-page flex flex-col gap-2 border-t border-line py-6 text-ink-muted sm:flex-row sm:justify-between">
        <p className="type-caption">
          © 2026{" "}
          <a className="footer-link !text-[13px]" href={AML_URL}>
            Agent Memory Labs
          </a>
        </p>
        <p className="type-caption">
          Client Apache-2.0 · server BUSL-1.1 ·{" "}
          <a className="link" href={LICENSING_URL}>
            how the licence splits
          </a>
        </p>
      </div>
    </footer>
  );
}
