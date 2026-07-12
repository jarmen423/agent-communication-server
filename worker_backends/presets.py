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
        # agy's --print-timeout expects a Go-style duration (e.g. "90s"), not a
        # bare integer. Normalize "90" -> "90s".
        pt = str(print_timeout)
        if pt and not pt.endswith(("s", "m", "h")):
            pt = pt + "s"
        base.extend(["--print-timeout", pt])
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


def grok_spec(
    *,
    repo: str | Path = ".",
    model: str | None = None,
    max_turns: int = 40,
    always_approve: bool = True,
    grok_bin: str | None = None,
) -> HeadlessCliSpec:
    """Headless Grok CLI (`grok -p`) — prefer GrokAcpBackend for multi-turn ACP sessions."""
    from worker_backends.grok_acp import resolve_grok_bin

    base: list[str] = []
    if always_approve:
        base.append("--always-approve")
    base.extend(["--max-turns", str(max_turns), "--no-auto-update"])
    if model:
        base.extend(["-m", model])
    return HeadlessCliSpec(
        binary=grok_bin or resolve_grok_bin(),
        log_label="grok-worker",
        repo=repo,
        base_argv=base,
        prompt_flag="-p",
        resume_mode="none",
    )


def kilo_spec(
    *,
    repo: str | Path = ".",
    model: str | None = None,
    kilo_bin: str = "kilo",
    auto_approve: bool = True,
    json_output: bool = True,
) -> HeadlessCliSpec:
    """Kilo CLI (`kilo run`) headless preset.

    Session resume: kilo supports `--continue` (last session) and `--session <id>`.
    We use session_id mode so each hub-session maps to a kilo session.
    """
    base: list[str] = ["run"]
    if auto_approve:
        base.append("--auto")
    if json_output:
        base.extend(["--format", "json"])
    if model:
        base.extend(["-m", model])
    return HeadlessCliSpec(
        binary=kilo_bin,
        log_label="kilo-worker",
        repo=repo,
        base_argv=base,
        prompt_flag=None,  # kilo uses positional message
        resume_mode="resume_id",
        resume_id_flag="--session",
        resume_ctx_key="kilo_session_id",
        json_events=True,  # parse NDJSON --format json output
        timeout_sec=600.0,
    )


def opencode_spec(
    *,
    repo: str | Path = ".",
    model: str | None = None,
    opencode_bin: str = "opencode",
) -> HeadlessCliSpec:
    """OpenCode CLI (`opencode run`) headless preset.

    OpenCode supports `opencode run <message>` for one-shot and session
    resume via `--session` / `--continue`.
    """
    base: list[str] = ["run", "--format", "json"]
    if model:
        base.extend(["-m", model])
    return HeadlessCliSpec(
        binary=opencode_bin,
        log_label="opencode-worker",
        repo=repo,
        base_argv=base,
        prompt_flag=None,  # positional message
        resume_mode="resume_id",
        resume_id_flag="--session",
        resume_ctx_key="opencode_session_id",
        json_events=True,  # parse NDJSON --format json output
        timeout_sec=600.0,
    )

