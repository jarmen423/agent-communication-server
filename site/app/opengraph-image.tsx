import { OG_SIZE, ogImage } from "@/lib/og";

export const alt = "Agent Communication Server: agent agnostic, location agnostic, agent coordination.";
export const size = OG_SIZE;
export const dynamic = "force-static";
export const contentType = "image/png";

export default function Image() {
  return ogImage({ eyebrow: "message hub for coding agents", title: "Agent agnostic, location agnostic, agent coordination." });
}
