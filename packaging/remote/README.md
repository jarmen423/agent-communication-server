# packaging/remote — thin install for remote teammates

This directory holds the **remote install bundle**: the smallest set of
files and a one-shot installer that lets a teammate join a nats-hub from
their laptop **without cloning the whole monorepo** (no Rust toolchain,
no SurrealDB, no full Hermes venv).

## What's here

| File | Purpose |
|---|---|
| `FILES.txt` | Explicit list of repo files a remote machine needs. |
| `install.sh` | Idempotent bootstrap: venv + pip + copy files into a target dir. |
| `README.md` | This file. |
| (repo root) `requirements-remote.txt` | Minimal pip requirements (just `nats-py`). |

## Two ways to install on a laptop

### Option A — clone the repo (simplest if they have access)

```bash
git clone <nats-hub-repo-url> nats-hub
cd nats-hub
bash packaging/remote/install.sh            # → ~/nats-hub-remote
# or: bash packaging/remote/install.sh /custom/target/dir
```

### Option B — sparse checkout (no full clone)

If the repo is large or the teammate shouldn't see `src/`, `deploy/`, etc.:

```bash
git clone --no-checkout --filter=blob:none <nats-hub-repo-url> nats-hub
cd nats-hub
git sparse-checkout init --cone
git sparse-checkout set packaging/remote worker_backends
git checkout

# Files needed that live at the repo root:
git checkout HEAD -- remote_agent_adapter.py nats_connect.py \
                     worker_runtime.py worker_events.py \
                     requirements-remote.txt

bash packaging/remote/install.sh
```

### Option C — copy the bundle by hand (air-gapped machines)

Copy these files to the target machine into a single directory:

- everything listed in [`FILES.txt`](./FILES.txt)
- `packaging/remote/install.sh`
- `packaging/remote/FILES.txt` (for reference)

Then run `bash install.sh` from that directory — the script detects when
it's been copied flat (next to the adapter) and works the same way.

## What `install.sh` does

1. Verifies Python 3.10+.
2. Creates `<target>/.venv` (default target: `~/nats-hub-remote`).
3. Copies the files listed in `FILES.txt` into `<target>/`.
4. Runs `pip install -r requirements-remote.txt` inside the venv.
5. Prints the next-step command line (with `--nats-url wss://…` and a
   placeholder for your token / creds file).

Re-running it is safe: it refreshes the copied files and re-runs pip.

## What `install.sh` does NOT do

- Does **not** accept or store tokens, passwords, or `.creds` paths. You
  pass the credential at adapter launch time via env vars or CLI flags.
- Does **not** start any daemon or background process.
- Does **not** modify the hub. It only prepares the local machine.

## After install

See [`docs/REMOTE_INSTALL.md`](../../docs/REMOTE_INSTALL.md) for the full
worked examples (shell backend, kilo backend, opencode backend) and the
security checklist. The short version:

```bash
source ~/nats-hub-remote/.venv/bin/activate
cd ~/nats-hub-remote
export NATS_TOKEN='<from-operator>'
python3 remote_agent_adapter.py \
    --identity my-worker-1 \
    --nats-url wss://hub.example.com:8080 \
    --ca-file ~/hub-ca.crt \
    --backend shell \
    --execute "my-agent-cli --prompt"
```

## Updating the bundle when the repo changes

If `remote_agent_adapter.py`, `nats_connect.py`, `worker_runtime.py`,
`worker_events.py`, or anything under `worker_backends/` changes:

1. Re-run `bash packaging/remote/install.sh` from a fresh checkout — it
   will overwrite the older copies in the target dir and re-pip-install.
2. If new files were added to the import closure, add them to both
   `FILES.txt` and the `REMOTE_FILES=( ... )` array in `install.sh`.

The fastest way to detect drift is to grep imports:

```bash
grep -nE '^(from|import) ' remote_agent_adapter.py worker_runtime.py \
    worker_events.py nats_connect.py worker_backends/__init__.py \
    worker_backends/headless_cli.py worker_backends/presets.py
```

Anything that is not from the Python stdlib or `nats*` belongs in
`FILES.txt` and `requirements-remote.txt`.
