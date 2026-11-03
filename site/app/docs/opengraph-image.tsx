import { OG_SIZE, ogImage } from "@/lib/og";

export const alt = "Agent Communication Server docs";
export const size = OG_SIZE;
export const dynamic = "force-static";
export const contentType = "image/png";

export default function Image() {
  return ogImage({ eyebrow: "docs", title: "Agent Communication Server docs" });
}
