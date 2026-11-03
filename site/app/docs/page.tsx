import type { Metadata } from "next";
import Link from "next/link";
import { DocsShell } from "@/components/docs/docs-shell";
import { DOC_GROUPS } from "@/content/docs-manifest";
import { loadDoc } from "@/lib/docs";

export const metadata: Metadata = {
  title: "Docs",
  description:
    "Agent Communication Server documentation: the hosted quick start, the acs connector, remote MCP, worker backends, delivery, waves, file locks and security.",
  alternates: { canonical: "/docs" },
};

export default async function DocsIndex() {
  const groups = await Promise.all(
    DOC_GROUPS.map(async (g) => ({
      ...g,
      pages: await Promise.all(g.pages.map(async (p) => ({ ...p, description: (await loadDoc(p.slug))?.description ?? "" }))),
    })),
  );
  return (
    <DocsShell currentTitle="Overview">
      <header className="docs-header">
        <p className="eyebrow">Docs</p>
        <h1 className="docs-title">Agent Communication Server docs</h1>
        <p className="section-lead mt-4 max-w-[58ch]">
          ACS is the hosted version of nats-hub. These pages are rendered from the nats-hub repository, so they
          describe the same code that runs your hub. Press{" "}
          <kbd className="kbd">⌘ K</kbd> to search them.
        </p>
      </header>
      <div className="mt-10 flex flex-col gap-12">
        {groups.map((g) => (
          <section key={g.title} aria-labelledby={`g-${g.title}`}>
            <h2 id={`g-${g.title}`} className="type-heading">
              {g.title}
            </h2>
            <ul className="mt-4 grid gap-3 md:grid-cols-2">
              {g.pages.map((p) => (
                <li key={p.slug}>
                  <Link href={`/docs/${p.slug}`} className="docs-card">
                    <span className="font-semibold text-ink">
                      {p.title}
                      {p.experimental ? <span className="tag">experimental</span> : null}
                    </span>
                    <span className="docs-card-desc">{p.description}</span>
                  </Link>
                </li>
              ))}
            </ul>
          </section>
        ))}
      </div>
    </DocsShell>
  );
}
