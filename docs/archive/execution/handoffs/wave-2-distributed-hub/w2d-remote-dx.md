# W2-D handoff — remote-machine install DX

**Wave / task:** wave-2-distributed-hub / W2-D
**Scope:** Thin install bundle so a teammate can join the hub from a laptop
without cloning the whole monorepo. Packaging, requirements, docs.
**Status:** ✅ Complete; verified end-to-end against a clean target dir.

## Files written

All paths relative to repo root `/home/jfrie/nats`.

| Path | Purpose |
|---|---|
| `requirements-remote.txt` | Minimal pip requirements — just `nats-py>=2.6.0`. 28 lines incl. rationale. |
| `packaging/remote/FILES.txt` | Explicit list of repo files a remote needs (9 files). Documents what's excluded and why. |
| `packaging/remote/install.sh` | Idempotent bootstrap: venv + pip + copy files into a target dir (default `~/nats-hub-remote`). 175 lines. |
| `packaging/remote/README.md` | Operator-facing: three ways to install (full clone, sparse checkout, air-gapped copy), how to update, how to detect drift. |
| `docs/REMOTE_INSTALL.md` | Operator+user facing: when to ship thin vs full clone, quick start, worked examples (shell + kilo), auth cheat sheet, 7-point security checklist, troubleshooting. ~270 lines. |

**Not touched (per MUST NOT TOUCH):** `src/`, `deploy/`, `nats_connect.py`
logic, discord, dogfood scripts. W2-A's auth flags on the adapter are
documented but not modified.

## How the file set was derived

Empirically, not by reading. In a clean interpreter:

```python
import remote_agent_adapter
import sys
sorted(m for m in sys.modules if 'worker' in m or m.startswith('nats'))
```

returned exactly:

```
nats, nats.aio.*, nats.errors, nats.js.*, nats.nuid, nats.protocol.*
nats_connect
remote_agent_adapter
worker_backends, worker_backends.headless_cli, worker_backends.sdk_agent
worker_events
worker_runtime
```

So the only third-party package in the import closure of the
shell/kilo/opencode backends is `nats-py`. The ACP-over-HTTP backends
(`acp_http.py`, `kilo_acp.py`, `hermes_acp.py`, `opencode_acp.py`,
`grok_acp.py`) would pull in `httpx`, but the adapter never instantiates
them at module import — they're lazily imported only if a future named
backend is added. `FILES.txt` and `requirements-remote.txt` both call
this out explicitly so the next maintainer knows what to change if they
wire one in.

## Verification performed

| Check | Result |
|---|---|
| `bash -n packaging/remote/install.sh` | ✅ syntax valid |
| `test -f requirements-remote.txt` | ✅ exists (28 lines) |
| `head docs/REMOTE_INSTALL.md` | ✅ H1 + intro render correctly |
| `bash packaging/remote/install.sh /tmp/w2d-test` (clean run) | ✅ venv created, 9 files copied, pip install succeeded, adapter `--help` runs in fresh venv |
| Idempotent re-run | ✅ "Reusing existing venv", files refreshed, no errors |
| Adapter imports from target dir using its own venv | ✅ `backends known: ['kilo', 'opencode', 'shell']` |
| Drift check: grep imports vs FILES.txt | ✅ all non-stdlib, non-`nats*` imports are in FILES.txt |

## Key design decisions

### Why `install.sh` and not a wheel / setup.py

The thin package is 9 flat files plus a single pip dep. A wheel would
require a `pyproject.toml` (which the repo doesn't have), a build step,
and a package layout that doesn't match the current flat-file
convention. A shell script that copies files into a target dir is the
smallest possible artifact, is readable by anyone, and survives
air-gapped installs (just `scp` the files). The tradeoff — no automatic
dependency resolution beyond pip — is fine because there's only one
dependency.

### Why the script never accepts credentials

Security rule #1 in `docs/REMOTE_INSTALL.md`: never bake a token into a
script. `install.sh` has no `--token`, `--password`, `--creds` flags.
Credentials are supplied at adapter launch time via env vars or CLI
flags. This is also called out in a comment at the top of the script.

### Why `requirements-remote.txt` lives at the repo root

Two reasons: (a) it's the conventional location that `pip install -r`
users will look for first, and (b) it's referenced from multiple places
(`packaging/remote/README.md`, `packaging/remote/install.sh`,
`docs/REMOTE_INSTALL.md`). Putting it at the root means those
references don't have to explain a non-standard path.

### Drift detection

`packaging/remote/README.md` includes a one-liner `grep` command that
lists every import in the closure. The rule: anything that is not
stdlib or `nats*` belongs in `FILES.txt` and `requirements-remote.txt`.
This is the cheapest possible drift alarm.

## Residual risks

1. **Drift between `FILES.txt` and `install.sh`'s `REMOTE_FILES` array.**
   The two lists are kept in sync manually. If a new file joins the
   import closure, both must be updated. The README documents this;
   there is no automation. Mitigation: the drift grep is one command.

2. **ACP-over-HTTP backends not covered.** If someone wires a kilo-acp
   or opencode-acp backend into `remote_agent_adapter.build_backend()`,
   they will need to add `httpx` to `requirements-remote.txt` and the
   ACP files to `FILES.txt`. This is called out in three places
   (FILES.txt, requirements-remote.txt, REMOTE_INSTALL.md) but is not
   enforced.

3. **`install.sh` resolves the repo root by walking up to 5 parents.**
   This works for the standard layouts (run from `packaging/remote/`,
   run from repo root, run from a flat dir of copied files). It would
   fail if the script were symlinked from more than 5 levels deep —
   unlikely but possible. Easy fix if it ever bites: bump the loop
   counter.

4. **Sparse-checkout instructions in README may rot if the repo moves
   files around.** The example uses `git sparse-checkout set
   packaging/remote worker_backends` and a `git checkout HEAD --` for
   the root-level files. If any of those paths change, the instructions
   need updating. No automation; this is inherent to documenting a
   manual procedure.

5. **No version pinning of `nats-py` beyond a lower bound.**
   `requirements-remote.txt` says `nats-py>=2.6.0`. A future breaking
   release in the 2.x series could break the adapter. Mitigation: the
   adapter only uses the stable `nats.connect()` + `nc.publish()` +
   `nc.subscribe()` API surface, which has been stable since 2.x.

6. **Docs cross-link to `OPERATOR_HUB.md` which is operator-facing.**
   Some links from REMOTE_INSTALL.md point at OPERATOR_HUB for
   completeness, but the teammate role doesn't need to read it. The
   "See also" section labels it clearly.

## Out of scope (deliberately)

- No git commit (per task instructions).
- No background daemon, no systemd unit, no hub-side changes.
- No ACP-over-HTTP backend packaging (called out as a future-work hook).
- No wheel / `pyproject.toml` (current convention is flat files).
- No CI integration for drift detection (manual grep documented).

## Pointers for the next maintainer

- To add a new backend to the bundle: add the file to `FILES.txt`, add
  the path to `REMOTE_FILES=( ... )` in `install.sh`, add any new pip
  dep to `requirements-remote.txt`, and re-run the installer to confirm.
- To change the default target dir: edit `TARGET_DIR=` at the top of
  `install.sh`.
- To add a new auth flag to the bundle docs: update the auth cheat
  sheet table in `docs/REMOTE_INSTALL.md` and the flag matrix in
  `docs/REMOTE_AGENTS.md` (W2-A owns the latter — coordinate).
