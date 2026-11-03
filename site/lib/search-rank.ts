import type { SearchEntry } from "./search-types";

export type Ranked = { entry: SearchEntry; score: number; snippet?: string };

const normalize = (s: string) =>
  s
    .toLowerCase()
    .replace(/[-_/.]+/g, " ")
    .replace(/[^\p{L}\p{N}#@ ]+/gu, " ")
    .replace(/\s+/g, " ")
    .trim();

const LIMIT = 40;

/**
 * A tiered scorer, enough for a few hundred curated entries:
 * exact label > label prefix > word start in label > substring in label >
 * every word in label > every word somewhere in the text.
 */
export function rank(pool: SearchEntry[], query: string): Ranked[] {
  const q = normalize(query);
  if (!q) {
    return pool
      .filter((e) => (e.kind === "action" && !e.searchOnly) || e.kind === "page")
      .map((entry) => ({ entry, score: 0 }));
  }
  const words = q.split(" ");
  const out: Ranked[] = [];
  for (const entry of pool) {
    const label = normalize(entry.heading ?? entry.title);
    const hay = `${label} ${normalize(entry.title)} ${normalize(entry.text)}`;
    if (!words.every((w) => hay.includes(w))) continue;
    let score: number;
    if (label === q) score = 100;
    else if (label.startsWith(q)) score = 80;
    else if (` ${label}`.includes(` ${q}`)) score = 60;
    else if (label.includes(q)) score = 45;
    else if (words.every((w) => label.includes(w))) score = 35;
    else score = 10;
    score += entry.kind === "page" ? 4 : entry.kind === "action" ? 2 : 0;
    out.push({ entry, score, snippet: score <= 14 ? snippet(entry.text, words[0]) : undefined });
  }
  out.sort((a, b) => b.score - a.score);
  return out.slice(0, LIMIT);
}

function snippet(text: string, word: string): string | undefined {
  const at = text.toLowerCase().indexOf(word);
  if (at < 0) return undefined;
  const start = Math.max(0, at - 36);
  const end = Math.min(text.length, at + 72);
  return `${start > 0 ? "…" : ""}${text.slice(start, end).trim()}${end < text.length ? "…" : ""}`;
}
