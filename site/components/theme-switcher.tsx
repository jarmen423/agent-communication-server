"use client";

import { setThemeChoice, useThemeChoice, type ThemeChoice } from "./theme-toggle";

const OPTIONS: { value: ThemeChoice; label: string }[] = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

/** Footer segmented control: a radio group, so arrow keys move between choices. */
export function ThemeSwitcher() {
  const choice = useThemeChoice();
  return (
    <fieldset className="segmented w-fit">
      <legend className="sr-only">Theme</legend>
      {OPTIONS.map((o) => (
        <label key={o.value} className="segmented-option">
          <input
            type="radio"
            name="acs-theme"
            value={o.value}
            checked={choice === o.value}
            onChange={() => setThemeChoice(o.value)}
            disabled={choice === null}
          />
          <span>{o.label}</span>
        </label>
      ))}
    </fieldset>
  );
}
