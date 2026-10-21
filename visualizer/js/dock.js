import { S, agents, STATUS_LABELS } from './state.js';
import { escapeHtml, showToast } from './state.js';
import { Agent } from './agent.js';
import { placeAgent } from './layout.js';
import { layoutAgents } from './scene.js';
import { Theme } from './theme.js';
import { render, updateChrome } from './dock-view.js';
import { showSessionDetail } from './chat.js';
import { modelsForProvider, defaultModelForProvider, requestModels, ingestModels, modelOptionsHtml, readModelPicker, syncModelCustomVisibility, fillModelPicker } from './dock-models.js';
export { ingestModels, fillModelPicker, readModelPicker, syncModelCustomVisibility, requestModels, modelsForProvider, defaultModelForProvider, modelOptionsHtml };
// Agent dock — provider-first spawn wizard (provider -> name+variant -> floor -> chat).
// Instances persist in localStorage nats-hub.instances.
export const dockState = { view: 'home', selectedProviderId: null, selectedPetSlug: null, wired: false };
export const LS_PROVIDERS = 'nats-hub.providers';
export const LS_INSTANCES = 'nats-hub.instances';

export const DEFAULT_PROVIDERS = [
    { id: 'cursor', label: 'Cursor', monogram: 'Cu', color: '#00ff88', blurb: 'Cursor agent CLI', backend: 'headless', defaultModel: null },
    { id: 'claude', label: 'Claude Code', monogram: 'CC', color: '#d4a27f', blurb: 'Anthropic Claude Code', backend: 'headless', defaultModel: null },
    { id: 'codex', label: 'Codex', monogram: 'Cx', color: '#a78bfa', blurb: 'OpenAI Codex CLI', backend: 'headless', defaultModel: null },
    { id: 'agy', label: 'Antigravity', monogram: 'AG', color: '#38bdf8', blurb: 'agy headless agent', backend: 'headless', defaultModel: null },
    { id: 'grok', label: 'Grok', monogram: 'Gk', color: '#00d9ff', blurb: 'Grok ACP stdio', backend: 'acp', defaultModel: 'grok-4.5' },
    { id: 'hermes', label: 'Hermes', monogram: 'He', color: '#f472b6', blurb: 'Hermes ACP / headless', backend: 'acp', defaultModel: null },
    { id: 'kilo', label: 'Kilo', monogram: 'Ki', color: '#ff9f43', blurb: 'Kilo CLI (kilo/minimax/minimax-m3)', backend: 'headless', defaultModel: 'kilo/minimax/minimax-m3' },
    { id: 'kilo-acp', label: 'Kilo ACP', monogram: 'KA', color: '#ff6b6b', blurb: 'Kilo ACP HTTP server', backend: 'acp', defaultModel: 'kilo/minimax/minimax-m3' },
    { id: 'opencode', label: 'OpenCode', monogram: 'OC', color: '#48dbfb', blurb: 'OpenCode CLI (opencode/deepseek-v4-flash-free)', backend: 'headless', defaultModel: 'opencode/deepseek-v4-flash-free' },
    { id: 'opencode-acp', label: 'OpenCode ACP', monogram: 'OA', color: '#54a0ff', blurb: 'OpenCode ACP stdio', backend: 'acp', defaultModel: 'opencode/deepseek-v4-flash-free' },
    { id: 'echo', label: 'Echo', monogram: 'Ec', color: '#94a3b8', blurb: 'Dogfood reverse worker', backend: 'sdk', defaultModel: null },
  ];

export function esc(s) {
    if (escapeHtml) return escapeHtml(s);
    return String(s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

export function providers() {
    let list = DEFAULT_PROVIDERS.slice();
    try {
      const raw = localStorage.getItem(LS_PROVIDERS);
      if (!raw) return list;
      const enabled = JSON.parse(raw);
      if (!Array.isArray(enabled) || !enabled.length) return list;
      const set = new Set(enabled.map(String));
      const filtered = DEFAULT_PROVIDERS.filter((p) => set.has(p.id));
      return filtered.length ? filtered : list;
    } catch (_) {
      return list;
    }
  }

export function loadInstances() {
    try {
      const raw = localStorage.getItem(LS_INSTANCES);
      if (!raw) return [];
      const arr = JSON.parse(raw);
      return Array.isArray(arr) ? arr.filter((i) => i && i.identity) : [];
    } catch (_) {
      return [];
    }
  }

export function saveInstances(list) {
    try {
      localStorage.setItem(LS_INSTANCES, JSON.stringify(list));
    } catch (_) { /* quota */ }
  }

export function providerById(id) {
    return DEFAULT_PROVIDERS.find((p) => p.id === id) || null;
  }

export function slugifyName(name) {
    return String(name || '')
      .trim()
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .slice(0, 40);
  }

export function makeIdentity(name) {
    let base = slugifyName(name);
    if (!base) base = 'agent';
    if (!base.endsWith('-agent') && !base.includes('worker')) base = base + '-agent';
    let id = base;
    let n = 2;
    const taken = new Set(loadInstances().map((i) => i.identity));
    if (true) {
      for (const k of agents.keys()) taken.add(k);
    }
    while (taken.has(id)) id = `${base}-${n++}`;
    return id;
  }

export function ensureAgentOnFloor(identity, opts = {}) {
    if (typeof agents === 'undefined' || false) return null;
    let agent = agents.get(identity);
    const created = !agent;
    if (!agent) {
      agent = new Agent(identity);
      agent.status = 'idle';
      agent.lastSeen = Date.now();
      agents.set(identity, agent);
    }
    if (opts.petSlug) {
      agent.petSlug = opts.petSlug;
      if (true && Theme.setExplicitVariant) {
        Theme.setExplicitVariant(identity, opts.petSlug);
      }
    }
    if (opts.providerId) agent.providerId = opts.providerId;
    if (opts.label) agent.displayLabel = opts.label;
    if (opts.model) agent.model = opts.model;
    if (created && true) placeAgent(agent);
    else if (true) layoutAgents();
    const countEl = document.getElementById('agent-count');
    if (countEl) countEl.textContent = String(agents.size);
    return agent;
  }

export function hydrateInstancesOntoFloor() {
    for (const inst of loadInstances()) {
      ensureAgentOnFloor(inst.identity, {
        petSlug: inst.petSlug,
        providerId: inst.providerId,
        label: inst.label,
        model: inst.model,
      });
    }
  }

export function setOpen(open) {
    const roster = document.getElementById('agent-roster');
    const toggleBtn = document.getElementById('agent-dock-toggle');
    if (!roster || !toggleBtn) return;
    roster.classList.toggle('open', open);
    roster.setAttribute('aria-hidden', open ? 'false' : 'true');
    toggleBtn.setAttribute('aria-expanded', open ? 'true' : 'false');
    if (open) {
      dockState.view = 'home';
      dockState.selectedProviderId = null;
      dockState.selectedPetSlug = null;
      render();
    }
  }

export function isOpen() {
    const roster = document.getElementById('agent-roster');
    return !!(roster && roster.classList.contains('open'));
  }

export function openChat(identity) {
    const agent = ensureAgentOnFloor(identity);
    if (!agent) {
      if (showToast) showToast('Could not open agent', true);
      return;
    }
    try {
      S.selectedAgent = agent;
    } catch (_) { /* non-writable */ }
    setOpen(false);
    if (showSessionDetail) showSessionDetail(agent);
    if (showToast) showToast(`Chat · ${identity}`);
  }

export function spawnFromForm() {
    const p = providerById(dockState.selectedProviderId);
    if (!p) return;
    const input = document.getElementById('spawn-name');
    const name = (input && input.value) || '';
    if (!slugifyName(name)) {
      if (showToast) showToast('Give the agent a name', true);
      if (input) input.focus();
      return;
    }
    const identity = makeIdentity(name);
    const label = String(name).trim();
    const model = readModelPicker('spawn-model-select', 'spawn-model-custom') || (p.defaultModel || '');
    const petSlug =
      dockState.selectedPetSlug ||
      Theme.VARIANTS[0] ||
      null;

    const list = loadInstances();
    list.push({
      identity,
      providerId: p.id,
      petSlug,
      label,
      model,
      createdAt: Date.now(),
    });
    saveInstances(list);

    const agent = ensureAgentOnFloor(identity, {
      petSlug,
      providerId: p.id,
      label,
      model,
    });
    if (agent) {
      if (agent.setSnippet) agent.setSnippet(`${p.label}${model ? ' · ' + model : ''} · ready`);
      agent.status = 'ready';
      agent.statusHoldUntil = Date.now() + 4000;
    }

    if (showToast) {
      showToast(`Spawned ${label} (${identity}) — click it to chat`);
    }
    dockState.view = 'home';
    dockState.selectedProviderId = null;
    dockState.selectedPetSlug = null;
    render();
  }

export function onListClick(ev) {
    const t = ev.target.closest('[data-action]');
    if (!t) return;
    ev.preventDefault();
    ev.stopPropagation();
    const action = t.getAttribute('data-action');

    if (action === 'new-agent') {
      dockState.view = 'providers';
      render();
      return;
    }
    if (action === 'back-home') {
      dockState.view = 'home';
      dockState.selectedProviderId = null;
      render();
      return;
    }
    if (action === 'back-providers') {
      dockState.view = 'providers';
      dockState.selectedProviderId = null;
      dockState.selectedPetSlug = null;
      render();
      return;
    }
    if (action === 'pick-provider') {
      dockState.selectedProviderId = t.getAttribute('data-provider');
      dockState.selectedPetSlug =
        Theme.VARIANTS[0] || null;
      dockState.view = 'create';
      render();
      return;
    }
    if (action === 'pick-variant') {
      const nameEl = document.getElementById('spawn-name');
      const kept = nameEl ? nameEl.value : '';
      dockState.selectedPetSlug = t.getAttribute('data-pet');
      render();
      const again = document.getElementById('spawn-name');
      if (again) again.value = kept;
      return;
    }
    if (action === 'spawn') {
      spawnFromForm();
      return;
    }
    if (action === 'open-chat') {
      const id = t.getAttribute('data-identity');
      if (id) openChat(id);
    }
  }

export function wire() {
    if (dockState.wired) return;
    const toggleBtn = document.getElementById('agent-dock-toggle');
    const list = document.getElementById('agent-roster-list');
    const dock = document.getElementById('agent-dock');
    if (!toggleBtn || !list) return;
    dockState.wired = true;

    toggleBtn.addEventListener('click', (ev) => {
      ev.stopPropagation();
      setOpen(!isOpen());
    });
    list.addEventListener('click', onListClick);
    if (dock) dock.addEventListener('mousedown', (ev) => ev.stopPropagation());

    document.addEventListener('keydown', (ev) => {
      if (ev.target.tagName === 'INPUT' || ev.target.tagName === 'TEXTAREA') return;
      if (ev.key === 'Escape' && isOpen()) {
        setOpen(false);
        return;
      }
      if (ev.key.toLowerCase() === 'a' && !ev.metaKey && !ev.ctrlKey && !ev.altKey) {
        const detail = document.getElementById('session-detail');
        if (detail && detail.classList.contains('open')) return;
        ev.preventDefault();
        setOpen(!isOpen());
      }
    });

    hydrateInstancesOntoFloor();
    updateChrome();
  }

export function refresh() {
    updateChrome();
    if (isOpen() && dockState.view === 'home') render();
  }
