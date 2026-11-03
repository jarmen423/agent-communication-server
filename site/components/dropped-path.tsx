"use client";

import { usePathname } from "next/navigation";

/** A log line for the 404 page, in the router's own voice. */
export function DroppedPath() {
  const path = usePathname();
  return (
    <>
      <span className="dir-recv">dropped</span> no route for {path || "this address"}
      {"\n"}
      <span className="text-lane-muted">0 subscribers · try ⌘K to search the docs</span>
    </>
  );
}
