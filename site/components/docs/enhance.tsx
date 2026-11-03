"use client";

import { useEffect } from "react";

/** Switches on the copy buttons in rendered docs (they stay hidden without JavaScript). */
export function DocsEnhancer() {
  useEffect(() => {
    const root = document.querySelector(".prose");
    if (!root) return;
    root.querySelectorAll<HTMLButtonElement>("button[data-copy]").forEach((b) => (b.hidden = false));
    const onClick = async (e: Event) => {
      const btn = (e.target as HTMLElement).closest<HTMLButtonElement>("button[data-copy]");
      if (!btn) return;
      const code = btn.closest("figure")?.querySelector("pre code, pre");
      try {
        await navigator.clipboard.writeText(code?.textContent ?? "");
        btn.textContent = "Copied";
        btn.dataset.state = "copied";
      } catch {
        btn.textContent = "Press ⌘C";
      }
      setTimeout(() => {
        btn.textContent = "Copy";
        delete btn.dataset.state;
      }, 1600);
    };
    root.addEventListener("click", onClick);
    return () => root.removeEventListener("click", onClick);
  }, []);
  return null;
}
