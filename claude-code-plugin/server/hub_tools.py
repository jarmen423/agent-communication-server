"""nats-hub MCP — tool schemas.

Identity is always stamped from ``NATS_HUB_IDENTITY`` (env), so no tool takes
a ``from`` argument. (`from` still appears in ``get_history`` as a *filter*,
and `orchestrator` in ``list_sessions`` — those query stored records, they
don't assert identity.)
"""

from mcp.types import Tool

from hub_query_tools import QUERY_TOOLS

TOOLS = [
    # ── Discovery ───────────────────────────────────────────────
    Tool(
        name="list_agents",
        description=(
            "List agents registered on the nats-hub bus. Optionally filter by "
            "capabilities or liveness window. Returns identity, capabilities, "
            "and last_seen for each agent."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "capability": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Only return agents with ALL these capabilities",
                },
                "alive_within_secs": {
                    "type": "integer",
                    "description": "Only return agents seen within this many seconds",
                },
                "limit": {"type": "integer", "description": "Max number of results"},
            },
        },
    ),
    Tool(
        name="get_agent",
        description="Get details for a single agent by identity.",
        inputSchema={
            "type": "object",
            "required": ["identity"],
            "properties": {
                "identity": {"type": "string", "description": "Agent identity string"},
            },
        },
    ),
    Tool(
        name="check_providers",
        description=(
            "Provider/worker health check (TODO.md). Lists agents alive on the "
            "bus within alive_within_secs (their registered capabilities and "
            "metadata, including advertised models), and optionally probes "
            "each named provider's live model list via the worker supervisor "
            "(hub.worker.models request). Honest scope: verifies bus presence "
            "and supervisor responses only — cannot verify provider auth or "
            "that a CLI will actually run."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "providers": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Provider names to probe for live model lists (optional)",
                },
                "alive_within_secs": {
                    "type": "integer",
                    "description": "Liveness window (default 120)",
                    "default": 120,
                },
            },
        },
    ),
    # ── Messaging ───────────────────────────────────────────────
    Tool(
        name="send_message",
        description=(
            "Broadcast a message on a channel. All subscribers on that channel "
            "will receive it. Sender identity is stamped from NATS_HUB_IDENTITY."
        ),
        inputSchema={
            "type": "object",
            "required": ["channel", "message"],
            "properties": {
                "channel": {"type": "string", "description": "Channel name (e.g. 'agents.broadcast')"},
                "message": {"type": "string", "description": "Message text to send"},
            },
        },
    ),
    Tool(
        name="send_direct",
        description=(
            "Send a direct message (DM) to a specific agent via inbox routing. "
            "Only the recipient agent sees it."
        ),
        inputSchema={
            "type": "object",
            "required": ["to", "message"],
            "properties": {
                "to": {"type": "string", "description": "Recipient agent identity"},
                "message": {"type": "string", "description": "Message text"},
                "channel": {
                    "type": "string",
                    "description": "Channel context (default: 'dm')",
                    "default": "dm",
                },
            },
        },
    ),
    Tool(
        name="send_status",
        description="Send a status update on a channel (e.g. 'working', 'idle', 'ready').",
        inputSchema={
            "type": "object",
            "required": ["channel", "status"],
            "properties": {
                "channel": {"type": "string"},
                "status": {"type": "string", "description": "Status text (e.g. 'working', 'idle')"},
            },
        },
    ),
    Tool(
        name="read_inbox",
        description=(
            "Read recent direct messages addressed to this orchestrator. The "
            "server subscribes to channel.inbox.<NATS_HUB_IDENTITY> at connect "
            "time and keeps a bounded buffer. Returns items with seq numbers; "
            "pass since_seq to get only newer messages."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "description": "Max items (default 50)", "default": 50},
                "since_seq": {
                    "type": "integer",
                    "description": "Only items with seq > since_seq (0 = buffered tail)",
                    "default": 0,
                },
            },
        },
    ),
    Tool(
        name="wait_for_message",
        description=(
            "Block until a DM arrives in this orchestrator's inbox (optionally "
            "from a specific sender), or timeout. Checks already-buffered "
            "messages first (seq > since_seq), then waits for a new arrival."
        ),
        inputSchema={
            "type": "object",
            "required": ["timeout"],
            "properties": {
                "timeout": {"type": "number", "description": "Seconds to wait"},
                "from": {
                    "type": "string",
                    "description": "Only match messages from this sender (filter, not identity)",
                },
                "since_seq": {
                    "type": "integer",
                    "description": "Skip inbox items at or below this seq",
                    "default": 0,
                },
            },
        },
    ),
    # ── Task delegation ─────────────────────────────────────────
    Tool(
        name="delegate_async",
        description=(
            "Delegate a task to a worker without blocking. Subscribes to the "
            "task channel before sending (reply contract), buffers progress "
            "events and the terminal result in-process, and returns "
            "{task_id, task_channel} immediately. Follow with task_status or "
            "wait_for_task."
        ),
        inputSchema={
            "type": "object",
            "required": ["to", "prompt"],
            "properties": {
                "to": {"type": "string", "description": "Worker agent identity"},
                "prompt": {"type": "string", "description": "Task/prompt to send"},
            },
        },
    ),
    Tool(
        name="delegate_task",
        description=(
            "Delegate a task and block until the worker's terminal result "
            "arrives (or timeout). Equivalent to delegate_async + wait_for_task. "
            "Ignores status/event progress envelopes; returns only the real "
            "result envelope (kind=message correlated to the task id)."
        ),
        inputSchema={
            "type": "object",
            "required": ["to", "prompt"],
            "properties": {
                "to": {"type": "string", "description": "Worker agent identity"},
                "prompt": {"type": "string", "description": "Task/prompt to send"},
                "timeout": {"type": "integer", "description": "Timeout in seconds", "default": 120},
            },
        },
    ),
    Tool(
        name="task_status",
        description=(
            "Snapshot of a task started via delegate_async/delegate_task: "
            "{state (running|done|error), last_status, recent events, "
            "result|error payload when finished}."
        ),
        inputSchema={
            "type": "object",
            "required": ["task_id"],
            "properties": {
                "task_id": {"type": "string", "description": "task_id from delegate_async"},
                "events_tail": {"type": "integer", "description": "Max recent events", "default": 10},
            },
        },
    ),
    Tool(
        name="wait_for_task",
        description=(
            "Block until a delegated task's terminal result arrives, or "
            "timeout seconds pass. Returns the task snapshot including "
            "result/error payload."
        ),
        inputSchema={
            "type": "object",
            "required": ["task_id", "timeout"],
            "properties": {
                "task_id": {"type": "string", "description": "task_id from delegate_async"},
                "timeout": {"type": "number", "description": "Seconds to wait"},
            },
        },
    ),
    # ── Sessions ────────────────────────────────────────────────
    Tool(
        name="start_session",
        description=(
            "Start a stateful multi-turn session with a worker agent. Sends "
            "session_start to the worker's inbox and returns the session ID. "
            "Use send_to_session for follow-ups, session_replies to read what "
            "the worker says back, and close_session when done."
        ),
        inputSchema={
            "type": "object",
            "required": ["worker", "prompt"],
            "properties": {
                "worker": {"type": "string", "description": "Worker agent identity"},
                "prompt": {"type": "string", "description": "Initial prompt for the worker"},
                "model": {"type": "string", "description": "Model to use (optional)"},
                "provider": {"type": "string", "description": "Provider to use (optional)"},
                "cwd": {"type": "string", "description": "Working directory (optional)"},
                "timeout": {"type": "integer", "description": "Timeout in seconds", "default": 30},
            },
        },
    ),
    Tool(
        name="send_to_session",
        description="Send a follow-up message on an existing session channel.",
        inputSchema={
            "type": "object",
            "required": ["session_id", "message"],
            "properties": {
                "session_id": {"type": "string"},
                "message": {"type": "string"},
            },
        },
    ),
    Tool(
        name="close_session",
        description="Close a session (sends session_close on the session channel).",
        inputSchema={
            "type": "object",
            "required": ["session_id"],
            "properties": {
                "session_id": {"type": "string"},
            },
        },
    ),
    Tool(
        name="session_replies",
        description=(
            "Read messages the worker has published on a session channel. The "
            "server lazily subscribes to channel.session.<id> and buffers "
            "envelopes; returns items with seq numbers — pass since_seq to "
            "follow along without re-reading."
        ),
        inputSchema={
            "type": "object",
            "required": ["session_id"],
            "properties": {
                "session_id": {"type": "string"},
                "since_seq": {"type": "integer", "description": "Only items with seq > since_seq", "default": 0},
                "limit": {"type": "integer", "description": "Max items (default 50)", "default": 50},
            },
        },
    ),
] + QUERY_TOOLS
