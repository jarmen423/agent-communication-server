# Wave 3 — Close W2 residuals + WSS dogfood

## Goal

1. Hub-server / ControlPlane / ApiClient / query API honor `NATS_TOKEN` (and friends).
2. Fix `list_pending` Surreal 2.x graph bug (unblocks analytics pending).
3. Prove `wss://` + CA + token end-to-end.
4. Surface distributed docs from README.

## Tasks

| ID | Work | Status |
|----|------|--------|
| W3-A | `ControlPlane::connect`, `ApiClient::connect`, `start_api_listener` → `connect_with_hub_opts` + `from_env` | done |
| W3-B | `list_pending` field-based filter + `raw_envelope_id` | done |
| W3-C | `scripts/dogfood_wss_tls.sh` parent PASS | done |
| W3-D | README distributed hub section | done |

## Parent verification

```
cargo test --test task_channels test_list_pending_storage  # ok
cargo test --test analytics test_agent_activity            # ok
bash scripts/dogfood_token_auth.sh                         # PASS
bash scripts/dogfood_wss_tls.sh                            # PASS (wss + token + ca)
```
