import { S, agents } from './state.js';
import { showToast } from './state.js';
import { showSessionDetail } from './chat.js';
import { agentPalette, extractEnvelopeBody, prettySnippet, formatUptime, STATUS_LABELS } from './state.js';
export function setWsBadge(state) {
  const badge = document.getElementById('ws-badge');
  const text = document.getElementById('ws-text');
  if (!badge || !text) return;
  badge.className = 'status-badge' + (state === 'connected' ? '' : state === 'reconnecting' ? ' reconnecting' : ' disconnected');
  text.textContent = state === 'connected' ? 'LIVE' : state === 'reconnecting' ? 'RECONNECT' : 'OFFLINE';
}

export function updateHudClock() {
  const el = document.getElementById('hud-clock');
  if (el) el.textContent = new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  const uptime = document.getElementById('ws-uptime');
  if (uptime && S.wsConnectedAt) uptime.textContent = 'uptime ' + formatUptime(Date.now() - S.wsConnectedAt);
}
setInterval(updateHudClock, 1000);

// === Envelope → Agent State Mapping ===

/** Pull human-readable body from a hub envelope payload. */
// Prepend formatted lines to UI side HUD Event ticker
export function addTickerEvent(env) {
  const ticker = document.getElementById('event-ticker');
  if (!ticker) return;

  const time = new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
  const sender = env.meta ? env.meta.from : '?';
  const kind = env.meta ? env.meta.kind : 'message';
  
  // Format description snippet (include nested progress.data.message)
  let raw = extractEnvelopeBody(env.payload || {}) || JSON.stringify(env.payload || {});
  let snippet = prettySnippet(String(raw));
  if (snippet.length > 32) snippet = snippet.substring(0, 30) + '...';

  const entry = document.createElement('div');
  entry.className = `ticker-entry kind-${String(kind).replace(/[^a-z_-]/gi, '')}`;
  
  // Calculate sender's style color
  const palette = agentPalette(sender);
  const colorHex = `rgb(${palette[0]}, ${palette[1]}, ${palette[2]})`;

  // Envelope fields are untrusted (any agent can broadcast): build the
  // entry with textContent only, never innerHTML.
  const span = (cls, text) => {
    const el = document.createElement('span');
    el.className = cls;
    el.textContent = text;
    return el;
  };
  const senderEl = span('ticker-sender', String(sender).substring(0, 12));
  senderEl.style.color = colorHex;
  entry.append(
    span('ticker-time', String(time)),
    span('kind-pill', String(kind)),
    senderEl,
    span('ticker-body', snippet),
  );

  ticker.insertBefore(entry, ticker.firstChild);

  // Keep DOM footprint lightweight and bounded
  while (ticker.children.length > 5) {
    ticker.removeChild(ticker.lastChild);
  }
}

export function showActionPopup(agent) {
  const popup = document.getElementById('action-popup');
  const palette = agentPalette(agent.identity);
  const title = document.getElementById('popup-title');
  title.textContent = agent.identity.toUpperCase();
  title.style.color = `rgb(${palette[0]}, ${palette[1]}, ${palette[2]})`;
  const statusLabel = agent.stopped ? 'STOPPED' : (STATUS_LABELS[agent.status] || agent.status.toUpperCase());
  document.getElementById('popup-info').textContent =
    `${statusLabel}` +
    (agent.providerId ? ` · ${agent.providerId}` : '') +
    (agent.model ? ` · ${agent.model}` : '') +
    ` · ${agent.messageCount} msgs` +
    (agent.petSlug ? ` · variant ${agent.petSlug}` : '') +
    (agent.sessionId ? ` · ses ${agent.sessionId.substring(0, 8)}` : '');

  popup.style.display = 'block';
  popup.style.left = Math.min(mouseX + 12, window.innerWidth - 250) + 'px';
  popup.style.top = Math.min(mouseY + 12, window.innerHeight - 250) + 'px';
}

export function popupAction(action) {
  if (!S.selectedAgent) {
    showToast('No agent selected', true);
    return;
  }
  const agent = S.selectedAgent;

  // Open chat is the primary session surface (no separate prompt/logs paths)
  if (action === 'view' || action === 'message' || action === 'chat') {
    showSessionDetail(agent);
    document.getElementById('action-popup').style.display = 'none';
    return;
  }

  if (!S.ws || S.ws.readyState !== WebSocket.OPEN) {
    showToast('WebSocket not connected', true);
    return;
  }

  if (action === 'stop') {
    S.ws.send(JSON.stringify({ type: 'stop_agent', identity: agent.identity }));
    agent.stopped = true;
    agent.status = 'idle';
    agent.setSnippet('stop requested');
    showToast(`Stop requested for ${agent.identity}`);
  } else if (action === 'resume') {
    S.ws.send(JSON.stringify({ type: 'resume_agent', identity: agent.identity }));
    agent.stopped = false;
    agent.status = 'ready';
    agent.statusHoldUntil = Date.now() + 2500;
    agent.setSnippet('resume requested');
    showToast(`Resume requested for ${agent.identity}`);
  }

  document.getElementById('action-popup').style.display = 'none';
}


