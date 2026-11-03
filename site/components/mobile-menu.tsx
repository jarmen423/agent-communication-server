"use client";

import Link from "next/link";
import { useRef } from "react";
import { INVITE_REQUEST_URL, NAV, SIGN_IN_URL } from "@/content/site";

/** Below md the nav lives in a modal sheet: native <dialog> gives focus trapping and Escape. */
export function MobileMenu() {
  const ref = useRef<HTMLDialogElement>(null);
  const close = () => ref.current?.close();

  return (
    <>
      <button
        type="button"
        className="icon-btn md:hidden"
        aria-label="Open menu"
        aria-haspopup="dialog"
        onClick={() => ref.current?.showModal()}
      >
        <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M4 8h16M4 16h16" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
        </svg>
      </button>
      <dialog
        ref={ref}
        className="sheet"
        aria-label="Menu"
        onClick={(e) => {
          if (e.target === e.currentTarget) close();
        }}
      >
        <div className="sheet-panel">
          <div className="flex items-center justify-between">
            <span className="eyebrow">Menu</span>
            <button type="button" className="icon-btn" aria-label="Close menu" onClick={close}>
              <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true">
                <path d="M6 6l12 12M18 6 6 18" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
              </svg>
            </button>
          </div>
          <nav aria-label="Mobile" className="mt-4 flex flex-col">
            <Link href="/" className="sheet-link" onClick={close}>
              Home
            </Link>
            {NAV.map((item) => (
              <Link key={item.href} href={item.href} className="sheet-link" onClick={close}>
                {item.label}
              </Link>
            ))}
            <a href={SIGN_IN_URL} className="sheet-link">
              Sign in
            </a>
          </nav>
          <a href={INVITE_REQUEST_URL} className="btn btn-primary mt-6 w-full">
            Request an invite
          </a>
        </div>
      </dialog>
    </>
  );
}
