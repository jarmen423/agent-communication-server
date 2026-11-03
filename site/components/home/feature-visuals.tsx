// Product UI crops for the features bento, built in HTML so they stay crisp,
// themable and real. All of them are decorative (the cell copy says the same
// thing in words), so each is aria-hidden behind a role="img" label.

function Dir({ d }: { d: "sent" | "received" }) {
  return <span className={d === "sent" ? "dir-sent" : "dir-recv"}>{d}</span>;
}

/** The reply contract on one task channel. Source: refocus.md §6, AGENTS.md (hub-delegate). */
export function ReplyContractVisual() {
  const rows = [
    { d: "sent", who: "me → codex-1", kind: "message", body: "{prompt, task_channel}" },
    { d: "received", who: "codex-1", kind: "status", body: "working" },
    { d: "received", who: "codex-1", kind: "event", body: "started" },
    { d: "received", who: "codex-1", kind: "event", body: "progress" },
  ] as const;
  return (
    <div className="viz lane" role="img" aria-label="hub-delegate sends one task to codex-1, three progress envelopes come back on the task channel, then one result, which is printed to stdout.">
      <div aria-hidden="true">
        <div className="viz-head">
          <span className="truncate">
            <span className="text-lane-muted">$ </span>
            <span className="text-lane-ink">hub-delegate --to codex-1 --prompt &quot;Add a changelog entry&quot;</span>
          </span>
        </div>
        <div className="viz-sub">#task.7f3a2c1e · stderr</div>
        <ul className="viz-rows">
          {rows.map((r, i) => (
            <li key={i} className="viz-row">
              <Dir d={r.d} />
              <span>{r.who}</span>
              <span className="text-lane-muted">{r.kind}</span>
              <span className="truncate text-lane-muted">{r.body}</span>
            </li>
          ))}
          <li className="viz-row viz-row-result">
            <Dir d="received" />
            <span>codex-1</span>
            <span className="text-lane-muted">message</span>
            <span className="truncate">{`{"status": "done", "task_id": "…", "result": "…", "error": null}`}</span>
          </li>
        </ul>
        <div className="viz-sub">stdout</div>
        <p className="viz-stdout">Added an entry for the router fix under Unreleased in CHANGELOG.md.</p>
        <div className="viz-foot">
          <span>exit 0 done</span>
          <span>1 error</span>
          <span>2 timeout</span>
          <span>4 cancelled</span>
        </div>
      </div>
    </div>
  );
}

/** A multi-turn session. Source: AGENTS.md (hub-session), docs/WORKER_BACKENDS.md. */
export function SessionVisual() {
  const turns = [
    { d: "sent", text: "Read src/router.rs and list the routing rules." },
    { d: "received", text: "Four rules. DMs go to channel.inbox.<to>…" },
    { d: "sent", text: "Now add a test for the second one." },
  ] as const;
  return (
    <div className="viz-plain" role="img" aria-label="A session: three turns between me and claude-1 on one session channel.">
      <div aria-hidden="true" className="flex flex-col gap-2">
        <span className="viz-caption">#session.4b2e · claude-1 · turn 3</span>
        {turns.map((t, i) => (
          <div key={i} className={`bubble bubble-${t.d}`}>
            <span className="bubble-dir">{t.d === "sent" ? "sent" : "received"}</span>
            {t.text}
          </div>
        ))}
      </div>
    </div>
  );
}

/** A wave: two tasks in parallel, a dependent task, then the merge gate. Source: docs/WAVES.md. */
export function WaveVisual() {
  return (
    <div className="viz-plain" role="img" aria-label="A wave: tasks t1 and t2 run in parallel, t3 waits for both, then the merge gate completes.">
      <svg aria-hidden="true" viewBox="0 0 330 124" className="w-full">
        <g fill="none" stroke="var(--line-strong)" strokeWidth="1.5">
          <path d="M104 31 H138 Q154 31 154 47 V53 Q154 61 162 61 H188" />
          <path d="M104 91 H138 Q154 91 154 75 V69 Q154 61 162 61 H188" />
          <path d="M284 61 H290" />
        </g>
        <WaveNode x={8} y={14} id="t1" scope="src/a" state="done" />
        <WaveNode x={8} y={74} id="t2" scope="src/b" state="done" />
        <WaveNode x={188} y={44} id="t3" scope="docs/" state="running" />
        <g transform="translate(290 47)">
          <rect width="38" height="28" rx="6" fill="var(--surface-raised)" stroke="var(--line-strong)" />
          <text x="19" y="18" textAnchor="middle" className="dg-mono" fontSize="10" fill="var(--ink-muted)">gate</text>
        </g>
      </svg>
    </div>
  );
}

function WaveNode({ x, y, id, scope, state }: { x: number; y: number; id: string; scope: string; state: string }) {
  return (
    <g transform={`translate(${x} ${y})`}>
      <rect width="96" height="34" rx="6" fill="var(--surface-raised)" stroke="var(--line)" />
      <text x="10" y="14" className="dg-mono" fontSize="10.5" fill="var(--ink)">{id}</text>
      <text x="10" y="26" className="dg-mono" fontSize="9" fill="var(--ink-muted)">{scope}</text>
      <text x="88" y="14" textAnchor="end" className="dg-mono" fontSize="9" fill="var(--ink-muted)">{state}</text>
    </g>
  );
}

/** Advisory locks. Source: docs/FILE_LOCKS.md (CLI and exit codes). */
export function LockVisual() {
  return (
    <div className="viz lane" role="img" aria-label="hub-lock acquire is refused because codex-1 already holds src/router.rs.">
      <pre aria-hidden="true" className="viz-pre">
        <span className="text-lane-muted">$ </span>hub-lock acquire --scope repo \{"\n"}
        {"    "}--path src/router.rs{"\n"}
        <span className="dir-recv">conflict</span> src/router.rs{"\n"}
        <span className="text-lane-muted">{"  "}held by codex-1 · 11 min left{"\n"}</span>
        <span className="text-lane-muted">exit 1</span>
      </pre>
    </div>
  );
}

/** Durable delivery. Source: docs/DELIVERY.md stream defaults. */
export function DurableVisual() {
  return (
    <div className="viz-plain" role="img" aria-label="The inbox of an offline agent holds three messages until it reconnects.">
      <div aria-hidden="true">
        <div className="flex items-center justify-between">
          <span className="viz-caption">inbox-box-1 · offline</span>
          <span className="viz-caption">kept 7 d</span>
        </div>
        <div className="mt-3 flex gap-2">
          {[0, 1, 2].map((i) => (
            <span key={i} className="queue-tile" />
          ))}
          <span className="queue-tile queue-tile-empty" />
          <span className="queue-tile queue-tile-empty" />
        </div>
        <p className="viz-caption mt-3">3 waiting · delivered when box-1 wakes</p>
      </div>
    </div>
  );
}

/** History and threads. Source: README.md (hub-history, hub-thread, hub-stats). */
export function HistoryVisual() {
  return (
    <div className="viz lane" role="img" aria-label="hub-history output: recent envelopes with sender, channel and kind.">
      <pre aria-hidden="true" className="viz-pre">
        <span className="text-lane-muted">$ </span>hub-history --tail{"\n"}
        <span className="text-lane-muted">12:04:07 </span>me <span className="text-lane-muted">#inbox.claude-1</span> message{"\n"}
        <span className="text-lane-muted">12:04:07 </span>claude-1 <span className="text-lane-muted">#task.7f3a</span> status{"\n"}
        <span className="text-lane-muted">12:04:41 </span>claude-1 <span className="text-lane-muted">#task.7f3a</span> message
      </pre>
    </div>
  );
}
