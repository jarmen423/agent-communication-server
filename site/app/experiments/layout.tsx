import type { Metadata } from "next";

// Hero bake-off pages: scratch, never indexed, deleted once a concept is picked.
export const metadata: Metadata = { robots: { index: false, follow: false } };

export default function ExperimentsLayout({ children }: { children: React.ReactNode }) {
  return children;
}
