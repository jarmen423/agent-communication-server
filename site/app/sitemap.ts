import type { MetadataRoute } from "next";
import { DOC_PAGES } from "@/content/docs-manifest";
import { SITE_URL } from "@/content/site";

export const dynamic = "force-static";

export default function sitemap(): MetadataRoute.Sitemap {
  const pages = ["", "/pricing", "/docs", ...DOC_PAGES.map((d) => `/docs/${d.slug}`)];
  return pages.map((p) => ({ url: `${SITE_URL}${p}`, changeFrequency: "weekly", priority: p === "" ? 1 : 0.7 }));
}
