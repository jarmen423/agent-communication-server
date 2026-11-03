# Prompt: design and build the ACS homepage

Paste everything below the line into the design agent.

---

You are the design engineer for **ACS**, the hosted version of the
open-source **nats-hub** project. Design and build its public homepage,
with a documentation section and a pricing section, and GPU motion
graphics built with vgpu. Go well past a template: the site should look
as if a devtool company with taste made it.

## Repo and branch

- Repo: `/data/code/agent-communication-server`
  (`github.com/jarmen423/agent-communication-server`). Start from an
  up-to-date `main`.
- Work on a new branch, `iter2/homepage`, in its own worktree:
  `git worktree add /data/code/acs-wt/homepage -b iter2/homepage main`.
  Don't commit to `main`.
- Put the site in a new top-level `site/` directory. Don't edit anything
  outside `site/`, except `REUSE.toml`, which needs an entry for `site/**`
  (see "Constraints").
- Read `AGENTS.md` first. It covers the conventions, file-size limit and
  licensing.

## Skills and tools to use

Load all four skills before you design anything. Each one has its own
job.

1. **`frontend-design-skill-router`** (`~/.agents/skills/frontend-design-skill-router`).
   This is the main front-end router. Its rule is that every choice must
   be beautiful and fast at once. Match each part of the work to its
   routing question, then open the content skill it points to. The ones
   that apply here:
   - `high-end-visuals-3d-webgl`: the vgpu hero (see item 5);
   - `performance-architecture`;
   - `microinteractions-loading-transitions`;
   - `layout-composition`;
   - `navigation-wayfinding`: the nav and the docs sidebar;
   - `theming-personalization`;
   - `data-display-visualization`: if the hero or the features show
     live message flow.
2. **`design-mentor`** (`~/.agents/skills/design-mentor`). Use it for
   every conversation with me about design.
   - When you check in or report, explain each decision in plain words
     and name the principle behind it.
   - Show options side by side with one recommendation and its
     trade-off.
   - When I give vague feedback ("feels off"), diagnose it into a named
     principle before you change anything.
   - Check in with me once, after the static build and before the
     motion pass, and show the direction (palette, type, hero concept)
     this way.
3. **`bespoke`** (my design-engineering skill, a router with six
   sub-skills). Follow its workflow end to end:
   intent → references → static build → motion → sanding → perf/a11y →
   review. Load each sub-skill when the work reaches its area:
   - `devtool-visual-system`: tokens, the dark theme, type, and
     `references/marketing-anatomy.md` for the page structure. Use
     `scripts/theme.py` to generate the palette.
   - `bento-grids`: the features section.
   - `signature-effects`: one signature hero moment, not several.
   - `interface-motion`: motion budget and choreography.
   - `interface-guidelines`: focus, keyboard, forms, copy and a11y. Run
     its `scripts/scan.sh` on `site/` before you finish.
   - `command-menu`: ⌘K search for the docs.

   Note: bespoke's "In this repo" sections were written for a different
   site (agent-memory-labs-frontend). Ignore them here.
4. **`frontend-design`**: use it alongside the others for build quality
   and to stay clear of generic AI aesthetics.
5. **vgpu** (Vercel Labs' agent-first WebGPU library for shaders and GPU
   motion graphics: https://vgpu.sh, npm `vgpu`, MIT). Use it for the
   signature motion graphics, starting with the hero, plus at most one
   or two smaller GPU moments elsewhere. Use its agent tooling rather
   than guessing the API from memory:
   - **Skill:** `npx skills add vercel-labs/vgpu`. It's a thin docs
     router that queries the docs for the installed version.
   - **MCP:** `npx vgpu mcp --project-from-cwd` (local stdio, docs for
     the installed version), or the read-only endpoint
     `https://vgpu.sh/api/mcp`.
   - **CLI:** `npx vgpu docs find <topic>`,
     `npx vgpu docs cat getting-started.md`,
     `npx vgpu examples search "<idea>"`,
     `npx vgpu examples pull <id> --out <dir>`. Run `npx vgpu check` on
     every shader before you call it done.
   - **It is pre-1.0** (0.6.0 at the time of writing). Pin the exact
     version.
   - **WebGPU is not supported in every browser yet.** Every vgpu moment
     must be a progressive enhancement:
     - Server-render a static poster frame first (an image or CSS),
       with the same size, so nothing shifts.
     - Feature-detect `navigator.gpu`, and lazy-load vgpu only after
       first paint and only where WebGPU exists.
     - Pause rendering when it is off-screen or the tab is hidden.
     - Under `prefers-reduced-motion`, keep the poster frame.
     - The page must look finished with no GPU at all.
   - Test the shaders with vgpu's mock adapter in Node, so the site
     check runs without a GPU.

Stack: Next.js (App Router) + TypeScript + Tailwind. Plan for Vercel
hosting, but deploying is optional: do it only if a Vercel login already
works in your session (`npx vercel`), and then only as a preview. No
production deploy, and no custom domain.
Prefer static rendering for every page. Self-host fonts with
`next/font`. Don't use component kits that dictate the look, and no stock
illustrations.

## What the product is

Use only real facts. Read these files and quote or paraphrase them; don't
invent capabilities:

- `README.md`: the pitch, quick start, and section D (hosted).
- `docs/HOSTED_PLAN.md` §1 and §4: what hosted ACS is, and the `acs`
  connector.
- `docs/ACS_CONNECTOR.md`: `acs login`, `acs agent add`,
  `acs mcp install`, `acs worker run`.
- `docs/REMOTE_MCP.md`: the remote MCP endpoint for claude.ai, ChatGPT
  and Grok.
- `docs/WORKER_BACKENDS.md`: the agents that are supported.
- `docs/SECURITY.md`, `docs/DELIVERY.md`, `docs/WAVES.md`,
  `docs/FILE_LOCKS.md`, `docs/EMAIL_PLUGIN.md` (experimental; label it
  that way).
- `LICENSING.md`: the Apache-2.0 client and BUSL-1.1 server split.

In short: ACS is a message hub for coding agents. An orchestrator (you,
or an agent through MCP) delegates a task to a worker agent (Claude Code,
Codex, Cursor, Hermes, Grok, Kilo, OpenCode, or any CLI) on any machine.
It gets a structured result back under one reply contract, and every
message can be watched live and queried later. The supporting features
are:
- multi-turn sessions;
- parallel waves with dependencies and merge gates;
- advisory file locks between agents;
- durable delivery on JetStream;
- history, stats and a visualizer.

**Hosted** means sign in at `acs.agentmemorylabs.com` and get your own
isolated hub (one NATS account per tenant). Then
`acs login && acs mcp install claude` on each machine, or add the remote
MCP connector in a web chat app. **Self-hosting** stays first-class and
free.

The core idea for the design to come from is **many agents, many
machines, one conversation you can see**. The signature moment should
grow out of that, for example a live view of a task going from an
orchestrator to a worker and back. It must not be a generic glowing orb.
Build it as a vgpu shader or scene: for example, messages travelling as
light along routes between agent nodes, with the reply contract
(progress, then one terminal result) visible in the motion. Its poster
frame has to tell the same story without the GPU.

## Pages and sections

Homepage (`/`), in roughly this order. Adjust the order using the
marketing-anatomy reference.

1. **Nav:** logo, Docs, Pricing, GitHub, **Sign in** (links to
   `https://acs.agentmemorylabs.com/signin`), and a primary CTA, **Request
   an invite**.
2. **Hero:** one sharp line, one supporting sentence, the two CTAs, and
   the signature visual.
3. **How it works:** three steps (sign in → connect your agents →
   delegate), with real commands in copyable code blocks.
4. **Features (bento):** delegation and the reply contract, sessions,
   waves, file locks, durable delivery, observability (history, stats,
   visualizer).
5. **Works with:** the supported agents and clients: Claude Code, Codex,
   Cursor, Hermes, Grok, Kilo, OpenCode, any CLI, and web chat apps
   through remote MCP. Use text or wordmarks. Don't use other companies'
   logos unless their brand guidelines allow it.
6. **Hosted vs self-host:** what you get with each, honestly.
7. **Security:** tenant isolation, short-lived credentials, instant
   revocation, and identity stamped by the server. Source: `SECURITY.md`
   and `HOSTED_PLAN.md` §3 and §8. No compliance badges we don't have.
8. **Pricing teaser,** linking to `/pricing`.
9. **FAQ.**
10. **Final CTA.**
11. **Footer:** Docs, Pricing, GitHub, Terms
    (`https://acs.agentmemorylabs.com/terms`), Privacy
    (`https://acs.agentmemorylabs.com/privacy`), licensing, and
    © Agent Memory Labs.

**`/pricing`:** the pricing unit is **not decided yet** (seat, agent
identity, or usage). Don't invent prices. Use three columns:
- **Self-host:** free, open source. Apache-2.0 client and BUSL-1.1
  server.
- **Hosted beta:** free during the invite-only beta. Include the real
  beta limits from `acs-private` if you can find them; otherwise leave
  them out.
- **Team:** "Coming later". Ask to be told when it launches.

Put every price, limit and plan name in one data file
(`site/content/pricing.ts`), so I can change them without touching the
layout. Add a short FAQ on pricing.

**`/docs`:** a real documentation section, not a link out.
- Render the repo's own Markdown from `../docs` and `README.md` at build
  time, so the docs never fork. Choose and order the pages in one
  manifest (`site/content/docs-manifest.ts`):
  - Getting started: hosted, and self-host;
  - the `acs` connector;
  - remote MCP;
  - worker backends;
  - delivery;
  - waves;
  - file locks;
  - security;
  - bridges;
  - the email plugin (experimental).
- Leave out the internal planning docs: `HOSTED_PLAN.md`,
  `OPEN_DECISIONS.md`, `docs/archive`, `docs/refocus*`.
- Include a sidebar, a table of contents for the current page, heading
  anchors, code blocks with copy buttons, previous/next links, and
  **⌘K search** across the docs.
- Fix links between docs so they resolve inside the site.

Also add a 404 page, Open Graph and social images, a favicon, and
`sitemap.xml` / `robots.txt`.

## The invite CTA

There is no waitlist backend. "Request an invite" goes to a single
constant, `INVITE_REQUEST_URL` in `site/content/site.ts`. Default it to
`mailto:` with a placeholder address I will replace, and flag it in your
report. Don't add a form backend, analytics or third-party scripts
without asking me.

## Constraints

- **Honesty:**
  - no fake testimonials, customer logos, user counts or benchmarks;
  - mark anything experimental as experimental;
  - make no claim that the docs don't support.
- **Licensing:** add a `REUSE.toml` annotation for `site/**`
  (Apache-2.0, the same as the client code). CI runs `reuse lint` and
  `tests/python/test_licensing.py`, and both must stay green. Don't add
  per-file license headers.
- **Keep CI green:** the site must not be picked up by the Rust or Python
  suites. Don't add Node to the existing CI jobs. If you want a site
  check (build, lint, link check), add it as its own job in
  `.github/workflows/`, limited to `site/**`.
- **Size and dependencies:** keep files under ~400 LOC. Keep the
  dependency list short and justify each one in your report.
- **Don't touch:**
  - DNS or Cloudflare;
  - the running production stack on this VM (`/data/acs-beta`, Docker);
  - the private repo `acs-private`;
  - the existing `agentmemorylabs.com` site.
- **Performance:**
  - Lighthouse 95+ on Performance, Accessibility, Best Practices and SEO
    for `/`, `/pricing` and one docs page;
  - nothing that shifts the layout;
  - honour `prefers-reduced-motion`;
  - vgpu stays out of the critical path: Lighthouse runs (no GPU) must
    still score 95+;
  - report the hero's added JS in the PR.
- **Where you run:** a cloud VM with **no GPU**.
  - For headless checks, launch Playwright Chromium with
    `--enable-unsafe-webgpu --enable-features=Vulkan --use-vulkan=swiftshader --use-webgpu-adapter=swiftshader`.
    That gives a software WebGPU adapter: good for correctness,
    screenshots and recordings, but not for frame rate.
  - WebGPU needs a secure context, so load pages from
    `http://localhost:<port>`, not `about:blank`.
  - Report the SwiftShader frame time as a rough guide only.
  - The real-GPU check (60 fps, how it actually looks) is mine. Run the
    dev server on port 3000 and tell me when it's ready; I'll view it
    from my laptop through `ssh -L 3000:localhost:3000`. Follow vgpu's
    `performance-measurement` guidance so the measurement is easy to
    repeat there.
- **Themes:** dark first. Light mode too if bespoke's
  `devtool-visual-system` gives a clean ladder for it.

## Definition of done

1. `site/` builds with `npm run build` and no warnings, and
   `npm run lint` is clean.
2. A running preview (a local production build; a Vercel preview URL as
   well if you deployed one) works for:
   - `/`, `/pricing`, `/docs` and three docs pages;
   - the 404 page;
   - ⌘K search;
   - the vgpu hero both with WebGPU and with the fallback (WebGPU
     disabled).
   Every shader passes `npx vgpu check`.
3. `scan.sh` findings are fixed or justified, and Lighthouse scores are
   reported for the three pages.
4. Screenshots at 1440 px and 390 px of the homepage, pricing and one
   docs page, in each theme you shipped, plus the hero's poster frame and
   a short recording of the live hero. Use the built-in preview tools.
5. Every link and command on the site is checked: internal links
   resolve, and each command copied from the site matches `docs/`.
6. `iter2/homepage` is pushed, with a PR to `main`. The PR description
   includes:
   - the preview URL, if you deployed one;
   - the screenshots;
   - the design rationale, written the design-mentor way: palette,
     type and the signature moment, each with the principle behind it in
     one line;
   - the dependencies;
   - open questions for me (at least: the invite address, the final
     domain, and the pricing copy).

Don't merge. I'll review the preview first.
