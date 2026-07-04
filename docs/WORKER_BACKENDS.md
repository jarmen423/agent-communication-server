# New CLI worker in ~15 lines of backend + run_worker()

```python
from worker_runtime import WorkerConfig, run_worker

class MyBackend:
    async def run(self, prompt: str, ctx: dict) -> tuple[str, dict]:
        # ctx: per hub-session state (_session_id set by runtime on session_start)
        # oneshot: ctx is {}
        text = await my_cli(prompt, resume=ctx.get("my_handle"))
        ctx["my_handle"] = ...
        return text, ctx

await run_worker(WorkerConfig(identity="my-worker-1", backend=MyBackend(), log_prefix="my-worker"))
```

`worker_runtime` handles: inbox routing, hub-delegate one-shot, hub-session start/send/close, status/message envelopes, heartbeat.

See `agy_worker.py` (agy `--continue` per session cwd) and `hermes_worker.py` (`hermes chat --resume`).