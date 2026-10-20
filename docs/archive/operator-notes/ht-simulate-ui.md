nats-server -p 4222

hub-server with WS + static visualizer on 127.0.0.1:9191

CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats --db-path /data/cargo-targets/jfrie/nats/debug/hub-server --db-path nats_hub.db --ws-addr 127.0.0.1:9191 --static-dir visualizer/

## **fake agents = echo workers with real-looking names**

```bash
python3 echo_worker.py --identity cursor-agent
python3 echo_worker.py --identity agy-agent
python3 echo_worker.y --identity hermes-agent
```

Each one:
1. connects to NATS as that identity
2. listens on `channel.inbox.<identity>`
3. when a task arrives, runs this "brain":
```python
def echo_run(prompt, ctx):
  return f"echo: {prompt[::-1]}", ctx
```
(reverses the prompt text in the reply)

## **task spam = pretend orchestrator**
Run `hub-delegate` in a loop:
```bash
hub-delegate --to <random-of-the-3> --from josh --prompt "Continuous task...analyze and report"
```
This is real hub protocol:
- creates `task.<uuid>`
- DMs the worker inbox
- waits for reply

## **Visualizer = spectator**
Browser at `http://127.0.0.1:9191/`:
- hub-server mirrors bus envelopes over websocket
- p5 draws squares labeled by identity
- thought bubbles show result snippets 
- connection lines = 'recently active'