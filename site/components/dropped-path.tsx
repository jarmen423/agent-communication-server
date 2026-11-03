"use client";

import { useEffect, useState } from "react";

/** A log line for the 404 page, in the router's own voice. The path fills in after hydration. */
export function DroppedPath() {
  const [path, setPath] = useState<string | null>(null);
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- the static 404 page can't know the URL at build time
    setPath(location.pathname);
  }, []);
  return (
    <>
      <span className="dir-recv">dropped</span> no route for {path ?? "this address"}
      {"\n"}
      <span className="text-lane-muted">0 subscribers · try ⌘K to search the docs</span>
    </>
  );
}
