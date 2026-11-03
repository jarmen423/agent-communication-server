import Link from "next/link";
import { DOC_GROUPS } from "@/content/docs-manifest";

function NavList({ current }: { current?: string }) {
  return (
    <nav aria-label="Docs" className="docs-nav">
      <Link href="/docs" className="docs-nav-link" aria-current={current === undefined ? "page" : undefined}>
        Overview
      </Link>
      {DOC_GROUPS.map((g) => (
        <div key={g.title} className="mt-6">
          <h2 className="docs-nav-group">{g.title}</h2>
          <ul className="mt-1">
            {g.pages.map((p) => (
              <li key={p.slug}>
                <Link
                  href={`/docs/${p.slug}`}
                  className="docs-nav-link"
                  aria-current={current === p.slug ? "page" : undefined}
                >
                  {p.nav ?? p.title}
                  {p.experimental ? <span className="tag !ml-2">experimental</span> : null}
                </Link>
              </li>
            ))}
          </ul>
        </div>
      ))}
    </nav>
  );
}

/** Desktop: a sticky column. Below lg: a disclosure, so the menu works without JavaScript. */
export function DocsSidebar({ current, currentTitle }: { current?: string; currentTitle: string }) {
  return (
    <>
      <details className="docs-nav-mobile lg:hidden">
        <summary>
          <span className="text-ink-muted">Docs</span>
          <span aria-hidden="true">/</span>
          <span className="truncate">{currentTitle}</span>
        </summary>
        <div className="pb-4 pt-2">
          <NavList current={current} />
        </div>
      </details>
      <aside className="docs-sidebar hidden lg:block">
        <NavList current={current} />
      </aside>
    </>
  );
}
