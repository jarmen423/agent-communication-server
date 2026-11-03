import Link from "next/link";
import { AGENTS, BETA, SECURITY, SECURITY_LIMITS, WEB_CHAT } from "@/content/home";
import { SectionHeader } from "../section-header";

export function WorksWith() {
  return (
    <section id="works-with" aria-labelledby="works-title" className="section">
      <div className="container-page grid gap-12 lg:grid-cols-12">
        <SectionHeader
          id="works-title"
          channel="works-with"
          title="Works with the agents you already run."
          rest="Each worker backend wraps the agent's own CLI or SDK. Nothing to fork."
          className="lg:col-span-4"
          lead={
            <Link href="/docs/worker-backends" className="link">
              How the worker backends work
            </Link>
          }
        />
        <div className="lg:col-span-8">
          <table className="agents-table">
            <caption className="sr-only">Supported agents, what each can act as, and how it connects</caption>
            <thead>
              <tr>
                <th scope="col">Agent</th>
                <th scope="col">Acts as</th>
                <th scope="col">Connects through</th>
              </tr>
            </thead>
            <tbody>
              {AGENTS.map((a) => (
                <tr key={a.name}>
                  <th scope="row">{a.name}</th>
                  <td>{a.as}</td>
                  <td>
                    <code>{a.via}</code>
                  </td>
                </tr>
              ))}
              <tr className="agents-web">
                <th scope="row">
                  {WEB_CHAT.name}
                  <span className="tag">early</span>
                </th>
                <td>{WEB_CHAT.as}</td>
                <td>
                  <code>{WEB_CHAT.via}</code>
                  <span className="mt-1 block text-ink-muted">{WEB_CHAT.note}</span>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </section>
  );
}

export function Beta() {
  return (
    <section id="beta" aria-labelledby="beta-title" className="section">
      <div className="container-page">
        <SectionHeader
          id="beta-title"
          channel="beta"
          title="What the hosted beta includes."
          rest="We run the hub; you connect your machines. Here is what is in it today, and what isn't yet."
        />
        <div className="mt-14 grid gap-4 md:grid-cols-2">
          <article className="card card-accent flex flex-col p-6 md:p-8">
            <header className="flex flex-wrap items-baseline justify-between gap-2">
              <h3 className="type-display text-[32px] leading-[36px]">In the beta</h3>
              <span className="eyebrow">invite only, free</span>
            </header>
            <ul className="check-list mt-6">
              {BETA.included.map((p) => (
                <li key={p}>{p}</li>
              ))}
            </ul>
          </article>
          <article className="card flex flex-col p-6 md:p-8">
            <header className="flex flex-wrap items-baseline justify-between gap-2">
              <h3 className="type-display text-[32px] leading-[36px]">Not yet</h3>
              <span className="eyebrow">on the way</span>
            </header>
            <ul className="dash-list mt-6">
              {BETA.notYet.map((p) => (
                <li key={p}>{p}</li>
              ))}
            </ul>
          </article>
        </div>
        <p className="mt-6">
          <Link href="/docs/hosted" className="link">
            Hosted quick start
          </Link>
        </p>
      </div>
    </section>
  );
}

export function Security() {
  return (
    <section id="security" aria-labelledby="security-title" className="section">
      <div className="container-page">
        <SectionHeader
          id="security-title"
          channel="security"
          title="Your agents, isolated and accountable."
          rest="Every envelope carries a sender the server checked, inside an account no other tenant can see."
        />
        <ul className="mt-14 grid gap-px overflow-hidden rounded-[var(--radius-lg)] bg-line shadow-[0_0_0_1px_var(--line)] sm:grid-cols-2 lg:grid-cols-4">
          {SECURITY.map((s) => (
            <li key={s.title} className="flex flex-col gap-3 bg-paper p-6">
              <h3 className="type-heading">{s.title}</h3>
              <p className="text-ink-muted">{s.body}</p>
              <code className="type-mono mt-auto pt-2 text-ink-muted">{s.detail}</code>
            </li>
          ))}
        </ul>
        <p className="mt-6 max-w-[62ch] text-ink-muted">
          {SECURITY_LIMITS}{" "}
          <Link href="/docs/security#honest-threat-model--what-is-and-isnt-proved" className="link">
            Read the threat model
          </Link>
        </p>
      </div>
    </section>
  );
}
