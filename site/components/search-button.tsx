"use client";

import { useCallback, useEffect, useState, type ComponentType } from "react";

type MenuProps = { onClose: () => void; returnFocus: HTMLElement | null };

/**
 * The ⌘K trigger. The menu itself (and the docs index it searches) loads on
 * first use, so neither is in the page's initial JavaScript.
 */
export function SearchButton() {
  const [Menu, setMenu] = useState<ComponentType<MenuProps> | null>(null);
  const [open, setOpen] = useState(false);
  const [opener, setOpener] = useState<HTMLElement | null>(null);
  const [mac, setMac] = useState(true);

  const show = useCallback(async () => {
    setOpener(document.activeElement as HTMLElement | null);
    if (!Menu) {
      const mod = await import("./command-menu");
      setMenu(() => mod.CommandMenu);
    }
    setOpen(true);
  }, [Menu]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- platform is only known on the client
    setMac(/Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent));
    const onKey = (e: KeyboardEvent) => {
      if (e.isComposing || e.keyCode === 229) return;
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        if (open) setOpen(false);
        else void show();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open, show]);

  return (
    <>
      <button
        type="button"
        onClick={() => void show()}
        className="search-trigger"
        aria-label="Search docs"
        aria-keyshortcuts={mac ? "Meta+K" : "Control+K"}
        onPointerEnter={() => void import("./command-menu")}
      >
        <svg width="16" height="16" viewBox="0 0 24 24" aria-hidden="true">
          <circle cx="11" cy="11" r="6.5" fill="none" stroke="currentColor" strokeWidth="1.6" />
          <path d="m16 16 4.5 4.5" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
        </svg>
        <span className="hidden lg:inline">Search docs</span>
        <span className="kbd hidden lg:inline-flex" aria-hidden="true">
          {mac ? "⌘ K" : "Ctrl K"}
        </span>
      </button>
      {open && Menu ? <Menu onClose={() => setOpen(false)} returnFocus={opener} /> : null}
    </>
  );
}
