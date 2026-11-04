"use client";

// On a page that opens on a dark hero, the header wears the dark tokens
// (data-tone="dark", from tokens.css) while the hero is under it, then follows
// the page theme. It is server-rendered dark, so the first paint matches; without
// JavaScript it simply stays dark.
import { useEffect } from "react";

export function HeaderTone() {
  useEffect(() => {
    const header = document.querySelector<HTMLElement>(".site-header");
    const hero = document.querySelector<HTMLElement>("[data-hero-stage]");
    if (!header || !hero) return;
    let frame = 0;
    const update = () => {
      frame = 0;
      if (hero.getBoundingClientRect().bottom > header.offsetHeight / 2) header.dataset.tone = "dark";
      else delete header.dataset.tone;
    };
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(update);
    };
    update();
    addEventListener("scroll", schedule, { passive: true });
    addEventListener("resize", schedule);
    return () => {
      removeEventListener("scroll", schedule);
      removeEventListener("resize", schedule);
      cancelAnimationFrame(frame);
    };
  }, []);
  return null;
}
