# License: not decided yet

**nats-hub has no license yet.** There is no `LICENSE` file, and the manifests
below contradict each other, so don't treat any one of them as the license.
The choice is open decision #1 in
[`refocus-iteration-2.md`](refocus-iteration-2.md) §7. The candidates are
**BSL-1.1**, **MIT** and **Apache-2.0**.

This note records where the license is declared today and how to switch all of
those declarations in one step once the decision is made. The declarations are
deliberately left unchanged until then, even though they disagree.

## Where the license is declared today

| Location | Says | Notes |
|---|---|---|
| `Cargo.toml` → `license` | `BSL-1.1` | **Not a valid SPDX id.** The SPDX id for the Business Source License is `BUSL-1.1`; `BSL-1.0` is the unrelated Boost license. `cargo publish` would warn or reject it. |
| `claude-code-plugin/.claude-plugin/plugin.json` → `license` | `MIT` | The Claude Code plugin manifest |
| `codex-plugin/.codex-plugin/plugin.json` → `license` | `MIT` | The Codex plugin manifest |
| `hermes-plugin/plugin.yaml` | *(none)* | The Hermes plugin manifest has no license key |
| `README.md` → "License" section | "BSL 1.1 — converts to Apache 2.0 on 2030-01-01. The SurrealDB Rust SDK is Apache 2.0." | Between `<!-- license:start -->` / `<!-- license:end -->` markers. The terms quoted (BSL 1.1 converting to Apache 2.0 on 2030-01-01) are **SurrealDB's**, as written up in `docs/DATABASE_PLAN.md`. They were probably never a decision about nats-hub itself. |
| `LICENSE` | *(missing)* | Release tarballs pick up any `LICENSE*` file automatically |

**These mention BSL but are not declarations of nats-hub's license.** Leave
them as they are. They describe **SurrealDB's** license and the "Storage trait
as BSL safeguard" design:

- `docs/DATABASE_PLAN.md`
- `docs/PORTABILITY.md` ("BSL note (SurrealDB)")
- `docs/PRODUCT_VISION.md`
- `AGENTS.md` (`SurrealStorage` bullet)
- `src/storage/surreal.rs` (doc comment)

**Dependency note.** The embedded SurrealDB engine (the `surrealdb` crate with
`kv-rocksdb`) is under SurrealDB's BSL-1.1 with an Additional Use Grant. That
constrains what nats-hub may *offer as a service* (see `docs/DATABASE_PLAN.md`)
whichever license nats-hub picks. The `no-storage` build doesn't include it.

## Switch everything in one step

[`packaging/license/set_license.py`](packaging/license/set_license.py) (stdlib Python):

```bash
# preview
python3 packaging/license/set_license.py MIT --dry-run

# apply one of:
python3 packaging/license/set_license.py MIT
python3 packaging/license/set_license.py Apache-2.0
python3 packaging/license/set_license.py BSL-1.1 \
    --change-date 2030-01-01 --change-license Apache-2.0 \
    --additional-use-grant "<the production use you allow, or None>"

git diff && git commit -am "license: <choice>"
```

It makes all of these changes together:

1. Writes `LICENSE` with the full text. MIT is generated with the holder and
   year (`--holder`, default "Agent Memory Labs" from `Cargo.toml` `authors`).
   Apache-2.0 and BUSL-1.1 come from SPDX license-list-data at a pinned tag,
   verified by SHA-256. BUSL-1.1 gets its Parameters block (Licensor, Licensed
   Work, Additional Use Grant, Change Date, Change License).
2. Sets the SPDX id (`MIT`, `Apache-2.0` or `BUSL-1.1`) in `Cargo.toml`, both
   `plugin.json` manifests and `hermes-plugin/plugin.yaml`.
3. Rewrites the README License section between the markers.
4. Deletes this `LICENSE.md`.

It's idempotent, and a dry run writes nothing. It is tested against a scratch
copy of the tree in `tests/python/test_license_switch.py`.

### Manual checklist (if you'd rather not run the script)

- [ ] Add `LICENSE` with the full text. For BUSL-1.1, fill in the Parameters block.
- [ ] `Cargo.toml`: `license = "<SPDX id>"`
- [ ] `claude-code-plugin/.claude-plugin/plugin.json` and `codex-plugin/.codex-plugin/plugin.json`: `"license": "<SPDX id>"`
- [ ] `hermes-plugin/plugin.yaml`: `license: <SPDX id>`
- [ ] `README.md`: License section
- [ ] Delete `LICENSE.md`
- [ ] Optional: cut a release so the tarballs ship the `LICENSE` (see `docs/RELEASING.md`)
