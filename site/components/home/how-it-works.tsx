import { STEPS } from "@/content/home";
import { CommandBlock } from "../command-block";
import { SectionHeader } from "../section-header";

export function HowItWorks() {
  return (
    <section id="how-it-works" aria-labelledby="how-title" className="section">
      <div className="container-page">
        <SectionHeader
          id="how-title"
          channel="how-it-works"
          title="From sign-in to a delegated task."
          rest="Sign in once, connect each machine, then delegate from the agent you already use."
        />
        <ol className="mt-12 border-b border-line">
          {STEPS.map((s) => (
            <li key={s.n} className="grid gap-6 border-t border-line py-8 lg:grid-cols-12 lg:gap-10 lg:py-10">
              <div className="lg:col-span-5">
                <div className="flex items-baseline gap-3">
                  <span className="step-n" aria-hidden="true">
                    0{s.n}
                  </span>
                  <h3 className="type-heading">
                    <span className="sr-only">Step {s.n}: </span>
                    {s.title}
                  </h3>
                </div>
                <p className="mt-3 max-w-[44ch] text-ink-muted">{s.body}</p>
              </div>
              <div className="min-w-0 lg:col-span-7">
                <CommandBlock
                  lines={s.code}
                  shell={!("tools" in s)}
                  title={"tools" in s ? "MCP tools, called by your agent" : undefined}
                />
                <p className="type-caption mt-3 text-ink-muted">{s.note}</p>
              </div>
            </li>
          ))}
        </ol>
      </div>
    </section>
  );
}
