"""Provider-agnostic model catalog for nats-hub workers.

Lists models for any configured provider by consulting the agent CLI/ACP
surface that provider already exposes — not a hard-coded UI table.

Sources (in priority order for a provider):
  1. config/provider_models.json  (operator overrides / future providers)
  2. built-in registry in this module
  3. empty list + reason (UI still offers Provider default + Other…)

CLI parse strategies:
  - plain_ids:       one model id per stdout line  (kilo models, opencode models)
  - id_dash_label:   "id - Human Label" lines     (cursor agent models)
  - plain_lines:     any non-empty line is both id and label
  - static:          models supplied in config/registry (no CLI)

API:
  list_models(provider) -> dict
  list_provider_catalog() -> dict  # all known providers + source metadata
"""
from __future__ import annotations

import asyncio
import json
import logging
import os
import shutil
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

log = logging.getLogger("nats_hub.model_catalog")

REPO = Path(__file__).resolve().parent.parent
DEFAULT_CONFIG_PATH = REPO / "config" / "provider_models.json"
CACHE_TTL_SEC = float(os.environ.get("NATS_HUB_MODEL_CACHE_TTL", "60"))

# Built-in registry: every known supervisor provider maps to a model source.
# Future providers: add here OR put an entry in config/provider_models.json.
BUILTIN_SOURCES: dict[str, dict[str, Any]] = {
    "kilo": {
        "kind": "cli",
        "cmd": ["kilo", "models"],
        "parser": "plain_ids",
        "label": "Kilo CLI",
    },
    "kilo-acp": {
        "kind": "cli",
        "cmd": ["kilo", "models"],
        "parser": "plain_ids",
        "label": "Kilo CLI (ACP HTTP backend)",
    },
    "opencode": {
        "kind": "cli",
        "cmd": ["opencode", "models"],
        "parser": "plain_ids",
        "label": "OpenCode CLI",
    },
    "opencode-acp": {
        "kind": "cli",
        "cmd": ["opencode", "models"],
        "parser": "plain_ids",
        "label": "OpenCode CLI (ACP stdio backend)",
    },
    "cursor": {
        "kind": "cli",
        "cmd": ["agent", "models"],  # cursor-agent is often linked as `agent`
        "fallback_cmd": ["cursor-agent", "models"],
        "parser": "id_dash_label",
        "label": "Cursor Agent CLI",
    },
    "agy": {
        "kind": "cli",
        "cmd": ["agy", "models"],
        "parser": "plain_lines",
        "label": "Antigravity CLI",
    },
    # Interactive / no stable machine-readable list yet
    "hermes": {
        "kind": "none",
        "reason": "hermes model picker is interactive (hermes model); use Other… or set default in Hermes config",
        "label": "Hermes",
    },
    "grok": {
        "kind": "none",
        "reason": "grok CLI has no stable models subcommand yet; pass model via Other…",
        "label": "Grok",
    },
    "claude": {
        "kind": "none",
        "reason": "claude worker not wired; use Other… when a backend lands",
        "label": "Claude Code",
    },
    "codex": {
        "kind": "none",
        "reason": "codex worker not wired with a listable models command yet",
        "label": "Codex",
    },
    "echo": {
        "kind": "static",
        "models": [],
        "reason": "echo worker ignores models",
        "label": "Echo",
    },
}

_cache: dict[str, tuple[float, dict[str, Any]]] = {}


@dataclass(frozen=True)
class ModelInfo:
    value: str
    label: str

    def as_dict(self) -> dict[str, str]:
        return {"value": self.value, "label": self.label}


def _load_config(path: Path | None = None) -> dict[str, Any]:
    cfg_path = path or Path(os.environ.get("NATS_HUB_PROVIDER_MODELS", DEFAULT_CONFIG_PATH))
    if not cfg_path.is_file():
        return {}
    try:
        data = json.loads(cfg_path.read_text(encoding="utf-8"))
        return data if isinstance(data, dict) else {}
    except Exception as e:
        log.warning("failed to load %s: %s", cfg_path, e)
        return {}


def resolve_source(provider: str) -> dict[str, Any] | None:
    """Merge config override over built-in source for one provider."""
    provider = (provider or "").strip().lower()
    if not provider:
        return None
    cfg = _load_config()
    overrides_raw = cfg.get("providers")
    overrides: dict[str, Any] = overrides_raw if isinstance(overrides_raw, dict) else {}
    base = dict(BUILTIN_SOURCES.get(provider) or {})
    override_raw = overrides.get(provider)
    override: dict[str, Any] = override_raw if isinstance(override_raw, dict) else {}
    if not base and not override:
        # Unknown provider: still allow config-only registration for future backends
        return {
            "kind": "none",
            "reason": f"unknown provider {provider!r} — add it to config/provider_models.json or BUILTIN_SOURCES",
            "label": provider,
        }
    if override:
        base.update(override)
    return base


def _parse_plain_ids(stdout: str) -> list[ModelInfo]:
    out: list[ModelInfo] = []
    seen: set[str] = set()
    for line in stdout.splitlines():
        s = line.strip()
        if not s or s.startswith("{") or s.startswith("["):
            continue
        # skip log/noise
        if s.lower().startswith("info ") or s.lower().startswith("error"):
            continue
        if s in seen:
            continue
        seen.add(s)
        out.append(ModelInfo(value=s, label=s))
    return out


def _parse_id_dash_label(stdout: str) -> list[ModelInfo]:
    out: list[ModelInfo] = []
    seen: set[str] = set()
    for line in stdout.splitlines():
        s = line.strip()
        if not s:
            continue
        low = s.lower()
        if low.startswith("available models") or low.startswith("usage:"):
            continue
        if " - " in s:
            value, label = s.split(" - ", 1)
            value, label = value.strip(), label.strip()
        else:
            value, label = s, s
        if not value or value in seen:
            continue
        seen.add(value)
        out.append(ModelInfo(value=value, label=label or value))
    return out


def _parse_plain_lines(stdout: str) -> list[ModelInfo]:
    return _parse_plain_ids(stdout)


_PARSERS = {
    "plain_ids": _parse_plain_ids,
    "id_dash_label": _parse_id_dash_label,
    "plain_lines": _parse_plain_lines,
}


async def _run_cmd(cmd: list[str], timeout: float = 25.0) -> tuple[int, str, str]:
    if not cmd:
        return 127, "", "empty command"
    binary = cmd[0]
    if not shutil.which(binary) and not Path(binary).exists():
        return 127, "", f"binary not found: {binary}"
    try:
        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            stdin=asyncio.subprocess.DEVNULL,
        )
        try:
            stdout_b, stderr_b = await asyncio.wait_for(proc.communicate(), timeout=timeout)
        except asyncio.TimeoutError:
            proc.kill()
            await proc.wait()
            return 124, "", f"timeout after {timeout}s: {' '.join(cmd)}"
        return (
            proc.returncode or 0,
            stdout_b.decode("utf-8", errors="replace"),
            stderr_b.decode("utf-8", errors="replace"),
        )
    except Exception as e:
        return 1, "", str(e)


async def list_models(provider: str, *, use_cache: bool = True) -> dict[str, Any]:
    """Return {ok, provider, models:[{value,label}], source, reason?, error?}."""
    provider = (provider or "").strip().lower()
    if not provider:
        return {"ok": False, "error": "provider required", "models": []}

    now = time.time()
    if use_cache and provider in _cache:
        ts, payload = _cache[provider]
        if now - ts < CACHE_TTL_SEC:
            return dict(payload)

    source = resolve_source(provider) or {}
    kind = (source.get("kind") or "none").lower()
    label = source.get("label") or provider

    result: dict[str, Any] = {
        "ok": True,
        "provider": provider,
        "label": label,
        "source": kind,
        "models": [],
    }

    if kind == "static":
        raw = source.get("models") or []
        models: list[ModelInfo] = []
        for item in raw:
            if isinstance(item, str):
                models.append(ModelInfo(value=item, label=item))
            elif isinstance(item, dict) and item.get("value"):
                models.append(
                    ModelInfo(
                        value=str(item["value"]),
                        label=str(item.get("label") or item["value"]),
                    )
                )
        result["models"] = [m.as_dict() for m in models]
        if source.get("reason"):
            result["reason"] = source["reason"]
    elif kind == "cli":
        cmds: list[list[str]] = []
        if isinstance(source.get("cmd"), list) and source["cmd"]:
            cmds.append([str(x) for x in source["cmd"]])
        if isinstance(source.get("fallback_cmd"), list) and source["fallback_cmd"]:
            cmds.append([str(x) for x in source["fallback_cmd"]])
        parser_name = source.get("parser") or "plain_ids"
        parser = _PARSERS.get(parser_name, _parse_plain_ids)
        last_err = "no command configured"
        models = []
        used_cmd: list[str] | None = None
        for cmd in cmds:
            code, out, err = await _run_cmd(cmd)
            if code == 0 and out.strip():
                models = parser(out)
                used_cmd = cmd
                last_err = ""
                break
            last_err = err.strip() or f"exit {code}"
            # some CLIs write models to stderr; try that too
            if code == 0 and err.strip():
                models = parser(err)
                if models:
                    used_cmd = cmd
                    last_err = ""
                    break
        result["models"] = [m.as_dict() for m in models]
        result["cmd"] = used_cmd
        if last_err and not models:
            result["ok"] = False
            result["error"] = last_err
            result["reason"] = f"CLI model list failed for {provider}"
        elif not models:
            result["reason"] = source.get("reason") or "CLI returned no models"
    else:
        result["source"] = "none"
        result["reason"] = source.get("reason") or "no model list source configured"
        # ok=true with empty models: UI can still show Provider default + Other
        result["ok"] = True

    _cache[provider] = (now, dict(result))
    return result


def list_provider_catalog() -> dict[str, Any]:
    """Describe every known provider model source (no CLI execution)."""
    cfg = _load_config()
    overrides_raw = cfg.get("providers")
    overrides: dict[str, Any] = overrides_raw if isinstance(overrides_raw, dict) else {}
    ids = sorted(set(BUILTIN_SOURCES) | set(overrides.keys()))
    providers = []
    for pid in ids:
        src = resolve_source(pid) or {}
        providers.append(
            {
                "id": pid,
                "label": src.get("label") or pid,
                "kind": src.get("kind") or "none",
                "reason": src.get("reason"),
                "cmd": src.get("cmd"),
            }
        )
    return {"ok": True, "providers": providers, "config_path": str(DEFAULT_CONFIG_PATH)}


def clear_cache(provider: str | None = None) -> None:
    if provider:
        _cache.pop(provider.strip().lower(), None)
    else:
        _cache.clear()


__all__ = [
    "ModelInfo",
    "BUILTIN_SOURCES",
    "list_models",
    "list_provider_catalog",
    "resolve_source",
    "clear_cache",
]
