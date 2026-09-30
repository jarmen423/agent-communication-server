"""nats-hub MCP — tool schemas: sessions listing, history, waves, analytics.

Split from ``hub_tools`` (file-size cap); assembled into the full ``TOOLS``
list there. Same rules: no ``from``/``orchestrator`` args — identity comes
from ``NATS_HUB_IDENTITY`` (``orchestrator`` survives only as a query filter
in ``list_sessions``).
"""

from mcp.types import Tool

QUERY_TOOLS = [
    Tool(
        name="list_sessions",
        description="List sessions, optionally filtered by status, worker, or orchestrator.",
        inputSchema={
            "type": "object",
            "properties": {
                "status": {"type": "string", "description": "e.g. 'active', 'closed'"},
                "worker": {"type": "string"},
                "orchestrator": {"type": "string", "description": "Filter by orchestrator identity"},
                "limit": {"type": "integer"},
            },
        },
    ),
    Tool(
        name="get_session",
        description="Get a single session by ID.",
        inputSchema={
            "type": "object",
            "required": ["session_id"],
            "properties": {"session_id": {"type": "string"}},
        },
    ),
    # ── History & threads ───────────────────────────────────────
    Tool(
        name="get_history",
        description=(
            "Query message history from the persistent store. Filter by channel, "
            "sender, recipient, message kind, or time range."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "channel": {"type": "string"},
                "from": {"type": "string", "description": "Filter: sender identity"},
                "to": {"type": "string", "description": "Filter: recipient identity"},
                "kind": {"type": "string", "description": "message|status|event|control|human"},
                "since": {"type": "string", "description": "ISO 8601 timestamp"},
                "until": {"type": "string", "description": "ISO 8601 timestamp"},
                "limit": {"type": "integer", "description": "Max results (most recent first)"},
            },
        },
    ),
    Tool(
        name="get_thread",
        description="Get a conversation thread by root message ID.",
        inputSchema={
            "type": "object",
            "required": ["root_id"],
            "properties": {"root_id": {"type": "string", "description": "Root envelope ID"}},
        },
    ),
    Tool(
        name="list_pending",
        description="List unanswered (pending) messages for an agent.",
        inputSchema={
            "type": "object",
            "required": ["identity"],
            "properties": {"identity": {"type": "string", "description": "Agent identity"}},
        },
    ),
    # ── Waves ───────────────────────────────────────────────────
    Tool(
        name="create_wave",
        description=(
            "Create a wave — a group of parallel tasks with disjoint write "
            "scopes. Provide a goal and a list of tasks; each task needs "
            "{worker, goal} and may add write_scope, dependencies, verify_cmd, "
            "handoff_path, task_id. Returns the wave_id. The wave is only "
            "registered — call spawn_wave to actually dispatch its tasks."
        ),
        inputSchema={
            "type": "object",
            "required": ["goal", "tasks"],
            "properties": {
                "goal": {"type": "string", "description": "High-level goal for the wave"},
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["worker", "goal"],
                        "properties": {
                            "task_id": {"type": "string", "description": "Stable id for dependencies (auto if omitted)"},
                            "worker": {"type": "string"},
                            "goal": {"type": "string"},
                            "write_scope": {"type": "array", "items": {"type": "string"}},
                            "dependencies": {"type": "array", "items": {"type": "string"}, "description": "task_ids that must finish first"},
                            "verify_cmd": {"type": "string"},
                            "handoff_path": {"type": "string"},
                        },
                    },
                    "description": "Tasks in this wave",
                },
            },
        },
    ),
    Tool(
        name="spawn_wave",
        description=(
            "Spawn a wave created by create_wave: marks it running, DMs each "
            "ready task's worker a session_start on its wave task channel "
            "(respecting dependencies), and drives the wave to "
            "completed/failed in the background. Watch progress with "
            "list_wave_tasks / get_wave."
        ),
        inputSchema={
            "type": "object",
            "required": ["wave_id"],
            "properties": {
                "wave_id": {"type": "string"},
                "timeout": {"type": "integer", "description": "Overall wave timeout secs", "default": 3600},
            },
        },
    ),
    Tool(
        name="list_waves",
        description="List waves, optionally filtered by status.",
        inputSchema={
            "type": "object",
            "properties": {
                "status": {"type": "string", "description": "e.g. 'active', 'completed'"},
            },
        },
    ),
    Tool(
        name="get_wave",
        description="Get a wave by ID.",
        inputSchema={
            "type": "object",
            "required": ["wave_id"],
            "properties": {"wave_id": {"type": "string"}},
        },
    ),
    Tool(
        name="list_wave_tasks",
        description="List tasks in a wave.",
        inputSchema={
            "type": "object",
            "required": ["wave_id"],
            "properties": {"wave_id": {"type": "string"}},
        },
    ),
    Tool(
        name="get_wave_task",
        description="Get a single task within a wave.",
        inputSchema={
            "type": "object",
            "required": ["wave_id", "task_id"],
            "properties": {
                "wave_id": {"type": "string"},
                "task_id": {"type": "string"},
            },
        },
    ),
    # ── Analytics ───────────────────────────────────────────────
    Tool(
        name="get_analytics",
        description=(
            "Get analytics: message rate, latency stats, agent activity, "
            "channel hotspots, or error rate. Specify which metric via 'metric' param."
        ),
        inputSchema={
            "type": "object",
            "required": ["metric"],
            "properties": {
                "metric": {
                    "type": "string",
                    "enum": ["message_rate", "latency", "agent_activity", "channel_hotspots", "error_rate"],
                    "description": "Which analytics metric to fetch",
                },
                "secs": {"type": "integer", "description": "Time window in seconds (default 3600)", "default": 3600},
                "interval": {"type": "string", "description": "Bucket interval: minute|hour|day", "default": "hour"},
                "channel": {"type": "string", "description": "Filter by channel (latency only)"},
                "identity": {"type": "string", "description": "Agent identity (agent_activity only)"},
                "limit": {"type": "integer", "description": "Max results (channel_hotspots only)", "default": 10},
            },
        },
    ),
]
