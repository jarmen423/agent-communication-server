# Wave 1: Remote Agent Connectivity

## Goal

Complete the remaining agent connectivity surfaces after the WebSocket gateway commit (7fbc8ba):
1. ACP HTTP transport abstraction (for distributed workers driving remote agent servers)
2. OpenCode ACP stdio backend
3. Kilo ACP HTTP backend (uses the HTTP transport)
4. TLS + auth on NATS WebSocket config

## Write Scope Analysis

| Task | Owned paths | Collision risk |
|------|-------------|----------------|
| W1-A | `worker_backends/acp_http.py` (NEW) | none — new file |
| W1-B | `worker_backends/opencode_acp.py` (NEW), `opencode_acp_worker.py` (NEW) | none — new files |
| W1-C | `worker_backends/kilo_acp.py` (NEW), `kilo_acp_worker.py` (NEW) | depends on W1-A |
| W1-D | `config/nats-server.conf` (EDIT), `docs/REMOTE_AGENTS.md` (EDIT) | none — isolated files |

**Safe parallel:** W1-A, W1-B, W1-D can run concurrently (all disjoint files).
**Sequential:** W1-C depends on W1-A (it uses the ACP HTTP transport).

## Dependencies

```
W1-A (ACP HTTP transport)  ──────┐
W1-B (OpenCode stdio ACP)         │   W1-C (Kilo HTTP ACP)
W1-D (TLS+auth config)            │      depends on W1-A
                                  │
              wave-1a (parallel)──┘   wave-1b (after W1-A)
```
