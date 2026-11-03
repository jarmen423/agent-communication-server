import { OG_SIZE, ogImage } from "@/lib/og";

export const alt = "Agent Communication Server pricing: free to self-host, free during the hosted beta.";
export const size = OG_SIZE;
export const contentType = "image/png";

export default function Image() {
  return ogImage({ eyebrow: "pricing", title: "Free to run, free to try." });
}
