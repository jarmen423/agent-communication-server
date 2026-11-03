"use client";

import { useSyncExternalStore } from "react";

export type ThemeChoice = "system" | "light" | "dark";
const KEY = "acs-theme";
const listeners = new Set<() => void>();

function readChoice(): ThemeChoice {
  try {
    const v = localStorage.getItem(KEY);
    return v === "light" || v === "dark" ? v : "system";
  } catch {
    return "system";
  }
}

export function setThemeChoice(choice: ThemeChoice) {
  try {
    if (choice === "system") localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, choice);
  } catch {
    /* private mode: the choice lasts for this page only */
  }
  const root = document.documentElement;
  if (choice === "system") delete root.dataset.theme;
  else root.dataset.theme = choice;
  listeners.forEach((l) => l());
}

function subscribe(cb: () => void) {
  listeners.add(cb);
  const mq = matchMedia("(prefers-color-scheme: dark)");
  mq.addEventListener("change", cb);
  return () => {
    listeners.delete(cb);
    mq.removeEventListener("change", cb);
  };
}

/** The stored choice ("system" when none), or null during SSR. */
export function useThemeChoice(): ThemeChoice | null {
  return useSyncExternalStore(subscribe, readChoice, () => null);
}

function resolvedDark(): boolean {
  const choice = readChoice();
  return choice === "dark" || (choice === "system" && matchMedia("(prefers-color-scheme: dark)").matches);
}

/** Header toggle: flips between light and dark, starting from what is on screen. */
export function ThemeToggle() {
  const dark = useSyncExternalStore(subscribe, resolvedDark, () => null);
  return (
    <button
      type="button"
      className="icon-btn"
      aria-label="Dark theme"
      aria-pressed={dark === null ? undefined : dark}
      title="Toggle dark theme"
      onClick={() => setThemeChoice(resolvedDark() ? "light" : "dark")}
    >
      {/* Both glyphs render; CSS shows the one for the current theme, so SSR never mismatches. */}
      <svg className="theme-icon-moon" width="18" height="18" viewBox="0 0 24 24" aria-hidden="true">
        <path
          d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5Z"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.6"
          strokeLinejoin="round"
        />
      </svg>
      <svg className="theme-icon-sun" width="18" height="18" viewBox="0 0 24 24" aria-hidden="true">
        <circle cx="12" cy="12" r="4" fill="none" stroke="currentColor" strokeWidth="1.6" />
        <path
          d="M12 2.5v2M12 19.5v2M4.6 4.6l1.4 1.4M18 18l1.4 1.4M2.5 12h2M19.5 12h2M4.6 19.4 6 18M18 6l1.4-1.4"
          stroke="currentColor"
          strokeWidth="1.6"
          strokeLinecap="round"
        />
      </svg>
    </button>
  );
}
