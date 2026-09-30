# T4 — MCP orchestrator surface to 100%

Branch `iter2/t4-mcp` (from `main`). Also read `_COMMON.md`.

## Deliverables (acceptance criteria: §2, "Orchestrator surface (MCP)")
1. **`cancel_task(task_id)`**, per contract §4.2. It returns the terminal snapshot, with state `cancelled`. Test it against a fake worker that honors cancel. The real worker support lands in T3, so write the tests to the contract.
2. **A real `check_providers`**, which is the provider health check from `TODO.md`:
   - Lists live workers (from the agent registry, `alive_within`) with their capabilities and models.
   - Optionally runs a cheap liveness ping per worker (`ping: true`): a tiny delegate with a short timeout.
   - Reports ok, slow or unresponsive, and doesn't hang.
   - Document what it can and can't verify (for example, API credits can't be checked from the hub).
3. **Plugin install-layout test in CI:** a new `.github/workflows/plugins.yml` plus a pytest. It copies each plugin into a simulated `~/.claude/plugins/cache/<mkt>/nats-hub/0.1.0/` layout (and the Codex equivalent), then starts the MCP server over stdio with `NATS_HUB_IDENTITY` unset. It asserts the right default identity and that `tools/list` returns the tool set. It also runs `sync_plugins.sh --check`.
4. **SKILL** (`mcp_server/skills/nats-hub/SKILL.md`): update the workflow guide for `cancel_task`, `check_providers`, only-new `wait_for_message`, and the bound-identity note (identity comes from env/manifest).
5. **Quality:**
   - Every tool has schema-validated arguments and a test.
   - Error messages are actionable.
   - No stale in-process state. Wave functions are T2's; don't edit them.

## Must not touch
The §4.1 subject strings in `hub_connection.py` (T1), the wave functions (T2), Rust, the worker runtime.
