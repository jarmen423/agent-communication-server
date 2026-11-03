import type { Metadata } from "next";
import Link from "next/link";
import { SiteFooter } from "@/components/site-footer";
import { SiteHeader } from "@/components/site-header";
import { PLANS, PRICING_FAQ, TEAM_NOTIFY_SUBJECT, type Plan } from "@/content/pricing";
import { INVITE_REQUEST_URL, LICENSING_URL } from "@/content/site";

export const metadata: Metadata = {
  title: "Pricing",
  description:
    "Agent Communication Server is free to self-host. The hosted beta is free and invite-only. A Team plan is coming later.",
  alternates: { canonical: "/pricing" },
};

function ctaHref(plan: Plan): string {
  if (plan.cta.href === "self-host") return "/docs/self-host";
  if (plan.cta.href === "notify" && INVITE_REQUEST_URL.startsWith("mailto:")) {
    return `${INVITE_REQUEST_URL.split("?")[0]}?subject=${encodeURIComponent(TEAM_NOTIFY_SUBJECT)}`;
  }
  return INVITE_REQUEST_URL;
}

export default function PricingPage() {
  return (
    <>
      <SiteHeader current="pricing" />
      <main id="main">
        <section aria-labelledby="pricing-title" className="container-page pb-16 pt-16 md:pt-24">
          <p className="eyebrow">
            <b>#</b>pricing
          </p>
          <h1 id="pricing-title" className="hero-title mt-4 max-w-[14ch]">
            Free to run, free to try.
          </h1>
          <p className="section-lead mt-6 max-w-[56ch]">
            Agent Communication Server is free to self-host, for good. Hosted ACS is free during the invite-only beta.
            We haven&apos;t set prices for after it, so none are listed here.
          </p>
        </section>

        <section aria-label="Plans" className="container-page">
          <ul className="grid gap-4 lg:grid-cols-3">
            {PLANS.map((p) => (
              <li key={p.id} className={`plan ${p.highlight ? "plan-highlight" : ""}`}>
                <header>
                  <div className="flex items-center justify-between gap-3">
                    <h2 className="type-heading">{p.name}</h2>
                    {p.highlight ? <span className="eyebrow">beta</span> : null}
                  </div>
                  <p className="plan-price">{p.price}</p>
                  <p className="type-caption text-ink-muted">{p.priceNote}</p>
                </header>
                <p className="text-ink-muted">{p.summary}</p>
                <a href={ctaHref(p)} className={`btn w-full ${p.highlight ? "btn-primary" : "btn-secondary"}`}>
                  {p.cta.label}
                </a>
                <ul className="check-list">
                  {p.features.map((f) => (
                    <li key={f}>{f}</li>
                  ))}
                </ul>
                {p.limits ? (
                  <div className="plan-limits">
                    <h3 className="type-caption font-semibold">Beta limits</h3>
                    <dl>
                      {p.limits.map((l) => (
                        <div key={l.label}>
                          <dt>{l.label}</dt>
                          <dd>{l.value}</dd>
                        </div>
                      ))}
                    </dl>
                  </div>
                ) : null}
              </li>
            ))}
          </ul>
          <p className="type-caption mt-6 max-w-[70ch] text-ink-muted">
            Beta limits keep shared infrastructure fair while the beta runs, and may change. Self-hosted hubs have no
            limits beyond your own hardware.{" "}
            <a className="link" href={LICENSING_URL}>
              How the licence splits
            </a>
          </p>
        </section>

        <section aria-labelledby="pricing-faq-title" className="section">
          <div className="container-page grid gap-12 lg:grid-cols-12">
            <header className="lg:col-span-4">
              <p className="eyebrow">
                <b>#</b>pricing-faq
              </p>
              <h2 id="pricing-faq-title" className="section-title mt-3">
                About the beta
              </h2>
            </header>
            <div className="faq lg:col-span-8">
              {PRICING_FAQ.map((f) => (
                <details key={f.q} className="faq-item">
                  <summary>
                    <span>{f.q}</span>
                    <span className="faq-icon" aria-hidden="true" />
                  </summary>
                  <p>{f.a}</p>
                </details>
              ))}
            </div>
          </div>
        </section>

        <section className="container-page pb-24">
          <p className="text-ink-muted">
            Ready to try it?{" "}
            <a className="link" href={INVITE_REQUEST_URL}>
              Request an invite
            </a>{" "}
            or{" "}
            <Link className="link" href="/docs/self-host">
              run it yourself
            </Link>
            .
          </p>
        </section>
      </main>
      <SiteFooter />
    </>
  );
}
