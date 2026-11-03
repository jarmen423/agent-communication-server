import Link from "next/link";
import { SiteFooter } from "@/components/site-footer";
import { SiteHeader } from "@/components/site-header";
import { DroppedPath } from "@/components/dropped-path";

export default function NotFound() {
  return (
    <>
      <SiteHeader />
      <main id="main" className="container-page flex min-h-[60vh] flex-col justify-center py-24">
        <p className="eyebrow">
          <b>#</b>404
        </p>
        <h1 className="hero-title mt-4 max-w-[16ch]">Nothing is subscribed to this page.</h1>
        <div className="cmd lane mt-8 max-w-[640px]">
          <div className="cmd-head">
            <span>hub-server</span>
          </div>
          <pre className="cmd-body">
            <code>
              <DroppedPath />
            </code>
          </pre>
        </div>
        <p className="mt-8 flex flex-wrap gap-3">
          <Link href="/" className="btn btn-primary">
            Go to the homepage
          </Link>
          <Link href="/docs" className="btn btn-secondary">
            Browse the docs
          </Link>
        </p>
      </main>
      <SiteFooter />
    </>
  );
}
