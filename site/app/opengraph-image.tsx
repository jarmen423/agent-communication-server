import { OG_SIZE, ogImage } from "@/lib/og";

export const alt = "Agent Communication Server: many agents, many machines, one conversation.";
export const size = OG_SIZE;
export const contentType = "image/png";

export default function Image() {
  return ogImage({ eyebrow: "message hub for coding agents", title: "Many agents, many machines, one conversation." });
}
