"use client";

import { useEffect, useState } from "react";
import type { TocItem } from "@/lib/markdown";

/** "On this page", with the section in view marked as current. */
export function Toc({ items }: { items: TocItem[] }) {
  const [active, setActive] = useState<string | null>(null);

  useEffect(() => {
    const heads = items.map((i) => document.getElementById(i.id)).filter((e): e is HTMLElement => !!e);
    if (!heads.length) return;
    const io = new IntersectionObserver(
      (entries) => {
        const visible = entries.filter((e) => e.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
        if (visible[0]) setActive(visible[0].target.id);
      },
      { rootMargin: "-88px 0px -70% 0px" },
    );
    heads.forEach((h) => io.observe(h));
    return () => io.disconnect();
  }, [items]);

  if (!items.length) return null;
  return (
    <nav aria-label="On this page" className="docs-toc">
      <h2 className="docs-nav-group">On this page</h2>
      <ul className="mt-2">
        {items.map((i) => (
          <li key={i.id} data-depth={i.depth}>
            <a href={`#${i.id}`} aria-current={active === i.id ? "location" : undefined}>
              {i.text}
            </a>
          </li>
        ))}
      </ul>
    </nav>
  );
}
