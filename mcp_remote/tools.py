"""Tool list of the remote MCP endpoint.

Shared tools reuse the stdio server's schemas and annotations
(``mcp_server/hub_tools.py``). The remote-only tools are turn-bounded: chat
apps cap one tool call (ChatGPT reportedly at ~60 s, claude.ai at 240 s), so
nothing waits longer than ``WAIT_CAP`` (50 s) and long results are paged.
IDs that end up in NATS subjects are pattern-checked, so a tool argument
can never add subject tokens or wildcards.
"""

from __future__ import annotations

from mcp.types import Tool

from hub_annotations import annotate
from hub_tools import TOOLS as STDIO_TOOLS

WAIT_CAP = 50  # seconds; under ChatGPT's reported ~60 s tool-call limit
PAGE_CHARS = 100_000  # one result page (claude.ai caps a tool result at ~150k)

TASK_ID = {"type": "string", "pattern": "^[A-Za-z0-9-]{8,64}$",
           "description": "task_id returned by delegate"}
WORKER = {"type": "string", "pattern": "^[A-Za-z0-9_-]{1,128}$",
          "description": "the worker the task was delegated to (`worker` from delegate)"}
SESSION_ID = {"type": "string", "pattern": "^[A-Za-z0-9_-]{1,64}$"}

SHARED = (
    "list_agents", "get_agent", "send_message", "send_direct",
    "close_session", "list_sessions", "get_session", "get_history", "get_thread",
    "list_pending", "create_wave", "spawn_wave", "cancel_wave", "wave_status",
    "list_waves", "get_wave", "list_wave_tasks", "get_wave_task",
)

REMOTE_ANNOTATIONS = {
    "delegate": ("Delegate a task", False, False, False),
    "get_task_result": ("Get a task result", True, False, True),
    "cancel_task": ("Cancel a task", False, True, True),
    # Not read-only: it acks the durable inbox consumer, so each message is
    # handed out once. Hosts must not run it without the user's say-so.
    "read_inbox": ("Read inbox (consumes messages)", False, False, False),
    "start_session": ("Start a session", False, False, False),
    "send_to_session": ("Send to a session", False, False, False),
    "session_replies": ("Read session replies", True, False, True),
    "whoami": ("Show my identity", True, False, True),
}

_REMOTE = [
    Tool(
        name="whoami",
        description=("Show the hub identity and tenant this connector acts as. The "
                     "identity comes from your sign-in, never from tool arguments."),
        inputSchema={"type": "object", "properties": {}},
    ),
    Tool(
        name="delegate",
        description=(
            "Delegate a task to a worker agent and return at once with a task_id "
            "(it never waits for the worker). Then call get_task_result(task_id, "
            "worker), in this turn or a later one: results are kept for 24 h."),
        inputSchema={
            "type": "object", "required": ["to", "prompt"],
            "properties": {
                "to": {**WORKER, "description": "worker agent identity (see list_agents)"},
                "prompt": {"type": "string", "minLength": 1, "maxLength": 200_000,
                           "description": "the task"},
            },
        },
    ),
    Tool(
        name="get_task_result",
        description=(
            "Get a delegated task's terminal result, waiting up to wait_secs "
            f"(max {WAIT_CAP}) for it. Returns state running (call again later) or "
            "done/error/cancelled with the result. Long results are paged: pass "
            "next_cursor back as cursor to get the next page."),
        inputSchema={
            "type": "object", "required": ["task_id", "worker"],
            "properties": {
                "task_id": TASK_ID,
                "worker": WORKER,
                "wait_secs": {"type": "number", "minimum": 0, "maximum": WAIT_CAP,
                              "default": 25,
                              "description": f"seconds to wait for the result (0-{WAIT_CAP})"},
                "cursor": {"type": "string", "pattern": "^[0-9]{1,12}$",
                           "description": "next_cursor from the previous page"},
                "max_chars": {"type": "integer", "minimum": 1000, "maximum": PAGE_CHARS,
                              "default": PAGE_CHARS,
                              "description": f"page size in characters (max {PAGE_CHARS})"},
            },
        },
    ),
    Tool(
        name="cancel_task",
        description=(
            "Ask the worker to stop a delegated task. Waits up to wait_secs for "
            "the worker to confirm (state cancelled), then returns the task state."),
        inputSchema={
            "type": "object", "required": ["task_id", "worker"],
            "properties": {
                "task_id": TASK_ID, "worker": WORKER,
                "wait_secs": {"type": "number", "minimum": 0, "maximum": 20, "default": 5},
            },
        },
    ),
    Tool(
        name="read_inbox",
        description=(
            "Read new direct messages sent to this connector's identity, oldest "
            "first. Each message is returned once (the read position is shared by "
            "every turn); messages sent while you were away are kept for 7 days."),
        inputSchema={
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 50, "default": 20},
                "wait_secs": {"type": "number", "minimum": 0, "maximum": WAIT_CAP,
                              "default": 0,
                              "description": "wait this long for a message if none is waiting"},
            },
        },
    ),
    Tool(
        name="start_session",
        description=(
            "Start a multi-turn session with a worker. Returns session_id; follow "
            "up with send_to_session, read replies with session_replies, finish "
            "with close_session."),
        inputSchema={
            "type": "object", "required": ["worker", "prompt"],
            "properties": {
                "worker": {**WORKER, "description": "worker agent identity"},
                "prompt": {"type": "string", "minLength": 1, "maxLength": 200_000},
                "model": {"type": "string"}, "provider": {"type": "string"},
                "cwd": {"type": "string"},
            },
        },
    ),
    Tool(
        name="send_to_session",
        description="Send a follow-up message in a session.",
        inputSchema={
            "type": "object", "required": ["session_id", "message"],
            "properties": {"session_id": SESSION_ID,
                           "message": {"type": "string", "minLength": 1, "maxLength": 200_000}},
        },
    ),
    Tool(
        name="session_replies",
        description=(
            "Read what was said on a session after since_seq (oldest first). Pass "
            "the returned next_seq as since_seq to continue."),
        inputSchema={
            "type": "object", "required": ["session_id"],
            "properties": {
                "session_id": SESSION_ID,
                "since_seq": {"type": "integer", "minimum": 0, "default": 0},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 50},
                "wait_secs": {"type": "number", "minimum": 0, "maximum": WAIT_CAP, "default": 0},
            },
        },
    ),
]


def _shared() -> list[Tool]:
    by_name = {t.name: t for t in STDIO_TOOLS}
    out = []
    for name in SHARED:
        tool = by_name[name]
        desc = (tool.description or "").replace(
            "NATS_HUB_IDENTITY", "your connector identity")
        out.append(tool.model_copy(update={"description": desc}))
    return out


TOOLS: list[Tool] = annotate(_REMOTE, REMOTE_ANNOTATIONS) + _shared()
