export function openModal() {
  document.getElementById('modal-backdrop')?.classList.add('visible');
}

export function closeModal() {
  S.openLogAgent = null;
  document.getElementById('modal-backdrop')?.classList.remove('visible');
  const detail = document.getElementById('session-detail');
  if (detail) detail.classList.remove('open');
}

import { S, agents, envelopes, optimisticChatHtml, STATUS_LABELS } from './state.js';
import { classifyChatRole, extractEnvelopeBody, prettySnippet } from './model.js';
import { showToast, agentPalette } from './state.js';
import * as Dock from './dock.js';
export function refreshOpenLogIfNeeded(env) {
  if (!S.openLogAgent) return;
  const detail = document.getElementById('session-detail');
  if (!detail || !detail.classList.contains('open')) return;
  const id = S.openLogAgent.identity;
  const from = env.meta && env.meta.from;
  const to = env.meta && env.meta.to;
  if (from === id || to === id) {
    // Real bus traffic supersedes optimistic duplicates for same text
    renderSessionDetail(S.openLogAgent);
  }
}

export function agentEnvelopesFor(agent) {
  return envelopes.filter(e =>
    (e.meta && e.meta.from === agent.identity) ||
    (e.meta && e.meta.to === agent.identity)
  ).slice(-120);
}

export function renderSessionDetail(agent) {
  const title = document.getElementById('detail-title');
  const sub = document.getElementById('detail-sub');
  const box = document.getElementById('chat-messages');
  const input = document.getElementById('chat-input');
  if (!title || !box) return;

  title.textContent = agent.identity;
  const statusLabel = agent.stopped ? 'STOPPED' : (STATUS_LABELS[agent.status] || agent.status || 'ready');
  if (sub) {
    sub.textContent =
      `${statusLabel}` +
      (agent.providerId ? ` · ${agent.providerId}` : '') +
      (agent.model ? ` · ${agent.model}` : '') +
      (agent.sessionId ? ` · ses ${agent.sessionId.substring(0, 8)}` : '') +
      ` · ${agent.messageCount || 0} bus msgs` +
      (agent.petSlug ? ` · variant ${agent.petSlug}` : '');
  }
  if (input) input.placeholder = `Message ${agent.identity}…`;
  const modelSel = document.getElementById('chat-model-select');
  const modelCustom = document.getElementById('chat-model-custom');
  if (modelSel && document.activeElement !== modelSel && document.activeElement !== modelCustom) {
    if (Dock.fillModelPicker) {
      Dock.fillModelPicker(
        'chat-model-select',
        'chat-model-custom',
        agent.providerId || '',
        agent.model || ''
      );
    }
  }

  const agentEnvelopes = agentEnvelopesFor(agent);
  if (!agentEnvelopes.length && !optimisticChatHtml.length) {
    box.innerHTML =
      `<div class="chat-empty"><strong>No messages yet</strong>` +
      `Type below to task this worker — replies stream here live.</div>`;
    return;
  }

  const history = agentEnvelopes.map(e => formatChatBubble(e, agent.identity)).join('');
  box.innerHTML = history + optimisticChatHtml.join('');
  box.scrollTop = box.scrollHeight;
}

export function showSessionDetail(agent) {
  S.openLogAgent = agent;
  optimisticChatHtml.length = 0;
  const detail = document.getElementById('session-detail');
  renderSessionDetail(agent);
  openModal();
  detail.classList.add('open');
  const input = document.getElementById('chat-input');
  if (input) {
    input.value = '';
    updateChatSendEnabled();
    setTimeout(() => input.focus(), 40);
  }
}

// Agent dock lives in agent-dock.js (provider → name+pet → floor → chat)
export function refreshAgentRoster() {
  if (true) Dock.refresh();
}
export function setAgentRosterOpen(open) {
  if (true) Dock.setOpen(open);
}
export function toggleAgentRoster() {
  Dock.setOpen(!Dock.isOpen());
}
export function ensureAgent(identity) {
  if (true) return Dock.ensureAgentOnFloor(identity);
  return agents.get(identity) || null;
}
export function openChatForIdentity(identity) {
  if (true) {
    const a = Dock.ensureAgentOnFloor(identity);
    if (!a) return;
    S.selectedAgent = a;
    Dock.setOpen(false);
    showSessionDetail(a);
  }
}

export function updateChatSendEnabled() {
  const input = document.getElementById('chat-input');
  const btn = document.getElementById('chat-send');
  if (!input || !btn) return;
  btn.disabled = !input.value.trim();
}

export function sendChatMessage() {
  const input = document.getElementById('chat-input');
  if (!input || !S.openLogAgent) return;
  const msg = input.value.trim();
  if (!msg) return;

  if (!S.ws || S.ws.readyState !== WebSocket.OPEN) {
    showToast('WebSocket not connected', true);
    return;
  }

  const agent = S.openLogAgent;
  const provider = agent.providerId || null;
  const model = (agent.model || '').trim() || null;
  if (provider && !agent.workerEnsured) {
    showToast(`Spawning ${provider}${model ? ' · ' + model : ''} for ${agent.identity}…`);
  }
  const payload = {
    type: 'send_message',
    to: agent.identity,
    message: msg,
    provider: provider,
    ensure_worker: true,
  };
  if (model) payload.model = model;
  S.ws.send(JSON.stringify(payload));
  agent.status = 'working';
  agent.statusHoldUntil = Date.now() + 8000;
  agent.setSnippet(
    (provider ? `[${provider}] ` : '') +
    (model ? `${model} · ` : '') +
    'task → ' + msg
  );
  if (provider) agent.workerEnsured = true;

  optimisticChatHtml.push(formatOptimisticYouBubble(msg));
  if (optimisticChatHtml.length > 20) optimisticChatHtml.shift();
  input.value = '';
  updateChatSendEnabled();
  renderSessionDetail(agent);
  showToast(`Sent to ${agent.identity}`);
}

export function persistAgentModel(identity, model) {
  if (!Dock.loadInstances || !Dock.saveInstances) return;
  try {
    const list = Dock.loadInstances();
    let found = false;
    for (const inst of list) {
      if (inst.identity === identity) {
        inst.model = model || '';
        found = true;
        break;
      }
    }
    if (!found) {
      list.push({
        identity,
        providerId: S.openLogAgent && S.openLogAgent.providerId,
        model: model || '',
        createdAt: Date.now(),
      });
    }
    Dock.saveInstances(list);
  } catch (_) { /* ignore localStorage failures */ }
}

export function applyChatModel() {
  if (!S.openLogAgent) {
    showToast('No agent selected', true);
    return;
  }
  const model =
    (Dock.readModelPicker)
      ? Dock.readModelPicker('chat-model-select', 'chat-model-custom')
      : '';
  const agent = S.openLogAgent;
  const prev = agent.model || '';
  agent.model = model;
  persistAgentModel(agent.identity, model);

  // Force next message to re-ensure worker with the new model.
  if (prev !== model) agent.workerEnsured = false;

  if (S.ws && S.ws.readyState === WebSocket.OPEN && agent.providerId && model) {
    // Stop any currently-running worker so the next ensure uses the new model.
    S.ws.send(JSON.stringify({ type: 'stop_agent', identity: agent.identity }));
    S.ws.send(JSON.stringify({
      type: 'ensure_worker',
      identity: agent.identity,
      provider: agent.providerId,
      model,
    }));
    agent.workerEnsured = true;
    agent.status = 'ready';
    agent.statusHoldUntil = Date.now() + 4000;
    agent.setSnippet(`model → ${model}`);
    showToast(`Model set to ${model} (worker restart requested)`);
  } else if (model) {
    showToast(`Model set to ${model}`);
  } else {
    showToast('Model cleared — next spawn uses provider default');
  }
  renderSessionDetail(agent);
  if (true) Dock.refresh();
}

