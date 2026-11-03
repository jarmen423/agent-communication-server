import Link from "next/link";
import { INVITE_REQUEST_URL, NAV, SIGN_IN_URL } from "@/content/site";
import { Lockup } from "./logo";
import { MobileMenu } from "./mobile-menu";
import { SearchButton } from "./search-button";
import { ThemeToggle } from "./theme-toggle";

export function SiteHeader({ current }: { current?: "docs" | "pricing" | "home" }) {
  return (
    <header className="site-header sticky top-0 z-40">
      <div className="container-page flex h-16 items-center gap-4">
        <Link href="/" className="-m-2 rounded-md p-2" aria-label="Agent Communication Server home">
          <Lockup id="nav" compact />
        </Link>

        <nav aria-label="Main" className="ml-2 hidden items-center gap-1 md:flex">
          {NAV.map((item) => {
            const active = current && item.href === `/${current}`;
            const external = item.href.startsWith("http");
            return (
              <Link
                key={item.href}
                href={item.href}
                aria-current={active ? "page" : undefined}
                className="nav-link"
                {...(external ? { rel: "noopener" } : {})}
              >
                {item.label}
              </Link>
            );
          })}
        </nav>

        <div className="ml-auto flex items-center gap-2">
          <SearchButton />
          <ThemeToggle />
          <a href={SIGN_IN_URL} className="nav-link hidden md:inline-flex">
            Sign in
          </a>
          <a href={INVITE_REQUEST_URL} className="btn btn-primary btn-sm hidden sm:inline-flex">
            Request an invite
          </a>
          <MobileMenu />
        </div>
      </div>
    </header>
  );
}
