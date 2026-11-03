import type { Metadata } from "next";
import Link from "next/link";
import { notFound } from "next/navigation";
import { DocsShell } from "@/components/docs/docs-shell";
import { DocsEnhancer } from "@/components/docs/enhance";
import { Toc } from "@/components/docs/toc";
import { DOC_PAGES } from "@/content/docs-manifest";
import { GITHUB_BLOB } from "@/content/site";
import { groupOf, loadDoc, neighbours } from "@/lib/docs";

type Props = { params: Promise<{ slug: string }> };

export const dynamicParams = false;

export function generateStaticParams() {
  return DOC_PAGES.map((d) => ({ slug: d.slug }));
}

export async function generateMetadata({ params }: Props): Promise<Metadata> {
  const { slug } = await params;
  const doc = await loadDoc(slug);
  if (!doc) return {};
  return {
    title: `${doc.entry.title} · Docs`,
    description: doc.description,
    alternates: { canonical: `/docs/${slug}` },
    openGraph: { title: `${doc.entry.title} · Agent Communication Server docs`, description: doc.description },
  };
}

export default async function DocPage({ params }: Props) {
  const { slug } = await params;
  const doc = await loadDoc(slug);
  if (!doc) notFound();
  const { prev, next } = neighbours(slug);
  const group = groupOf(slug);
  const siteAuthored = doc.entry.source.startsWith("site/");

  return (
    <DocsShell current={slug} currentTitle={doc.entry.title} aside={<Toc items={doc.toc} />}>
      <article>
        <header className="docs-header">
          <p className="eyebrow">{group?.title}</p>
          <h1 className="docs-title">
            {doc.entry.title}
            {doc.entry.experimental ? <span className="tag align-middle">experimental</span> : null}
          </h1>
          {doc.entry.experimental ? (
            <p className="docs-callout">
              Experimental: this plugin is new, and its configuration and behaviour may change between releases.
            </p>
          ) : null}
        </header>
        <div className="prose" dangerouslySetInnerHTML={{ __html: doc.html }} />
        <DocsEnhancer />
        <footer className="docs-footer">
          {siteAuthored ? null : (
            <a className="link type-caption" href={`${GITHUB_BLOB}/${doc.entry.source}`}>
              View {doc.entry.source} on GitHub
            </a>
          )}
          <nav aria-label="Previous and next" className="docs-pager">
            {prev ? (
              <Link href={`/docs/${prev.slug}`} className="docs-pager-link" rel="prev">
                <span className="type-caption text-ink-muted">Previous</span>
                <span>{prev.title}</span>
              </Link>
            ) : (
              <span />
            )}
            {next ? (
              <Link href={`/docs/${next.slug}`} className="docs-pager-link text-right" rel="next">
                <span className="type-caption text-ink-muted">Next</span>
                <span>{next.title}</span>
              </Link>
            ) : null}
          </nav>
        </footer>
      </article>
    </DocsShell>
  );
}
