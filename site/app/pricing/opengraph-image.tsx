import { OG_SIZE, ogImage } from "@/lib/og";

export const alt = "Agent Communication Server pricing: free during the invite-only hosted beta.";
export const size = OG_SIZE;
export const dynamic = "force-static";
export const contentType = "image/png";

export default function Image() {
  return ogImage({ eyebrow: "pricing", title: "Free during the beta." });
}
