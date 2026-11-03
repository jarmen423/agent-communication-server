"use client";

import { useRouter } from "next/navigation";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { GITHUB_URL, INVITE_REQUEST_URL } from "@/content/site";
import type { SearchEntry } from "@/lib/search-types";
import { rank, type Ranked } from "@/lib/search-rank";
import { setThemeChoice } from "./theme-toggle";

type Item = Ranked & { run: () => void };
let indexPromise: Promise<SearchEntry[]> | null = null;
const loadIndex = () =>
  (indexPromise ??= fetch("/search-index.json")
    .then((r) => (r.ok ? r.json() : []))
    .catch(() => {
      indexPromise = null;
      return [];
    }));

export function CommandMenu({ onClose, returnFocus }: { onClose: () => void; returnFocus: HTMLElement | null }) {
  const router = useRouter();
  const dialog = useRef<HTMLDialogElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState<SearchEntry[] | null>(null);
  const [active, setActive] = useState<string | null>(null);
  const ids = useId();

  useEffect(() => {
    const d = dialog.current;
    d?.showModal();
    loadIndex().then(setIndex);
    return () => {
      d?.close();
      returnFocus?.focus?.();
    };
  }, [returnFocus]);

  const actions = useMemo<SearchEntry[]>(
    () => [
      { kind: "action", group: "Go to", title: "Go to Home", url: "/", text: "homepage start" },
      { kind: "action", group: "Go to", title: "Go to Docs", url: "/docs", text: "documentation guides" },
      { kind: "action", group: "Go to", title: "Go to Pricing", url: "/pricing", text: "price plans beta cost free" },
      { kind: "action", group: "Go to", title: "Open GitHub", url: GITHUB_URL, text: "source code repository nats-hub" },
      { kind: "action", group: "Actions", title: "Request an Invite", url: INVITE_REQUEST_URL, text: "beta waitlist sign up hosted" },
      { kind: "action", group: "Actions", title: "Use Light Theme", url: "theme:light", text: "theme appearance mode", searchOnly: true },
      { kind: "action", group: "Actions", title: "Use Dark Theme", url: "theme:dark", text: "theme appearance mode night", searchOnly: true },
      { kind: "action", group: "Actions", title: "Use System Theme", url: "theme:system", text: "theme appearance mode auto", searchOnly: true },
    ],
    [],
  );

  const items: Item[] = useMemo(() => {
    const go = (url: string) => () => {
      onClose();
      if (url.startsWith("theme:")) setThemeChoice(url.slice(6) as "light" | "dark" | "system");
      else if (url.startsWith("http") || url.startsWith("mailto:")) window.location.href = url;
      else router.push(url);
    };
    const pool = [...actions, ...(index ?? [])];
    return rank(pool, query).map((r) => ({ ...r, run: go(r.entry.url) }));
  }, [actions, index, query, router, onClose]);

  const groups = useMemo(() => {
    const out: { name: string; items: Item[] }[] = [];
    for (const it of items) {
      const g = out.find((x) => x.name === it.entry.group);
      if (g) g.items.push(it);
      else out.push({ name: it.entry.group, items: [it] });
    }
    return out;
  }, [items]);

  // Selection is tracked by stable key, reset to the first result on every keystroke.
  const keyOf = (it: Item) => `${it.entry.url}|${it.entry.title}`;
  const activeKey = active && items.some((i) => keyOf(i) === active) ? active : items[0] ? keyOf(items[0]) : null;
  const optionId = (k: string) => `${ids}-o-${encodeURIComponent(k).replace(/%/g, "")}`;

  const move = (delta: number | "first" | "last") => {
    if (!items.length) return;
    const i = items.findIndex((x) => keyOf(x) === activeKey);
    const next =
      delta === "first" ? 0 : delta === "last" ? items.length - 1 : (i + delta + items.length) % items.length;
    const k = keyOf(items[next]);
    setActive(k);
    requestAnimationFrame(() =>
      document.getElementById(optionId(k))?.scrollIntoView({ block: "nearest" }),
    );
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
    const ctrl = e.ctrlKey && !e.metaKey;
    if (e.key === "ArrowDown" || (ctrl && (e.key === "n" || e.key === "j"))) {
      e.preventDefault();
      move(e.metaKey ? "last" : 1);
    } else if (e.key === "ArrowUp" || (ctrl && (e.key === "p" || e.key === "k"))) {
      e.preventDefault();
      move(e.metaKey ? "first" : -1);
    } else if (e.key === "Home" && !query) {
      e.preventDefault();
      move("first");
    } else if (e.key === "End" && !query) {
      e.preventDefault();
      move("last");
    } else if (e.key === "Enter") {
      e.preventDefault();
      items.find((x) => keyOf(x) === activeKey)?.run();
    }
  };

  return (
    <dialog
      ref={dialog}
      className="cmdk"
      aria-label="Search docs and pages"
      onCancel={(e) => {
        e.preventDefault();
        onClose();
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="cmdk-panel" onKeyDown={onKeyDown}>
        <label htmlFor={`${ids}-input`} className="sr-only">
          Search docs and pages
        </label>
        <div className="cmdk-input-row">
          <svg width="16" height="16" viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="11" cy="11" r="6.5" fill="none" stroke="currentColor" strokeWidth="1.6" />
            <path d="m16 16 4.5 4.5" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
          </svg>
          <input
            id={`${ids}-input`}
            autoFocus
            role="combobox"
            aria-expanded="true"
            aria-controls={`${ids}-list`}
            aria-activedescendant={activeKey ? optionId(activeKey) : undefined}
            aria-autocomplete="list"
            autoComplete="off"
            autoCorrect="off"
            spellCheck={false}
            placeholder="Search the docs, pages and commands…"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setActive(null);
              list.current?.scrollTo({ top: 0 });
            }}
          />
          <kbd className="kbd">Esc</kbd>
        </div>
        <div id={`${ids}-list`} ref={list} role="listbox" aria-label="Results" className="cmdk-list">
          {groups.map((g) => (
            <div key={g.name} role="presentation" className="cmdk-group">
              <div id={`${ids}-g-${g.name}`} className="cmdk-group-heading" aria-hidden="true">
                {g.name}
              </div>
              <div role="group" aria-labelledby={`${ids}-g-${g.name}`}>
                {g.items.map((it) => {
                  const k = keyOf(it);
                  return (
                    <div
                      key={k}
                      id={optionId(k)}
                      role="option"
                      aria-selected={k === activeKey}
                      className="cmdk-item"
                      onPointerMove={() => k !== activeKey && setActive(k)}
                      onClick={it.run}
                    >
                      <span className="cmdk-item-title">
                        {it.entry.heading ? <span className="cmdk-item-parent">{it.entry.title} › </span> : null}
                        {it.entry.heading ?? it.entry.title}
                      </span>
                      {it.snippet ? <span className="cmdk-item-snippet">{it.snippet}</span> : null}
                    </div>
                  );
                })}
              </div>
            </div>
          ))}
          {query && !items.length ? (
            <p className="cmdk-empty">
              {index === null ? "Loading the docs index…" : <>No results for “{query}”. Try a command name, such as hub-delegate.</>}
            </p>
          ) : null}
        </div>
        <div className="sr-only" aria-live="polite">
          {query ? `${items.length} results` : ""}
        </div>
        <div className="cmdk-foot" aria-hidden="true">
          <span><kbd className="kbd">↑</kbd> <kbd className="kbd">↓</kbd> to move</span>
          <span><kbd className="kbd">↵</kbd> to open</span>
        </div>
      </div>
    </dialog>
  );
}
