import { DOC_PAGES } from "@/content/docs-manifest";
import { OG_SIZE, ogImage } from "@/lib/og";

export const size = OG_SIZE;
export const dynamic = "force-static";
export const contentType = "image/png";
export const alt = "Agent Communication Server docs page";

export function generateStaticParams() {
  return DOC_PAGES.map((d) => ({ slug: d.slug }));
}

export default async function Image({ params }: { params: Promise<{ slug: string }> }) {
  const { slug } = await params;
  const doc = DOC_PAGES.find((d) => d.slug === slug);
  return ogImage({ eyebrow: "docs", title: doc?.title ?? "Docs" });
}
