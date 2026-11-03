# Agent Communication Server brand

Logo, colours and type for ACS. The live design system is at https://claude.ai/artifact/X7Kp6DdnukXVLAvomTFtHA; this folder holds a copy of its files: the logo SVGs in `logo/`, the tokens in `tokens.json`, and standalone previews of the lockup and cover in `previews/`.

Agent Communication Server is the message bus agents use to talk to each other. The brand is two speech bubbles that overlap: one agent sends, another replies, and the server is the place where they meet.

## Logo

The mark is two bubbles on a 64-unit grid. The upper-left bubble is `relay-teal` (the sender, sharp corner bottom-left). The lower-right bubble is `signal-orange` (the reply, sharp corner bottom-right). Their overlap is a leaf-shaped lens in `ink`: the server.

- Use `logo/mark-light.svg` on `paper` or `surface-raised`, and `logo/mark-dark.svg` on the dark theme's `paper`.
- Use `logo/mark-mono.svg` (single ink `#171a21`, lens cut out) for one-colour print, favicons on busy grounds and embossing.
- Lockup: the mark at 1.6× the cap height of the `wordmark` style, followed by "Agent Communication Server" in `wordmark`, gap `space-2`. See `previews/logo.html`.
- Keep clear space of a quarter of the mark's width on every side (`space-4` at 64px).
- Minimum size: 16px for the mark alone, 24px mark height in the lockup.
- Don't recolour the bubbles, swap their order, rotate the mark, add outlines or shadows, or set the wordmark in another face.
- The short form in UI chrome and logs is "ACS". Write the full name the first time on any page.

## Colour

- `paper` is the ground; `ink` on `paper` for text, `ink-muted` for secondary text.
- `relay-teal` is the primary: links, primary buttons, outbound messages. Put `on-teal` text on it.
- `signal-orange` is the accent: inbound messages, highlights, the one thing on a screen that needs attention. Put `on-orange` text on it. Never use it for body text on `paper`.
- `lane` is the dark band for message queues, terminals and code.
- `line` draws hairlines. Prefer a hairline to a shadow.
- Teal always means "sent" and orange always means "received". Keep that pairing in every diagram and chat view, and label direction in words too, never by colour alone.

## Type

- `display-xl` and `display` are Bricolage Grotesque, for titles only.
- `wordmark` is Bricolage Grotesque 600, for the lockup.
- `heading`, `body` and `caption` are IBM Plex Sans.
- `mono` is IBM Plex Mono, for agent IDs, channel names and payloads: `agent://planner`, `#build.queue`.
- All three families load from Google Fonts.

## Voice

Plain and exact, like a good log line. Name the agent, the channel and what happened: "planner sent 3 messages to #build.queue". Sentence case everywhere. No emoji.
