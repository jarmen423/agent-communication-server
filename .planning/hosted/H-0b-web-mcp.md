# H-0b — Remote MCP connectors in web chat apps (research, 2026-10-21)

Research only. All official pages were read on 2026-10-21, and none of the
developer-doc pages carry a publication date. The weakest points are:

- which ChatGPT consumer plans allow write tools;
- ChatGPT's tool-call timeout;
- nearly everything about Grok's protocol.

Summary and design implications: `docs/HOSTED_PLAN.md` §4b.

## claude.ai (web, Desktop, mobile)

- **Feature:** "Custom connectors": Customize → Connectors → "+ Add" → "Add custom connector". On Team/Enterprise an Owner adds the connector in Organization settings, then each member connects their own account. [C1, C2]
- **Plans:** Free (one custom connector), Pro, Max, Team, Enterprise. [C1]
- **Auth:**
  - OAuth per MCP auth spec 2025-03-26, 2025-06-18 and 2025-11-25.
  - Client identity: DCR, CIMD, an Anthropic-held client, or the user's own client ID.
  - PKCE S256 is mandatory. The server must answer `401` with a `resource_metadata` pointer.
  - Callback: `https://claude.ai/api/mcp/auth_callback`.
  - No-auth is allowed. Static headers are in beta.
  - OAuth endpoints get 10 s to respond (30 s for token refresh).
  - "Lazy authentication" lets public tools work before sign-in. [C3, C4]
- **Transport:** streamable HTTP; legacy SSE is still supported. [C3]
- **Limits:**
  - Tool calls time out after 240 s (an older FAQ said 300 s).
  - Results are capped at ~150,000 characters.
  - No resource subscriptions, sampling or draft MCP features.
  - Claude reaches servers from Anthropic's published IP range. [C3, C5]
- **Confirmation:** approval prompts with "Always allow"; tools can be blocked. Research mode runs tools without asking. [C1, C2]
- **Directory:** the Connectors Directory, at claude.ai/directory/manage, is open to any paid plan.
  - An automated scan lists a connector as "Community"; an optional human review makes it "Verified".
  - Every tool needs a `title` and `readOnlyHint`/`destructiveHint` annotations. [C6]
  - Custom connectors also get a prefilled install link. [C7]

## ChatGPT (chatgpt.com)

- **Feature:** "Add custom MCP server", created as a plugin at chatgpt.com/plugins → "+", and used in **Work** chats by typing `@`. [O1, O2]
- **Plans:** write tools on Business, Enterprise and Edu (beta). Plus/Pro are reportedly read/fetch only, but this is **unverified**: help.openai.com returned 403, so it comes from search snippets. [O3]
- **Auth:**
  - OAuth 2.1 per MCP auth spec 2025-11-25.
  - Client identity: CIMD (preferred), DCR or a predefined client, with PKCE S256.
  - Modes: "OAuth", "No authentication", or "OAuth or no authentication" (mixed, per tool).
  - Callback: `chatgpt.com/connector/oauth/{callback_id}`, or the stable `connector_platform_oauth_redirect`. [O1, O4]
- **Transport:** SSE and streamable HTTP. [O1]
- **Limits:** no official tool-call timeout. A forum post (2026-04-27, not confirmed as staff) says there is a hard limit of 1 minute. [O6]
- **Confirmation:** writes need confirmation by default. Only `readOnlyHint` tools skip it, and an approval can be remembered for the rest of the conversation. [O1]
- **Search/fetch:** "Custom MCP servers do not require `search`/`fetch` tools". [O1]
- **MCP Events:** ChatGPT subscribes to the server and receives webhooks.
  - Requires MCP protocol `2026-07-28`.
  - Work chats only.
  - No polling or streaming. [O7]
- **Secure MCP Tunnel** is available for private servers. [O2]
- **Directory:** one plugin directory shared by ChatGPT and Codex. Submission needs a verified developer identity, 5 positive and 3 negative test cases, and a review. [O5]

## Grok (grok.com)

- **Feature:** "Custom MCP connectors": grok.com/connectors → New Connector → Custom. Available "to all Grok users". On Business/Enterprise an admin adds it at console.x.ai, then members connect. [X1, X2]
- **Auth:** the docs only say "complete any required authentication". Third-party write-ups describe an OAuth pop-up. DCR/PKCE: not found. [X1, X5]
- **Transport:** not documented for grok.com. xAI's API remote-MCP tool supports both. [X4]
- **Requirements:** the server must be on the public internet; xAI documents tunnelling for local servers. [X1]
- **Launch date:** 2026-05-06, per a secondary source. [X5]
- **Directory:** a curated catalog; no submission process found. [X3]

## Others

- **Gemini (consumer):** "custom apps" under Settings → Connected Apps → "Add a custom app". English-only, needs Keep Activity on, and always asks for manual confirmation on writes. [G1] Gemini Enterprise is a separate path and supports streamable HTTP only. [G2]
- **Perplexity:** "custom remote connectors" (Add custom → Connector → Remote, HTTPS). Auth can be none, API key or OAuth 2.0. Pro and Enterprise per its help center as quoted by third parties; Max is unconfirmed. [P1]

## Sources

- C1 https://support.claude.com/en/articles/11175166 ("updated over 2 weeks ago")
- C2 https://claude.com/docs/connectors/custom/add-unlisted.md
- C3 https://claude.com/docs/connectors/building/index.md
- C4 https://claude.com/docs/connectors/building/authentication.md
- C5 https://support.claude.com/en/articles/11596036-anthropic-mcp-directory-faq (now a redirect stub, 2026-08-06; the 300 s figure was seen only in search snippets)
- C6 https://claude.com/docs/connectors/building/submission.md
- C7 https://claude.com/docs/connectors/building/directory-vs-custom.md
- O1 https://developers.openai.com/api/docs/guides/custom-mcp-server
- O2 https://developers.openai.com/plugins/deploy/connect-chatgpt.md
- O3 https://help.openai.com/en/articles/12584461 (403; search snippets only)
- O4 https://developers.openai.com/plugins/build/auth.md
- O5 https://developers.openai.com/plugins/deploy/submission.md
- O6 https://community.openai.com/t/how-to-configure-long-mcp-tool-call-times-for-chatgpt-app/1379834
- O7 https://developers.openai.com/plugins/build/mcp-events.md
- X1 https://docs.x.ai/grok/connectors
- X2 https://docs.x.ai/grok/connector-management
- X3 https://docs.x.ai/grok/connectors/catalog
- X4 https://docs.x.ai/developers/tools/remote-mcp
- X5 https://beginnersinai.org/grok-connectors-launch-2026/ (updated 2026-09-23), https://docs.anysite.io/mcp-server/unlimited-plan-grok
- G1 https://support.google.com/gemini/answer/17209137
- G2 https://support.google.com/g/answer/17106276
- P1 https://www.perplexity.ai/help-center/en/articles/13915507-adding-custom-remote-connectors
