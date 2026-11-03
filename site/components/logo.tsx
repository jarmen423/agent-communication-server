// The ACS mark, drawn from brand/logo/mark-{light,dark}.svg. The fills are the
// brand colour variables, so the light theme renders mark-light exactly and the
// dark theme renders mark-dark exactly. Never recolour, reorder or rotate it.

const SENDER = "M16 8H28A10 10 0 0 1 38 18V28A10 10 0 0 1 28 38H6V18A10 10 0 0 1 16 8Z";
const REPLY = "M36 26H48A10 10 0 0 1 58 36V56H36A10 10 0 0 1 26 46V36A10 10 0 0 1 36 26Z";

export function Mark({ id, size = 32, className }: { id: string; size?: number; className?: string }) {
  const clip = `acs-mark-${id}`;
  return (
    <svg
      viewBox="0 0 64 64"
      width={size}
      height={size}
      aria-hidden="true"
      focusable="false"
      className={className}
    >
      <defs>
        <clipPath id={clip}>
          <path d={SENDER} />
        </clipPath>
      </defs>
      <path fill="var(--relay-teal)" d={SENDER} />
      <path fill="var(--signal-orange)" d={REPLY} />
      <path fill="var(--ink)" clipPath={`url(#${clip})`} d={REPLY} />
    </svg>
  );
}

/**
 * Horizontal lockup: the mark at about 1.6x the wordmark's cap height, then the
 * full name in the wordmark style, gap space-2. Below `sm` only the mark shows
 * (minimum 16px alone), with the full name kept as the accessible label.
 */
export function Lockup({ id, compact = false }: { id: string; compact?: boolean }) {
  return (
    <span className="inline-flex items-center gap-2">
      <Mark id={id} size={34} />
      <span className={`type-wordmark whitespace-nowrap text-ink ${compact ? "sr-only lg:not-sr-only" : ""}`}>
        Agent Communication Server
      </span>
    </span>
  );
}
