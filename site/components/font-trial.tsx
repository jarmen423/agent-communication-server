"use client";

import { useEffect, useState } from "react";

type Face = "geist" | "bricolage";

function applyFace(f: Face) {
  const root = document.documentElement;
  if (f === "bricolage") root.dataset.display = "bricolage";
  else delete root.dataset.display;
  try {
    localStorage.setItem("acs-display", f);
  } catch {}
}

/** TEMPORARY: a display-font switcher, shown only with ?fonts in the URL. Delete after the pick. */
export function FontTrial() {
  const [face, setFace] = useState<Face | null>(null);
  useEffect(() => {
    if (!new URLSearchParams(location.search).has("fonts")) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- only known on the client
    setFace(document.documentElement.dataset.display === "bricolage" ? "bricolage" : "geist");
  }, []);
  if (!face) return null;
  const pick = (f: Face) => {
    setFace(f);
    applyFace(f);
  };
  return (
    <fieldset className="font-trial">
      <legend className="sr-only">Headline font (temporary comparison)</legend>
      <span aria-hidden="true">Headlines</span>
      <div className="segmented">
        {(["geist", "bricolage"] as const).map((f) => (
          <label key={f} className="segmented-option">
            <input type="radio" name="acs-display" checked={face === f} onChange={() => pick(f)} />
            <span>{f === "geist" ? "Geist Serif" : "Bricolage"}</span>
          </label>
        ))}
      </div>
    </fieldset>
  );
}
