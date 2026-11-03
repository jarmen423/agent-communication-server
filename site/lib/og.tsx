// Open Graph images, rendered once at build time with next/og.
import { ImageResponse } from "next/og";

export const OG_SIZE = { width: 1200, height: 630 };

// Light-theme brand values (brand/tokens.json); satori can't read CSS variables.
const C = {
  paper: "#f5f6f2",
  ink: "#171a21",
  muted: "#5a6068",
  line: "#d9dcd4",
  lane: "#171a21",
  laneLine: "#2b2e36",
  laneInk: "#eef0ea",
  laneMuted: "#a3a9b0",
  teal: "#0f7c74",
  orange: "#d9731a",
  laneTeal: "#3cb8ab",
  laneOrange: "#f5a742",
};

/** Fetches a TTF subset from Google Fonts (the same family next/font self-hosts). */
async function font(family: string, weight: number, text: string): Promise<ArrayBuffer | null> {
  try {
    const css = await fetch(
      `https://fonts.googleapis.com/css2?family=${family.replace(/ /g, "+")}:wght@${weight}&text=${encodeURIComponent(text)}`,
    ).then((r) => r.text());
    const url = css.match(/src: url\((.+?)\) format\('(?:opentype|truetype)'\)/)?.[1];
    return url ? await fetch(url).then((r) => r.arrayBuffer()) : null;
  } catch {
    return null;
  }
}

function Mark({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 64 64">
      <path fill={C.teal} d="M16 8H28A10 10 0 0 1 38 18V28A10 10 0 0 1 28 38H6V18A10 10 0 0 1 16 8Z" />
      <path fill={C.orange} d="M36 26H48A10 10 0 0 1 58 36V56H36A10 10 0 0 1 26 46V36A10 10 0 0 1 36 26Z" />
      {/* The lens: the reply bubble clipped to the sender bubble, drawn directly (satori has no clip-path). */}
      <path fill={C.ink} d="M36 26H38V28A10 10 0 0 1 28 38H26V36A10 10 0 0 1 36 26Z" />
    </svg>
  );
}

const tile = (x: number, y: number, sharpLeft: boolean) => {
  const w = 30, h = 22, r = 4;
  return sharpLeft
    ? `M${x + r} ${y}H${x + w - r}A${r} ${r} 0 0 1 ${x + w} ${y + r}V${y + h - r}A${r} ${r} 0 0 1 ${x + w - r} ${y + h}H${x}V${y + r}A${r} ${r} 0 0 1 ${x + r} ${y}Z`
    : `M${x + r} ${y}H${x + w - r}A${r} ${r} 0 0 1 ${x + w} ${y + r}V${y + h}H${x + r}A${r} ${r} 0 0 1 ${x} ${y + h - r}V${y + r}A${r} ${r} 0 0 1 ${x + r} ${y}Z`;
};

export async function ogImage({ eyebrow, title }: { eyebrow: string; title: string }) {
  const wordmark = "Agent Communication Server";
  const [display, wm, mono] = await Promise.all([
    font("Bricolage Grotesque", 700, title),
    font("Bricolage Grotesque", 600, wordmark),
    font("IBM Plex Mono", 500, `${eyebrow} sent → ← received acs.agentmemorylabs.com#`),
  ]);
  const fonts = [
    display && { name: "Display", data: display, weight: 700 as const },
    wm && { name: "Wordmark", data: wm, weight: 600 as const },
    mono && { name: "Mono", data: mono, weight: 500 as const },
  ].filter(Boolean) as { name: string; data: ArrayBuffer; weight: 500 | 600 | 700 }[];

  return new ImageResponse(
    (
      <div style={{ width: "100%", height: "100%", display: "flex", flexDirection: "column", background: C.paper, padding: "56px 64px 0" }}>
        <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
          <Mark size={52} />
          <span style={{ fontFamily: "Wordmark", fontSize: 30, fontWeight: 600, color: C.ink, letterSpacing: "-0.01em" }}>{wordmark}</span>
        </div>
        <span style={{ marginTop: 64, fontFamily: "Mono", fontSize: 24, color: C.muted }}>
          <span style={{ color: C.teal }}>#</span>
          {eyebrow}
        </span>
        <span style={{ marginTop: 12, fontFamily: "Display", fontWeight: 700, fontSize: title.length > 34 ? 64 : 78, lineHeight: 0.98, letterSpacing: "-0.02em", color: C.ink, maxWidth: 1000 }}>
          {title}
        </span>
        <div style={{ display: "flex", marginTop: "auto", marginLeft: -64, marginRight: -64, height: 120, background: C.lane, position: "relative" }}>
          <span style={{ position: "absolute", left: 64, top: 8, fontFamily: "Mono", fontSize: 18, color: C.laneTeal }}>sent →</span>
          <span style={{ position: "absolute", right: 64, bottom: 6, fontFamily: "Mono", fontSize: 18, color: C.laneOrange }}>← received</span>
          <svg width="1200" height="120" viewBox="0 0 1200 120">
            <path d="M64 46H1136" stroke={C.laneTeal} strokeWidth="3" />
            <path d="M64 74H1136" stroke={C.laneOrange} strokeWidth="3" />
            {[220, 460, 800].map((x) => (
              <path key={x} d={tile(x, 35, true)} fill={C.laneTeal} />
            ))}
            {[140, 520, 880].map((x) => (
              <path key={x} d={tile(x, 63, false)} fill={C.laneOrange} />
            ))}
          </svg>
        </div>
      </div>
    ),
    { ...OG_SIZE, fonts: fonts.length ? fonts : undefined },
  );
}
