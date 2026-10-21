"""nats-hub MCP — tool titles and behaviour hints (MCP ``ToolAnnotations``).

Every tool gets a human-readable ``title`` and explicit ``readOnlyHint`` /
``destructiveHint`` / ``idempotentHint`` / ``openWorldHint`` values. Chat
hosts use them: ChatGPT skips the confirmation prompt only for read-only
tools, and Claude's connector directory requires a title and the hints on
every tool. ``mcp_remote/`` adds entries for its own tools.

Hints, per the MCP spec: read-only tools don't change anything; a
destructive tool may undo or stop work (cancel); an idempotent tool can be
repeated with the same arguments without extra effect; open-world tools
reach agents/processes outside this server (every bus tool does).
"""

from __future__ import annotations

from mcp.types import Tool, ToolAnnotations

# name → (title, read_only, destructive, idempotent)
ANNOTATIONS: dict[str, tuple[str, bool, bool, bool]] = {
    "list_agents": ("List agents", True, False, True),
    "get_agent": ("Get agent", True, False, True),
    "whoami": ("Show my identity", True, False, True),
    "check_providers": ("Check worker health", False, False, False),
    "send_message": ("Broadcast a message", False, False, False),
    "send_direct": ("Send a direct message", False, False, False),
    "send_status": ("Send a status update", False, False, False),
    "read_inbox": ("Read inbox", True, False, True),
    "wait_for_message": ("Wait for a message", True, False, True),
    "delegate_async": ("Delegate a task (async)", False, False, False),
    "delegate_task": ("Delegate a task and wait", False, False, False),
    "task_status": ("Task status", True, False, True),
    "wait_for_task": ("Wait for a task result", True, False, True),
    "cancel_task": ("Cancel a task", False, True, True),
    "start_session": ("Start a session", False, False, False),
    "send_to_session": ("Send to a session", False, False, False),
    "close_session": ("Close a session", False, True, True),
    "session_replies": ("Read session replies", True, False, True),
    "list_sessions": ("List sessions", True, False, True),
    "get_session": ("Get session", True, False, True),
    "get_history": ("Message history", True, False, True),
    "get_thread": ("Get thread", True, False, True),
    "list_pending": ("Unanswered messages", True, False, True),
    "create_wave": ("Create a wave", False, False, False),
    "spawn_wave": ("Start a wave", False, False, False),
    "cancel_wave": ("Cancel a wave", False, True, True),
    "wave_status": ("Wave status", True, False, True),
    "list_waves": ("List waves", True, False, True),
    "get_wave": ("Get wave", True, False, True),
    "list_wave_tasks": ("List wave tasks", True, False, True),
    "get_wave_task": ("Get wave task", True, False, True),
    "get_analytics": ("Hub analytics", True, False, True),
    "lock_files": ("Lock files (advisory)", False, False, False),
    "release_files": ("Release file locks", False, False, True),
    "renew_lock": ("Renew a file lock", False, False, True),
    "list_locks": ("List file locks", True, False, True),
    "wait_for_files": ("Wait until files are free", True, False, True),
}

# Tools that never touch the bus.
_LOCAL_ONLY = frozenset({"whoami"})


def annotate(tools: list[Tool],
             extra: dict[str, tuple[str, bool, bool, bool]] | None = None) -> list[Tool]:
    """Copies of ``tools`` with ``title`` and ``annotations`` set. Every tool
    must have an entry (a missing one is a programming error)."""
    table = {**ANNOTATIONS, **(extra or {})}
    out = []
    for tool in tools:
        if tool.name not in table:
            raise RuntimeError(f"tool {tool.name!r} has no annotations entry")
        title, read_only, destructive, idempotent = table[tool.name]
        out.append(tool.model_copy(update={
            "title": title,
            "annotations": ToolAnnotations(
                title=title,
                readOnlyHint=read_only,
                destructiveHint=destructive,
                idempotentHint=idempotent,
                openWorldHint=tool.name not in _LOCAL_ONLY,
            ),
        }))
    return out
