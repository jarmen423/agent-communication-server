// Loads the docs named in content/docs-manifest.ts from the repository at build time.
import { readFile } from "node:fs/promises";
import path from "node:path";
import { cache } from "react";
import { DOC_GROUPS, DOC_PAGES, type DocEntry } from "@/content/docs-manifest";
import { plain, renderMarkdown, type Rendered } from "./markdown";
import type { SearchEntry } from "./search-types";

/** The repository root: this app lives in <repo>/site. */
export const REPO_ROOT = path.resolve(process.cwd(), "..");

const bySource = new Map(DOC_PAGES.map((d) => [d.source, d.slug]));
export const slugFor = (repoPath: string) => bySource.get(repoPath);

export function findDoc(slug: string): DocEntry | undefined {
  return DOC_PAGES.find((d) => d.slug === slug);
}

export function neighbours(slug: string) {
  const i = DOC_PAGES.findIndex((d) => d.slug === slug);
  return { prev: i > 0 ? DOC_PAGES[i - 1] : undefined, next: i >= 0 ? DOC_PAGES[i + 1] : undefined };
}

export function groupOf(slug: string) {
  return DOC_GROUPS.find((g) => g.pages.some((p) => p.slug === slug));
}

// Every docs page is prerendered, so this read only happens at build time.
const readSource = cache(async (source: string) =>
  readFile(path.join(/* turbopackIgnore: true */ REPO_ROOT, source), "utf8"),
);

export const loadDoc = cache(async (slug: string): Promise<(Rendered & { entry: DocEntry }) | undefined> => {
  const entry = findDoc(slug);
  if (!entry) return undefined;
  const md = await readSource(entry.source);
  const rendered = await renderMarkdown(md, entry.source, slugFor);
  return { ...rendered, entry };
});

/** One entry per page and one per h2/h3 section, for the ⌘K menu. */
export async function buildSearchIndex(): Promise<SearchEntry[]> {
  const { Marked } = await import("marked");
  const GithubSlugger = (await import("github-slugger")).default;
  const out: SearchEntry[] = [];
  for (const entry of DOC_PAGES) {
    const md = await readSource(entry.source);
    const tokens = new Marked({ gfm: true }).lexer(md);
    const slugger = new GithubSlugger();
    let section: SearchEntry | null = null;
    const pageText: string[] = [];
    const push = () => {
      if (section) out.push({ ...section, text: section.text.slice(0, 420) });
    };
    for (const t of tokens) {
      if (t.type === "heading") {
        const text = plain(t.text);
        const id = slugger.slug(text);
        if (t.depth === 2 || t.depth === 3) {
          push();
          section = { kind: "section", group: "Sections", title: entry.title, heading: text, url: `/docs/${entry.slug}#${id}`, text: "" };
        }
        continue;
      }
      const raw = "text" in t && typeof t.text === "string" ? t.text : "raw" in t ? String(t.raw) : "";
      const text = t.type === "code" ? raw.split("\n").slice(0, 3).join(" ") : plain(raw);
      if (!text) continue;
      if (section) section.text += (section.text ? " " : "") + text;
      else pageText.push(text);
    }
    push();
    out.push({
      kind: "page",
      group: "Docs",
      title: entry.title,
      url: `/docs/${entry.slug}`,
      text: `${entry.nav ?? ""} ${pageText.join(" ")}`.slice(0, 420),
    });
  }
  return out;
}
