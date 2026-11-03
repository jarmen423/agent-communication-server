// Build-time Markdown rendering for /docs: GitHub-compatible heading anchors,
// links rewritten to resolve inside the site, Shiki highlighting (no client JS),
// and copy buttons that a small client script switches on.
import path from "node:path";
import GithubSlugger from "github-slugger";
import { Marked, type Tokens } from "marked";
import { createCssVariablesTheme, createHighlighter, type Highlighter } from "shiki";
import { GITHUB_BLOB, GITHUB_TREE } from "@/content/site";

export type TocItem = { id: string; text: string; depth: 2 | 3 };
export type Rendered = { html: string; toc: TocItem[]; title: string; description: string };

const LANGS = ["bash", "json", "rust", "toml", "python", "yaml", "typescript", "javascript", "ini", "diff"] as const;
const ALIASES: Record<string, string> = { sh: "bash", shell: "bash", console: "bash", zsh: "bash", ts: "typescript", js: "javascript", py: "python", yml: "yaml", conf: "ini" };

const theme = createCssVariablesTheme({ name: "acs-lane", variablePrefix: "--shiki-", fontStyle: true });
let highlighter: Promise<Highlighter> | null = null;
const getHighlighter = () => (highlighter ??= createHighlighter({ themes: [theme], langs: [...LANGS] }));

const escapeHtml = (s: string) =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

/** Plain text of inline markdown, for TOC labels and descriptions. */
export function plain(md: string): string {
  return md
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/<[^>]+>/g, "")
    .replace(/[`~]|\*\*?|(?<![\p{L}\p{N}])_+|_+(?![\p{L}\p{N}])/gu, "")
    .replace(/\s+/g, " ")
    .trim();
}

/**
 * Resolve a link found in `source` (repo-relative). Docs in the manifest map
 * to /docs/<slug>; everything else in the repo goes to GitHub.
 */
export function resolveHref(href: string, source: string, slugFor: (repoPath: string) => string | undefined): string {
  if (/^(https?:|mailto:|tel:|#|\/)/.test(href)) return href;
  const [rawPath, hash = ""] = href.split("#");
  const repoPath = path.posix.normalize(path.posix.join(path.posix.dirname(source), decodeURIComponent(rawPath)));
  if (repoPath.startsWith("..")) return href;
  const anchor = hash ? `#${hash}` : "";
  const slug = slugFor(repoPath.replace(/\/$/, ""));
  if (slug) return `/docs/${slug}${anchor}`;
  const isDir = rawPath.endsWith("/") || !path.posix.extname(repoPath);
  return `${isDir ? GITHUB_TREE : GITHUB_BLOB}/${repoPath.replace(/\/$/, "")}${anchor}`;
}

export async function renderMarkdown(
  md: string,
  source: string,
  slugFor: (repoPath: string) => string | undefined,
): Promise<Rendered> {
  const hl = await getHighlighter();
  const slugger = new GithubSlugger();
  const toc: TocItem[] = [];
  let title = "";
  let description = "";
  const marked = new Marked({ gfm: true });

  marked.use({
    walkTokens(token) {
      if (!description && token.type === "paragraph") description = plain((token as Tokens.Paragraph).text).slice(0, 180);
    },
    renderer: {
      heading({ tokens, depth, text }) {
        const id = slugger.slug(plain(text));
        const inner = this.parser.parseInline(tokens);
        if (depth === 1) {
          title ||= plain(text);
          return ""; // the page renders its own title
        }
        if (depth === 2 || depth === 3) toc.push({ id, text: plain(text), depth });
        return `<h${depth} id="${id}"><a class="anchor" href="#${id}" aria-hidden="true" tabindex="-1">#</a>${inner}</h${depth}>\n`;
      },
      link({ href, title: t, tokens }) {
        const to = resolveHref(href, source, slugFor);
        const external = /^https?:/.test(to) && !to.startsWith("https://acs.agentmemorylabs.com");
        const label = this.parser.parseInline(tokens);
        return `<a href="${escapeHtml(to)}"${t ? ` title="${escapeHtml(t)}"` : ""}${external ? ' rel="noopener"' : ""}>${label}</a>`;
      },
      code({ text, lang }) {
        const name = (lang ?? "").trim().split(/\s+/)[0].toLowerCase();
        const resolved = ALIASES[name] ?? name;
        const known = (LANGS as readonly string[]).includes(resolved);
        const body = known
          ? hl.codeToHtml(text, { lang: resolved, theme: "acs-lane" })
          : `<pre class="shiki" tabindex="0"><code>${escapeHtml(text)}</code></pre>`;
        const label = name || "text";
        return `<figure class="code-block lane"><figcaption><span>${escapeHtml(label)}</span><button type="button" class="copy-btn" data-copy hidden>Copy</button></figcaption>${body}</figure>\n`;
      },
      table(token) {
        const head = token.header
          .map((c, i) => `<th scope="col"${align(token.align[i])}>${this.parser.parseInline(c.tokens)}</th>`)
          .join("");
        const rows = token.rows
          .map((r) => `<tr>${r.map((c, i) => `<td${align(token.align[i])}>${this.parser.parseInline(c.tokens)}</td>`).join("")}</tr>`)
          .join("");
        return `<div class="table-wrap" tabindex="0" role="region" aria-label="Table"><table><thead><tr>${head}</tr></thead><tbody>${rows}</tbody></table></div>\n`;
      },
    },
  });

  const html = await marked.parse(md);
  return { html, toc, title, description };
}

function align(a: "left" | "right" | "center" | null) {
  return a ? ` style="text-align:${a}"` : "";
}
