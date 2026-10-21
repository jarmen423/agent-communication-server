import { S, agents, envelopes, transmissions, broadcastRipples, COLORS } from './state.js';
import { envelopeToAgentEvent } from './model.js';
import { Agent } from './agent.js';
import { Transmission, BroadcastRipple, spawnParticles } from './fx.js';
import { placeAgent } from './layout.js';
import { addTickerEvent, setWsBadge } from './panels.js';
import { refreshOpenLogIfNeeded } from './chat.js';
import * as Dock from './dock.js';
import { refreshAgentRoster } from './chat.js';
export function connectWS() {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  let wsUrl = `${proto}//${location.host}/ws`;
  // Forward the bridge token from the page URL (hub-server --ws-token):
  // open http://<host>/?token=T and the WS upgrade carries it as ?token=T.
  const wsToken = new URLSearchParams(location.search).get('token');
  if (wsToken) wsUrl += `?token=${encodeURIComponent(wsToken)}`;
  const endpointEl = document.getElementById('ws-endpoint');
  if (endpointEl) endpointEl.textContent = `${location.host}/ws`;

  S.wsReconnecting = true;
  setWsBadge('reconnecting');

  S.ws = new WebSocket(wsUrl);
  S.ws.onopen = () => {
    S.wsConnectedAt = Date.now();
    S.wsReconnecting = false;
    setWsBadge('connected');
    updateHudClock();
  };
  S.ws.onclose = () => {
    S.wsConnectedAt = 0;
    setWsBadge('disconnected');
    const uptime = document.getElementById('ws-uptime');
    if (uptime) uptime.textContent = 'uptime —';
    setTimeout(connectWS, 2000);
  };
  S.ws.onerror = () => { S.ws.close(); };
  S.ws.onmessage = (event) => {
    try {
      const env = JSON.parse(event.data);
      if (env && env.type === 'ack') {
        let extra = '';
        if (env.ensure && env.ensure.status) {
          extra = ` · worker ${env.ensure.status}`;
        }
        showToast(`✓ ${env.action}${env.to ? ' → ' + env.to : ''}${env.identity ? ' ' + env.identity : ''}${env.task_channel ? ' (' + env.task_channel + ')' : ''}${extra}`);
        return;
      }
      if (env && env.type === 'models') {
        if (Dock.ingestModels) {
          Dock.ingestModels(env.provider, env.detail || {});
        }
        return;
      }
      if (env && env.type === 'error') {
        showToast('✗ ' + (env.message || 'error'), true);
        return;
      }
      if (env && env.type === 'lagged') {
        showToast(`⚠ lagged, skipped ${env.skipped} events`, true);
        return;
      }
      handleEnvelope(env);
    } catch (e) {
      console.error('parse error', e);
    }
  };
}

export function handleEnvelope(env) {
  const evt = envelopeToAgentEvent(env);
  if (!evt.identity) return;

  // Track routed stats in HUD
  S.totalMessageCount++;
  document.getElementById('routed-count').textContent = S.totalMessageCount;

  // Append new event ticker entry
  addTickerEvent(env);

  // Create or update agent
  let agent = agents.get(evt.identity);
  if (!agent) {
    agent = new Agent(evt.identity);
    agents.set(evt.identity, agent);
    placeAgent(agent);
  }

  agent.lastSeen = Date.now();
  agent.messageCount++;

  if (evt.status) {
    agent.status = evt.status;
    if (evt.statusHoldMs > 0) {
      agent.statusHoldUntil = Date.now() + evt.statusHoldMs;
    }
  }
  if (evt.sessionId) agent.sessionId = evt.sessionId;
  if (evt.snippet) agent.setSnippet(evt.snippet);

  // Real communication edges (see EDGE STRATEGY header)
  const channel = evt.channel || '';
  const isTask = channel.startsWith('task.') || channel.includes('.task.');
  if (evt.meta.to && agents.has(evt.meta.to)) {
    transmissions.push(new Transmission(evt.identity, evt.meta.to, evt.kind));
  } else if (evt.sessionId || isTask) {
    const peerKey = evt.sessionId || channel;
    for (const [id, otherAgent] of agents) {
      if (id === evt.identity) continue;
      const sharesSession = evt.sessionId && otherAgent.sessionId === evt.sessionId;
      const sharesTask = isTask && (otherAgent.sessionId === peerKey || otherAgent.lastTaskChannel === peerKey);
      if (sharesSession || sharesTask) {
        transmissions.push(new Transmission(evt.identity, id, evt.kind));
      }
    }
    if (isTask) agent.lastTaskChannel = channel;
  } else {
    broadcastRipples.push(new BroadcastRipple(agent.x, agent.y, agent.color));
  }

  // Particle bursts on events
  if (evt.eventType === 'completed') {
    spawnParticles(agent.x, agent.y, COLORS.agent.working, 25);
  } else if (evt.eventType === 'error') {
    spawnParticles(agent.x, agent.y, COLORS.agent.error, 30);
  } else if (evt.eventType === 'started') {
    spawnParticles(agent.x, agent.y, COLORS.agent.ready, 16);
  } else if (evt.kind === 'message') {
    spawnParticles(agent.x, agent.y, COLORS.agent.thinking, 12);
  }

  // Store envelope for log viewer
  envelopes.push({ ...env, _time: Date.now() });
  if (envelopes.length > 400) envelopes.shift();
  refreshOpenLogIfNeeded(env);

  // Update HUD worker count
  document.getElementById('agent-count').textContent = agents.size;
  refreshAgentRoster();
}

