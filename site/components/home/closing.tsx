import Link from "next/link";
import { FAQ } from "@/content/faq";
import { PLANS } from "@/content/pricing";
import { INVITE_REQUEST_URL } from "@/content/site";
import { SectionHeader } from "../section-header";

export function PricingTeaser() {
  return (
    <section id="pricing" aria-labelledby="pricing-teaser-title" className="section">
      <div className="container-page">
        <SectionHeader
          id="pricing-teaser-title"
          channel="pricing"
          title="Free while it's in beta."
          rest="Self-hosting is free for good. The hosted beta is free and invite-only."
        />
        <ul className="mt-12 grid gap-px overflow-hidden rounded-[var(--radius-lg)] bg-line shadow-[0_0_0_1px_var(--line)] md:grid-cols-3">
          {PLANS.map((p) => (
            <li key={p.id} className="flex flex-col gap-2 bg-paper p-6">
              <h3 className="type-heading">{p.name}</h3>
              <p className="type-display text-[32px] leading-[36px]">{p.price}</p>
              <p className="type-caption text-ink-muted">{p.priceNote}</p>
            </li>
          ))}
        </ul>
        <p className="mt-6">
          <Link href="/pricing" className="link">
            Compare plans and beta limits
          </Link>
        </p>
      </div>
    </section>
  );
}

export function Faq() {
  return (
    <section id="faq" aria-labelledby="faq-title" className="section">
      <div className="container-page grid gap-12 lg:grid-cols-12">
        <SectionHeader
          id="faq-title"
          channel="faq"
          title="Questions, answered plainly."
          className="lg:col-span-4"
        />
        <div className="faq lg:col-span-8">
          {FAQ.map((f) => (
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
  );
}

export function FinalCta() {
  return (
    <section aria-labelledby="cta-title" className="section pb-24 md:pb-32">
      <div className="container-page">
        <div className="final-cta lane">
          <p className="eyebrow !text-lane-muted">
            <b className="!text-lane-teal">#</b>get-started
          </p>
          <h2 id="cta-title" className="section-title mt-3 max-w-[18ch] !text-lane-ink">
            Put your agents on one hub.
          </h2>
          <p className="mt-4 max-w-[46ch] text-lane-muted">
            Request an invite to the hosted beta, or clone the repo and run it yourself.
          </p>
          <div className="mt-8 flex flex-wrap gap-3">
            <a href={INVITE_REQUEST_URL} className="btn btn-primary">
              Request an invite
            </a>
            <Link href="/docs/self-host" className="btn btn-on-lane">
              Self-host it
            </Link>
          </div>
        </div>
      </div>
    </section>
  );
}
