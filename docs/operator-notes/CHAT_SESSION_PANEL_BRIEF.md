# Chat session panel (arcade visualizer)

## Goal
Replace the CRT **LOGS** modal with a **coding-agent chat UI**.

Operator pain (current):
- Separate popup actions: **Send message** (`window.prompt`) vs **View session logs**
- Log viewer is raw terminal lines (`[time] KIND task… body`) — not a conversation
- Cannot type into the session view like Cursor / Claude / ChatGPT

## UX contract (must)

1. **Primary path:** click agent → open **chat panel** for that agent (not a two-step menu for talk vs logs).
2. **Chat transcript** (scrollable, newest at bottom):
   - **You** (operator) bubbles — right-aligned, accent color (purple/cyan)
   - **Agent** bubbles — left-aligned, agent palette color
   - **System / status / progress** — subtle centered or left muted chips (not giant raw logs)
3. **Composer** fixed at bottom of panel:
   - multiline `<textarea>` placeholder like `Message design-agent…`
   - **Send** button
   - **Enter** sends, **Shift+Enter** newline
   - disable while empty; optimistic append of operator bubble on send
4. **No `window.prompt`** for messaging when the chat panel is available.
5. Action popup can stay for **Stop / Resume / Open chat / Change pet** etc., but **Open chat** is the main session path (rename away from “View session logs”).
6. Live: when open for agent X, new bus envelopes for X append as chat rows (reuse `envelopes` + `refreshOpenLogIfNeeded` / rename to chat).
7. ESC / backdrop / ✕ close; focus textarea on open; stop p5 pointer steal (`pointerOnUi` already covers `#session-detail`).
8. Keep dark arcade aesthetic (cyan borders, glass panel) — **chat layout**, not terminal dump.
9. Prefer human-readable body via existing `extractEnvelopeBody` / `prettySnippet` — hide noisy internal channels unless useful as small meta under the bubble.
10. Single file: edit `visualizer/index.html` only (unless a tiny helper is cleaner).

## Anti-patterns
- Do not keep a separate “send message” flow that only uses browser `prompt()`.
- Do not dump raw JSON as the default view.
- Do not break WebSocket `send_message` / stop / resume.
- Do not rewrite the whole p5 floor — panel + popup wiring only.
- Respect `prefers-reduced-motion` (no layout thrash animations).

## Implementation sketch
- Replace `#session-detail` markup: header (agent name + status + close) · `#chat-messages` · `#chat-composer` form
- CSS: flex column panel (~min 640px / max 92vw / 80vh); bubble rows; composer row
- JS:
  - `showAgentChat(agent)` opens panel, renders history from `envelopes` filtered by from/to identity
  - Map kinds: operator `meta.from` is human/josh/operator → you; agent identity → agent; event/status → system
  - On send: `ws.send({type:'send_message', to, message})` + optimistic bubble + toast
  - `popupAction('view'|'message')` → open chat (message can focus composer)
  - Double-click agent optional: open chat immediately

## Verify
1. Hard refresh `http://127.0.0.1:9191/`
2. Click an agent → chat panel (not log dump)
3. Type + Send → bubble appears, WS routes task
4. Agent reply / progress appears as chat rows live
5. ESC closes; floor still works
