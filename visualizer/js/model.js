import { S, envelopes, optimisticChatHtml } from './state.js';
import { extractEnvelopeBody, prettySnippet } from './state.js';
import { escapeHtml } from './state.js';
export const OPERATOR_SENDERS = new Set([
  'josh', 'operator', 'human', 'you', 'arcade', 'hub', 'ui', 'nats-hub', 'broadcast',
]);

export function isOperatorSender(from) {
  if (!from) return false;
  const f = String(from).toLowerCase();
  if (OPERATOR_SENDERS.has(f)) return true;
  return f.startsWith('operator') || f.startsWith('human');
}

/** Classify a bus envelope into chat roles: you | agent | system */
export function classifyChatRole(env, agentId) {
  const kind = (env.meta && env.meta.kind) || '';
  const from = (env.meta && env.meta.from) || '';
  const to = (env.meta && env.meta.to) || '';
  const et = env.payload && env.payload.event_type;

  if (kind === 'event' || kind === 'status' || et) {
    // Agent completed results with body → agent bubble
    if (et === 'completed' && from === agentId) {
      const body = extractEnvelopeBody(env.payload || {});
      if (body && body.length > 0 && body !== 'completed' && body !== et) {
        return { role: 'agent', label: agentId, tone: '' };
      }
    }
    let tone = 'progress';
    if (et === 'error' || (kind === 'status' && String(env.payload && env.payload.status).toLowerCase() === 'error')) {
      tone = 'error';
    } else if (et === 'completed' || et === 'started') {
      tone = '';
    }
    return { role: 'system', label: et || kind || 'system', tone };
  }

  if (from === agentId) return { role: 'agent', label: agentId, tone: '' };
  if (to === agentId && isOperatorSender(from)) return { role: 'you', label: 'You', tone: '' };
  if (to === agentId) return { role: 'you', label: from || 'You', tone: '' };
  if (isOperatorSender(from)) return { role: 'you', label: 'You', tone: '' };
  return { role: 'agent', label: from || agentId, tone: '' };
}

export function formatChatBubble(e, agentId) {
  const time = new Date(e._time || Date.now()).toLocaleTimeString([], {
    hour: '2-digit', minute: '2-digit', second: '2-digit',
  });
  const { role, label, tone } = classifyChatRole(e, agentId);
  const raw = extractEnvelopeBody(e.payload || {});
  let body = prettySnippet(String(raw || '')).trim();
  if (!body) {
    const kind = (e.meta && e.meta.kind) || '';
    const et = e.payload && e.payload.event_type;
    body = et || kind || '(empty)';
  }
  if (body.length > 1200) body = body.slice(0, 1197) + '…';
  const toneClass = tone ? ` ${tone}` : '';
  return (
    `<div class="chat-row ${role}${toneClass}">` +
      `<div class="chat-meta"><span class="who">${escapeHtml(label)}</span><span>${time}</span></div>` +
      `<div class="chat-bubble">${escapeHtml(body)}</div>` +
    `</div>`
  );
}

export function formatOptimisticYouBubble(text) {
  const time = new Date().toLocaleTimeString([], {
    hour: '2-digit', minute: '2-digit', second: '2-digit',
  });
  return (
    `<div class="chat-row you">` +
      `<div class="chat-meta"><span class="who">You</span><span>${time}</span></div>` +
      `<div class="chat-bubble">${escapeHtml(text)}</div>` +
    `</div>`
  );
}

export function envelopeToAgentEvent(env) {
  const meta = env.meta || {};
  const payload = env.payload || {};
  const kind = (meta.kind || '').toLowerCase();

  // Extract agent identity from envelope
  let identity = meta.from;
  let eventType = null;
  let snippet = null;
  let status = null;
  let statusHoldMs = 0;

  // Event envelopes carry structured progress
  if (kind === 'event') {
    eventType = payload.event_type;
    const data = payload.data || {};
    snippet = data.result || data.message || data.text || data.error || data.prompt || data.delta || null;
    if (eventType === 'started') { status = 'working'; statusHoldMs = 2500; }
    else if (eventType === 'progress') { status = 'thinking'; statusHoldMs = 8000; }
    else if (eventType === 'completed') { status = 'working'; statusHoldMs = 1800; }
    else if (eventType === 'error') { status = 'error'; statusHoldMs = 3500; }
    else if (eventType === 'milestone') { status = 'thinking'; statusHoldMs = 2500; }
  }
  // Status envelopes
  else if (kind === 'status') {
    const s = payload.status;
    if (s === 'ready') { status = 'ready'; statusHoldMs = 2000; }
    else if (s === 'working') { status = 'working'; statusHoldMs = 3000; }
    else if (s === 'idle') { status = 'idle'; statusHoldMs = 0; }
    else if (s === 'error') { status = 'error'; statusHoldMs = 3500; }
    else if (s === 'closed') { status = 'idle'; statusHoldMs = 0; }
  }
  // Message envelopes (results)
  else if (kind === 'message') {
    snippet = payload.result || payload.error || payload.message || payload.prompt || null;
    if (payload.result) { status = 'working'; statusHoldMs = 2200; }
    else if (payload.error) { status = 'error'; statusHoldMs = 3500; }
    else if (payload.prompt || payload.message) { status = 'thinking'; statusHoldMs = 2200; }
  }

  const channel = meta.channel || '';
  let sessionId = null;
  if (channel.includes('session.')) {
    sessionId = channel.split('session.')[1]?.split('.')[0];
  } else if (channel.includes('.task.')) {
    sessionId = channel.split('.task.')[1]?.split('.')[0];
  } else if (channel.startsWith('task.')) {
    sessionId = channel.split('task.')[1]?.split('.')[0];
  }

  return {
    identity,
    eventType,
    snippet: prettySnippet(snippet),
    status,
    statusHoldMs,
    sessionId,
    channel,
    kind,
    payload,
    meta,
  };
}

// === Agent Class ===
