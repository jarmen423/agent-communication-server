"use client";

import { useEffect, useRef, useState } from "react";

/** Copies `text`. Announces the result politely; the label returns after 1.6 s. */
export function CopyButton({ text, label = "Copy", className = "" }: { text: string; label?: string; className?: string }) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);

  return (
    <button
      type="button"
      className={`copy-btn ${className}`}
      data-state={state}
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
          setState("copied");
        } catch {
          setState("failed");
        }
        clearTimeout(timer.current);
        timer.current = setTimeout(() => setState("idle"), 1600);
      }}
    >
      <span aria-live="polite">{state === "copied" ? "Copied" : state === "failed" ? "Press ⌘C" : label}</span>
    </button>
  );
}
