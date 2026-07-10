# Arcade visualizer pets

Agent sprites come from local Petdex installs under `visualizer/pets/<slug>/` (copied from `~/.hermes/pets/`). Each pet ships a `spritesheet.webp` (192×208 cells, 6 frames per animation row).

## Identity → pet assignment

1. **Explicit map** (stable demo agents):
   - `petdex-agent` → batmeme
   - `design-agent` → jill-stingray
   - `surreal-agent` → maisenpai

2. **Hash fallback** — any other identity is assigned `INSTALLED_PETS[hash % 4]` over `batmeme`, `jill-stingray`, `maisenpai`, `scoop`.

## Status → animation

| Agent status | Pet row | Notes |
|---|---|---|
| `working` | run | faster frame loop (~620ms) |
| `thinking` | review | |
| `idle`, `ready` | idle | gentle bob |
| `error` | failed | red tint if sheet has no failed row |
| stopped | idle | grayscale tint |

Sprites load from `visualizer/pets/<slug>/spritesheet.webp` (served by `hub-server --static-dir visualizer/`). If load fails, the visualizer falls back to the octagonal chip + glyph.

## Verify

```bash
./target/debug/hub-server --db-path nats_hub.db --ws-addr 127.0.0.1:9191 --static-dir visualizer/
# open http://127.0.0.1:9191/ — agents show animated pets once hub traffic arrives
curl -I http://127.0.0.1:9191/pets/batmeme/spritesheet.webp
```