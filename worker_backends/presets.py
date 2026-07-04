"""One-line presets for common headless CLIs."""

from __future__ import annotations

from pathlib import Path

from worker_backends.headless_cli import HeadlessCliSpec


def agy_spec(
    *,
    agy: str = "agy",
    repo: str | Path = ".",
    model: str | None = None,
    print_timeout: str | None = None,
) -> HeadlessCliSpec:
    base: list[str] = []
    if model:
        base.extend(["--model", model])
    if print_timeout:
        base.extend(["--print-timeout", print_timeout])
    return HeadlessCliSpec(
        binary=agy,
        log_label="agy-worker",
        repo=repo,
        base_argv=base,
        prompt_flag="-p",
        resume_mode="session_cwd_continue",
        session_cwd_subdir=".nats-hub/agy-sessions",
        has_turn_ctx_key="agy_has_turn",
    )


def hermes_spec(
    *,
    repo: str | Path = ".",
    model: str | None = None,
    provider: str | None = None,
    toolsets: str | None = None,
    skills: str | None = None,
    max_turns: int = 15,
) -> HeadlessCliSpec:
    base = ["chat", "-Q", "--max-turns", str(max_turns), "--pass-session-id"]
    if model:
        base.extend(["-m", model])
    if provider:
        base.extend(["--provider", provider])
    if toolsets:
        base.extend(["-t", toolsets])
    if skills:
        base.extend(["-s", skills])
    return HeadlessCliSpec(
        binary="hermes",
        log_label="hermes-worker",
        repo=repo,
        base_argv=base,
        prompt_flag="-q",
        resume_mode="resume_id",
        resume_ctx_key="hermes_session_id",
        strip_line_prefixes=("session_id:", "Warning:", "⚠️"),
    )