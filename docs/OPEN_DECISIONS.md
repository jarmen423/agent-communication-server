# Open decisions

**Status:** 2026-10-22. These are the decisions that only the owner can make.
Everything that can be built without them is done
([`HOSTED_PLAN.md`](HOSTED_PLAN.md) §10). Each entry gives the question, the
options, a recommendation, what it blocks, your answer, and my response. Once a decision is made, record it in `HOSTED_PLAN.md`
and move the entry to "Already decided".

## Summary

| # | Decision | Status |
|---|---|---|
| 1 | First release | ✅ `v0.0.1`, GitHub release + curl installer; Homebrew and npm next |
| 2 | Private repo | ✅ `agentmemorylabs/acs-private` |
| 3 | Control-plane language | ✅ Rust/axum (recommended; proceeding) |
| 4 | Sign-in | ✅ GitHub + Google OAuth + email magic links; invite codes in beta |
| 5 | Tenant model | ✅ one user = one tenant |
| 6 | Identity | 🟡 proposal: account → user → device → agent (trusted), plus a self-reported session id |
| 7 | Domain | ✅ `acs.agentmemorylabs.com` |
| 8 | Cloud | ✅ for the beta: one VM on GCP us-east, containerized (portable to UpCloud) |
| 9 | Infra | ✅ 1 NATS node, self-run Postgres, file-based keys; 🟡 set a billing alert |
| 10 | Beta scope | ✅ free, invite-only, 5–10 founders |
| 11 | Legal | ✅ fine for a free invite-only beta; terms/privacy page before invites |
| 12 | Pricing unit | open (H-4) |

## Decisions

### 1. Cut a first release

No GitHub release or tag exists yet. Testers and remote machines can only get
`acs` and the `hub-*` CLIs by building from source. The release workflow
(`.github/workflows/release.yml`, `docs/RELEASING.md`) and
`scripts/install_remote.sh` are ready.

- **Options:** (a) tag `v0.1.0` as a GitHub pre-release now; (b) wait for H-2.
- **Recommendation:** (a). It also tests the install path before the beta.
  Homebrew tap later.
- **Answer:** v0.0.1 for now. What is in H-2? It's a rust binary right? Where else besides homebrew should we release? Can we release a rust project on npm/bun or we just do a curl install script.
- **Response:**
  - **What is H-2?** H-2 is the control plane: the website and API behind
    `acs login` (accounts, devices, credentials). It is a server we deploy,
    not something users install, and it lives in the private repo. What
    users install is `acs` plus the `hub-*` CLIs, and that is what the
    release ships.
  - **Where to release:**
    - **v0.0.1:** a GitHub release plus the existing curl installer
      (`scripts/install_remote.sh`, sha256-verified).
    - **Next:** a Homebrew tap and an npm package. Rust binaries ship on
      npm the way esbuild and biome do: a small wrapper package plus one
      package per platform holding the prebuilt binary, so
      `npx @agentmemorylabs/acs login` and `bunx` work. `cargo-dist` can
      generate the curl, Homebrew and npm installers from one config.
    - **Not yet:** crates.io and Windows.
  - Plain `v0.0.1`, not a pre-release: GitHub's "latest" link, which the
    installers use, skips pre-releases.
- **Status:** ✅ decided. v0.0.1 (crate versions set to 0.0.1).

### 2. Private repo for the control plane

The control plane (sign-up, tenants, devices, credential minting, billing)
is private by design (`HOSTED_PLAN.md` §6). It replaces the DEV-ONLY
`stub_control_plane/`.

- **Options:** (a) a repo under your personal account; (b) a new GitHub org
  matching the licensor ("Agent Memory Labs").
- **Recommendation:** (b), so the public repo can move into the same org
  later and access is managed per org.
- **Answer:** yes should have a private repo. org: agentmemorylabs/acs-private
- **Status:** ✅ decided. `agentmemorylabs/acs-private` (private).

### 3. Control-plane language

- **Options:**
  - (a) **Rust/axum.** It shares the nkey/JWT code with `acs` and
    `hub-admin`, uses one toolchain, and is the same stack the agents already
    work in.
  - (b) **TypeScript.** Faster for web pages, but the JWT/nkey signing and
    account push would be reimplemented.
  - (c) **Grow the Python stub.** Fastest start, but the stub was written as
    throwaway code.
- **Recommendation:** (a) for the API. The web UI (sign-in pages now,
  dashboard in H-4) can stay server-rendered, or be TypeScript later.
- **Answer:**  What is your reccomendation here?
- **Response:** Rust/axum.
  - **Shared code.** The hard part of the control plane is signing NATS
    account and user JWTs and handling nkeys. That code already exists in
    Rust (`acs`, `hub-admin`), so the control plane reuses it instead of
    porting it.
  - **One stack.** One language, one CI, and the stack every agent on this
    project already works in.
  - **Web pages.** Sign-in and device approval are a few server-rendered
    pages. A TypeScript frontend only becomes worth it for the dashboard
    (H-4).
- **Status:** ✅ decided. Rust/axum (proceeding unless you object).

### 4. Sign-in method

The stub auto-approves a single dev user. Real accounts need a real sign-in.

- **Options:**
  - (a) GitHub and Google OAuth, implemented directly.
  - (b) a hosted auth provider (WorkOS, Clerk, Auth0).
  - (c) email magic links.
- **Recommendation:** (a). The audience is developers, it keeps the account
  data in our Postgres, and there are no passwords to store. Revisit (b)
  if enterprise SSO matters.
- **Answer:** (a) + what are magic links? if that lets us allow non google email signups and avoids dupes/abuse then yes that too. 
- **Response:**
  - **What a magic link is.** The user types an email address, and we
    email a one-time sign-in link that expires in about 15 minutes.
    Clicking it proves they own that address. Any email works, with no
    password.
  - **Duplicates.** One account per **verified email**. Google, GitHub and
    a magic link for the same address land in the same account.
  - **Abuse.** A magic link alone doesn't stop throwaway addresses. For the
    beta, **invite codes** solve that completely. After the beta, add rate
    limits per IP and per email, plus a disposable-domain blocklist.
  - **Cost.** Magic links need an email sender: Resend's free tier covers
    3,000 emails a month. They also need SPF/DKIM DNS records on the
    domain.
- **Status:** ✅ decided. GitHub + Google OAuth + email magic links, with
  invite codes during the beta.

### 5. Tenant model at beta

- **Options:** (a) one user = one tenant; (b) orgs with several members from
  day one.
- **Recommendation:** (a). The data model keeps a `tenant` separate from
  `user`, so teams can be added later without a migration of hubs.
- **Answer:** (a)
- **Status:** ✅ decided. One user = one tenant.

### 6. Identity naming (`HOSTED_PLAN.md` §11.5)

- **Options:** (a) free-form names, unique per tenant (current rules);
  (b) default to `<device>-<agent>`, e.g. `laptop-claude`.
- **Recommendation:** (a), with `acs agent add` suggesting
  `<hostname>-<agent>` when no name is given.
- **Answer:** we need to have identity for agent by provider/agent and also session id so it can be traced back to the exact session/conversation. Also by the human user its tied to and the account its tied to. 
- **Response:** Proposed model: five layers, where each one is either
  trusted (from the credential) or self-reported.

  | Layer | Example | Source | Trusted? |
  |---|---|---|---|
  | account (tenant) | `T_A` | the NATS account of the connection | yes |
  | human user | `josh@…` | the control plane, recorded when the identity is minted | yes |
  | device | `laptop` | `acs login` | yes |
  | agent identity (provider/agent) | `claude-laptop`, provider `claude-code` | `acs agent add --provider`, set automatically by `acs mcp install` | yes |
  | session / conversation | the Claude Code session id, the Codex thread id | the MCP server or worker, per conversation | **self-reported** |

  - **The agent identity stays the address.** It is what others DM and what
    NATS subjects bind to, so it has to be stable.
  - **A session can't be an identity.** That would need a new credential
    for every conversation. Instead, each envelope gets an `origin` block
    in `meta`:
    - the router stamps the trusted layers, from the bound subject plus
      what the control plane registered for that identity;
    - the client adds the session id.
  - **What this gives you:** every message and task traces back to
    account → human → device → agent → conversation. The session id is
    an audit and trace aid, not a security boundary: an agent could lie
    about its own session, but not about who it is.
  - **Open research:** which clients expose a conversation id to an MCP
    server or worker. Claude Code hooks receive `session_id`, and Codex has
    thread ids. Remote MCP from claude.ai/ChatGPT probably only gives a
    per-connection id.
- **Status:** 🟡 proposal. Confirm it and it becomes a public-repo task
  (wire protocol + router + MCP server), independent of H-2.

### 7. Domain and product name

The OAuth issuer, the remote MCP URL users paste into claude.ai/ChatGPT,
the `wss://` NATS endpoint and the default `acs login` server all embed a
domain. Changing it after the beta breaks every connected device and
connector.

- **Needed:** the product name shown on sign-in and connector pages
  (still "ACS"?), and a domain you own or will buy.
- **Proposed layout:** `app.<domain>` (sign-in, dashboard),
  `mcp.<domain>` (remote MCP), `connect.<domain>` (NATS over `wss://`).
- **Answer:** Thinking to make this a sister project of agentmemorylabs.com so maybe use that domain? acs.agentmemorylabs.com. or should i buy a new domain for this?
- **Response:** Use agentmemorylabs.com; don't buy a new domain now.
  - **Hosts:**
    - `acs.agentmemorylabs.com`: sign-in, dashboard, OAuth issuer, and the
      remote MCP endpoint at `/mcp`;
    - `connect.acs.agentmemorylabs.com`: NATS over `wss://`.
  - **Switching later is cheap now.** If ACS later becomes its own brand,
    moving means every user re-runs `acs login` and re-adds the connector.
    That is cheap with beta-sized numbers and expensive later, so decide
    before a public launch, not before the beta.
- **Status:** ✅ decided. `acs.agentmemorylabs.com` (you need DNS access when
  we deploy).

### 8. Cloud provider and region (`HOSTED_PLAN.md` §11.3)

The design needs a 3-node NATS cluster with JetStream, one hub-server
container per tenant with a persistent volume, managed Postgres and a KMS.

- **Options:**
  - (a) **GCP.** You already run this VM there. Uses Compute Engine or GKE,
    Cloud SQL and Cloud KMS.
  - (b) **Fly.io.** The simplest per-tenant machines with volumes,
    scale-to-zero later and a cheap start. Fewer managed pieces (Postgres
    via a partner, no KMS).
  - (c) **AWS.** The most complete, the most setup.
  - (d) **Hetzner.** The cheapest, but everything is self-managed.
- **Recommendation:** (a), one region close to you (`us-central1` or
  `us-east1`). For the beta: 3 small VMs for NATS, one VM running tenant
  containers, and Cloud SQL.
- **Answer:** east is closest to me. you think we should have seperate vms will probably start on gcp i have startup credits there. I've also seen upcloud have a very generous $500 free credits i could make a new account there and start there. This could be decided last right? I cant imagine doing like hosted postgres or anything well probably just do everything on vms or maybe if you can convince me that some scale to zero type of deployment is better i might consider that. 
- **Response:** Yes, this can be decided last. Everything will ship as
  containers plus one compose file, so moving between GCP and UpCloud is a
  copy, not a rewrite.
  - **Beta layout: one VM.** It runs Caddy (TLS), nats-server with
    JetStream, the control plane, Postgres, and one hub-server container
    per tenant. A 4 vCPU / 16 GB machine holds dozens of tenants, which
    can be measured before inviting anyone.
  - **Scale-to-zero doesn't fit, for now.** Agents keep long-lived NATS
    connections, and every tenant hub needs a disk. Serverless platforms
    (Cloud Run and similar) are built for short HTTP requests. An idle
    tenant hub is a small process, so it costs little to leave running.
    Revisit only if idle tenants come to dominate the bill.
  - **Where:** GCP `us-east1` or `us-east4`, on your startup credits.
    UpCloud is a fine fallback with the same layout.
- **Status:** ✅ decided for the beta. One VM on GCP us-east, containerized.

### 9. Beta infra budget

Nothing that costs money gets created without this.

- **Rough size for (8a) at beta scale:** 3 small NATS VMs, 1 tenant host,
  1 small Cloud SQL instance, KMS, egress.
- **Needed:** a monthly cap, and whether a billing alert should stop or
  only notify.
- **Answer:** why do we need 3 NATS vms? why would we do cloud sql > just deploying postgres ourselves on the vm? what is KMS?
- **Response:**
  - **3 NATS VMs:** only for high availability. With 3 nodes and
    JetStream replication, losing one machine causes no downtime and loses
    no messages. A beta doesn't need that. One NATS node keeps durable
    messages on disk, and a restart is a short blip that clients reconnect
    through. Go to 3 nodes when uptime matters to paying users.
  - **Cloud SQL vs our own Postgres:** Cloud SQL buys automatic backups,
    patching and failover, for about $10–30 a month at the smallest size.
    Our own Postgres in a container on the VM is fine for the beta, with a
    nightly `pg_dump` to object storage. Agreed: self-run.
  - **KMS (Key Management Service):** a cloud vault that holds keys you
    can *use* (sign with) but never download. It matters here because the
    control plane holds the keys that sign every tenant's credentials;
    whoever steals them can connect as anyone. For the beta: the key
    lives in a file only the control-plane service can read, on an
    encrypted disk, with an offline backup. Move to KMS before a public
    launch.
- **Status:** ✅ decided. 1 NATS node, self-run Postgres, file-based keys
  for the beta. The startup credits should cover it. Still set a billing
  alert on the account (e.g. notify at $50/month) when it is created.

### 10. Beta scope

`HOSTED_PLAN.md` H-3 exits after "2 weeks of beta tenants without manual
intervention".

- **Needed:**
  - **Who:** invite-only friends, a waitlist, or public.
  - **How many:** for example 5–10.
  - **Which paths must work on day one:** local CLIs via `acs`, bots, web
    connectors (claude.ai/ChatGPT/Grok).
- **Recommendation:** invite-only, 5–10 people, with all three paths.
  Before inviting anyone, do one real claude.ai/ChatGPT connector test
  (`make demo-remote-mcp` through a tunnel).
- **Answer:** Do you think we need to let people try for free? I'll have to start talking with other founders who might be interested.
- **Response:** Yes, free. Founders who give you feedback are design
  partners; their feedback is worth more than their money right now, and
  the beta costs little to run. Say up front that it is "free during the
  beta, pricing announced before it ends", so nobody feels baited later.
  Start with 5–10 founders and invite codes.
- **Status:** ✅ decided. Free, invite-only, founders.

### 11. Legal basics

A hosted service stores other people's messages.

- **Needed:**
  - Whether "Agent Memory Labs" is (or will be) a legal entity. It is the
    licensor in `LICENSE`.
  - A terms-of-service and privacy page.
  - A default message-retention period. W2-B retention is configurable;
    proposed 30 days for the beta.
- **Answer:** It's not a legal entity yet but if i start making real money with these projects it likely will be. 
- **Response:** That works for a free, invite-only beta with people you
  know (this isn't legal advice).
  - **Before the first invite:** put up a short terms and privacy page:
    what is stored, how long it is kept, no warranty, and how to delete an
    account.
  - **Before charging anyone:** form the entity.
  - **Retention:** proposed 30 days for message history during the beta.
- **Status:** ✅ decided for the beta. I'll draft the terms/privacy page
  with H-2.

### 12. Pricing unit (`HOSTED_PLAN.md` §11.7)

- **Options:** (a) per seat; (b) per agent identity; (c) usage (messages,
  storage); (d) a mix.
- **Recommendation:**
  - **Beta:** free.
  - **After the beta:** per seat with a generous number of included agent
    identities and a usage cap. Per-identity pricing punishes the thing the
    product wants users to do (connect many agents).
- **Answer:** _open_ (not needed until H-4)

## Deferred (no decision needed yet)

- **MCP-only install without `acs`** (`HOSTED_PLAN.md` §11.6): after the beta.
- **Scale-to-zero tenant hubs:** after the beta.
- **HA hub-server (SurrealDB server mode):** after the beta.

## Already decided

- **License split:** Apache-2.0 client and `acs`, BUSL-1.1 server, private
  control plane. Licensor "Agent Memory Labs" (2026-10-20, `LICENSING.md`).
- **Repo visibility:** the repo and `HOSTED_PLAN.md` stay public for now
  (2026-10-20).
- **Tenancy:**
  - one NATS account per tenant;
  - JWT-based auth with no auth callout;
  - tenant-wide extra channels (H-0, `HOSTED_PLAN.md` §3).
- **One hub-server container per tenant**, with managed Postgres for the
  control plane (`HOSTED_PLAN.md` §5–6).
- **ACS is the connector for all agents:** CLIs, personal bots (dots, grok
  bot, muse) and web chat apps (2026-10-20).
