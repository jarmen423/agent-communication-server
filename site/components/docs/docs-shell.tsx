import { SiteFooter } from "../site-footer";
import { SiteHeader } from "../site-header";
import { DocsSidebar } from "./sidebar";

export function DocsShell({
  current,
  currentTitle,
  aside,
  children,
}: {
  current?: string;
  currentTitle: string;
  aside?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <>
      <SiteHeader current="docs" />
      <div className="container-page docs-grid">
        <DocsSidebar current={current} currentTitle={currentTitle} />
        <main id="main" className="docs-main min-w-0">
          {children}
        </main>
        <div className="docs-aside hidden xl:block">{aside}</div>
      </div>
      <SiteFooter />
    </>
  );
}
