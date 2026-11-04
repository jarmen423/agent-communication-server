"""What the MCP server reports in ``meta.origin.reported``.

Only values actually known are sent; the hub treats the block as unverified and
applies the caps. Nothing here may fail a send or a hook.

Stdio server, per key, first source that has it:

1. ``NATS_HUB_REPORTED_SESSION`` / ``_MODEL`` / ``_MODEL_PROVIDER``;
2. the MCP ``initialize`` ``clientInfo``, as extra key ``client``
   (``"claude-code 2.1.0"``);
3. the host's session id: the plugin's SessionStart hook writes it to a per-host
   file (:func:`record_session`), re-read per envelope because the hook can run
   after this server starts and again on ``/clear``. Without that file, a Claude
   Code client's ``CLAUDE_CODE_SESSION_ID``, which is fixed when the server is
   spawned. That variable is honoured only when ``clientInfo`` says Claude Code:
   any process started from inside a Claude Code session inherits it.

A multi-identity transport has no process-wide session or model: it scopes each
call with :func:`scoped`, and the environment is not consulted (see
:func:`call_reported`).
"""

from __future__ import annotations

import contextlib
import contextvars
import json
import os
import stat
import subprocess
from pathlib import Path
from typing import Any, AsyncIterator, Iterator

ENV_VARS = {
    "session": "NATS_HUB_REPORTED_SESSION",
    "model": "NATS_HUB_REPORTED_MODEL",
    "model_provider": "NATS_HUB_REPORTED_MODEL_PROVIDER",
}
CLAUDE_CODE_CLIENT = "claude-code"
_SHELLS = {"sh", "bash", "dash", "zsh", "ksh", "ash"}
_MAX_SESSION_FILE_BYTES = 4096

_client: str | None = None
_client_name: str | None = None
_host_pid_cache: int | None = None
_scope: contextvars.ContextVar[dict[str, str] | None] = contextvars.ContextVar(
    "nats_hub_reported_scope", default=None)


def _env_reported() -> dict[str, str]:
    found = {key: os.environ.get(var, "").strip() for key, var in ENV_VARS.items()}
    return {key: value for key, value in found.items() if value}


def process_reported() -> dict[str, str]:
    """The block for the stdio server's envelopes."""
    out: dict[str, str] = {}
    try:
        if _client:
            out["client"] = _client
        session = _host_session()
        if session:
            out["session"] = session
        out.update(_env_reported())
    except Exception:  # noqa: BLE001 - reporting never fails a send
        pass
    return out


def call_reported() -> dict[str, str]:
    """The block for one call of a multi-identity transport."""
    return dict(_scope.get() or {})


@contextlib.contextmanager
def scoped(reported: dict[str, str]) -> Iterator[None]:
    token = _scope.set(dict(reported))
    try:
        yield
    finally:
        _scope.reset(token)


def client_label(info: Any) -> str | None:
    """``clientInfo`` (an object or dict with name and version) as a label."""
    get = info.get if isinstance(info, dict) else lambda key: getattr(info, key, None)
    name, version = get("name"), get("version")
    if not isinstance(name, str) or not name.strip():
        return None
    has_version = isinstance(version, str) and version.strip()
    return f"{name.strip()} {version.strip()}" if has_version else name.strip()


def note_client_info(info: Any) -> None:
    global _client, _client_name
    label = client_label(info)
    if label:
        _client, _client_name = label, label.split(" ", 1)[0]


@contextlib.asynccontextmanager
async def client_info_bind(server: Any) -> AsyncIterator[None]:
    """``hub_app.build_server`` bind: note the client around each tool call."""
    try:
        note_client_info(server.request_context.session.client_params.clientInfo)
    except Exception:  # noqa: BLE001 - no request context, or no initialize seen
        pass
    yield


def _host_session() -> str | None:
    session = read_session()
    if session:
        return session
    if _client_name == CLAUDE_CODE_CLIENT:
        return os.environ.get("CLAUDE_CODE_SESSION_ID", "").strip() or None
    return None


# ── Per-host session file ───────────────────────────────────────────────


def _parent_of(pid: int) -> tuple[int, str] | None:
    """(parent pid, command name) of ``pid``."""
    try:
        text = Path(f"/proc/{pid}/stat").read_text()
        head, _, tail = text.rpartition(")")
        return int(tail.split()[1]), head.split("(", 1)[1]
    except (OSError, ValueError, IndexError):
        pass
    try:
        out = subprocess.run(["ps", "-o", "ppid=,comm=", "-p", str(pid)], capture_output=True,
                             text=True, timeout=1.0, check=False).stdout.split(None, 1)
        return int(out[0]), os.path.basename(out[1].strip())
    except (OSError, ValueError, IndexError, subprocess.SubprocessError):
        return None


def host_pid(start: int | None = None, *, hook: bool = False) -> int:
    """The agent host of this process. A host spawns its MCP server directly
    (the host is the parent) and runs a hook through ``sh -c`` (the host is the
    shell's parent), so ``hook=True`` skips exactly one shell parent. A host that
    is itself a shell stays the key on the server side."""
    pid = os.getppid() if start is None else start
    if hook:
        info = _parent_of(pid)
        if info is not None and info[1] in _SHELLS and info[0] > 1:
            pid = info[0]
    return pid


def _start_time(pid: int) -> str | None:
    """When ``pid`` started (clock ticks since boot, or ``ps`` lstart), so a
    reused pid is not mistaken for the host that wrote a session file."""
    try:
        tail = Path(f"/proc/{pid}/stat").read_text().rpartition(")")[2].split()
        return tail[19]
    except (OSError, IndexError):
        pass
    try:
        out = subprocess.run(["ps", "-o", "lstart=", "-p", str(pid)], capture_output=True,
                             text=True, timeout=1.0, check=False).stdout.strip()
        return out or None
    except (OSError, subprocess.SubprocessError):
        return None


def _session_dir(create: bool = False) -> Path | None:
    """The private directory, or None if it is missing or not safely ours."""
    runtime = os.environ.get("XDG_RUNTIME_DIR", "").strip()
    path = Path(runtime) / "nats-hub" if runtime else Path(f"/tmp/nats-hub-{os.getuid()}")
    if create:
        path.mkdir(mode=0o700, parents=bool(runtime), exist_ok=True)
    try:
        info = path.lstat()
    except OSError:
        return None
    private = stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid() \
        and not info.st_mode & 0o077
    return path if private else None


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except OSError:
        pass
    return True


def _prune(directory: Path) -> None:
    for entry in directory.glob("session-*.json"):
        try:
            os.kill(int(entry.stem.split("-", 1)[1]), 0)
        except ProcessLookupError:
            entry.unlink(missing_ok=True)
        except (ValueError, OSError):
            pass


def record_session(session_id: Any, *, pid: int | None = None) -> bool:
    """Called by the SessionStart hook: note ``session_id`` for this host. Never
    raises. Returns whether the file was written."""
    try:
        if not isinstance(session_id, str) or not session_id.strip():
            return False
        directory = _session_dir(create=True)
        if directory is None:
            return False
        _prune(directory)
        host = host_pid(hook=True) if pid is None else pid
        target = directory / f"session-{host}.json"
        tmp = target.with_name(f".{target.name}.{os.getpid()}")
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "w") as out:
            json.dump({"session_id": session_id.strip(), "host_start": _start_time(host)}, out)
        os.replace(tmp, target)
        return True
    except Exception:  # noqa: BLE001 - a hook must never fail over this
        return False


def read_session(*, pid: int | None = None) -> str | None:
    """The session the hook recorded for this server's host, if any."""
    global _host_pid_cache
    try:
        directory = _session_dir()
        if directory is None:
            return None
        if pid is None:
            if _host_pid_cache is None:
                _host_pid_cache = host_pid()
            pid = _host_pid_cache
        path = directory / f"session-{pid}.json"
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd) as src:
            if os.fstat(src.fileno()).st_uid != os.getuid():
                return None
            data = json.loads(src.read(_MAX_SESSION_FILE_BYTES))
        if not _alive(pid) or data.get("host_start") != _start_time(pid):
            path.unlink(missing_ok=True)  # written by an earlier process with this pid
            return None
        session = data.get("session_id")
        return session.strip() or None if isinstance(session, str) else None
    except Exception:  # noqa: BLE001 - absent or unreadable: leave the key out
        return None
