import { S, agents, STATUS_LABELS } from './state.js';
import { escapeHtml, showToast } from './state.js';
import { Agent } from './agent.js';
import { placeAgent } from './layout.js';
import { layoutAgents } from './scene.js';
import { Theme } from './theme.js';
import { showSessionDetail } from './chat.js';
import { modelsForProvider, defaultModelForProvider, requestModels, ingestModels, modelOptionsHtml, readModelPicker, syncModelCustomVisibility, fillModelPicker } from './dock-models.js';
export { ingestModels, fillModelPicker, readModelPicker, syncModelCustomVisibility, requestModels, modelsForProvider, defaultModelForProvider, modelOptionsHtml };
// Agent dock — provider-first spawn wizard (provider -> name+variant -> floor -> chat).
// Instances persist in localStorage nats-hub.instances.
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
      view = 'home';
      selectedProviderId = null;
      selectedPetSlug = null;
      render();
    }
  }

export function isOpen() {
    const roster = document.getElementById('agent-roster');
    return !!(roster && roster.classList.contains('open'));
  }

export function liveAgent(identity) {
    return true ? agents.get(identity) : null;
  }

export function statusDot(agent) {
    if (!agent) return 'off';
    if (agent.stopped) return 'off';
    if (agent.status === 'error') return 'err';
    if (agent.status === 'working' || agent.status === 'thinking') return 'busy';
    if (agent.status === 'ready') return 'live';
    return 'idle';
  }

export function statusText(agent) {
    if (!agent) return 'ON FLOOR';
    if (agent.stopped) return 'STOPPED';
    if (typeof STATUS_LABELS !== 'undefined' && STATUS_LABELS[agent.status]) {
      return STATUS_LABELS[agent.status];
    }
    return (agent.status || 'IDLE').toUpperCase();
  }

export function providerLogoHtml(p) {
    return (
      `<span class="provider-logo" style="--pcolor:${esc(p.color)}" aria-hidden="true">` +
      `<span class="provider-mono">${esc(p.monogram)}</span></span>`
    );
  }

export function variantThumbHtml(slug, selected) {
    return (
      `<button type="button" class="variant-pick${selected ? ' selected' : ''}" data-action="pick-variant" data-pet="${esc(slug)}" title="${esc(slug)}">` +
      `<span class="variant-pick-label">${esc(slug)}</span></button>`
    );
  }

export function renderHome(body) {
    const instances = loadInstances();
    const liveIds = true ? new Set(agents.keys()) : new Set();
    const known = new Set(instances.map((i) => i.identity));
    const liveExtra = [];
    for (const id of liveIds) {
      if (known.has(id)) continue;
      if (id === 'visualizer' || id === 'josh' || id === 'hub') continue;
      liveExtra.push(id);
    }

    let html = `<div class="roster-toolbar">
      <button type="button" class="roster-primary" data-action="new-agent">+ New agent</button>
    </div>`;

    if (!instances.length && !liveExtra.length) {
      html += `<div class="roster-empty">
        <strong>No agents yet</strong>
        Pick a provider type, name it, choose a variant — it lands on the floor.
        Then click the sprite to open chat and start a task.
      </div>`;
    } else {
      if (instances.length) {
        html += `<div class="roster-section-label">Your agents</div>`;
        for (const inst of instances) {
          const p = providerById(inst.providerId);
          const agent = liveAgent(inst.identity);
          const kind = p ? p.label : (inst.providerId || 'agent');
          const modelBit = inst.model ? ` · ${esc(inst.model)}` : '';
          html += `<button type="button" class="roster-item" data-action="open-chat" data-identity="${esc(inst.identity)}">
            <span class="roster-dot ${statusDot(agent)}"></span>
            <span class="roster-copy">
              <div class="roster-name">${esc(inst.label || inst.identity)}</div>
              <div class="roster-kind">${esc(kind)} · ${esc(inst.identity)}${modelBit}${inst.petSlug ? ' · ' + esc(inst.petSlug) : ''}</div>
            </span>
            <span class="roster-status">${esc(statusText(agent))}</span>
          </button>`;
        }
      }
      if (liveExtra.length) {
        html += `<div class="roster-section-label">Live on bus</div>`;
        for (const id of liveExtra) {
          const agent = liveAgent(id);
          html += `<button type="button" class="roster-item" data-action="open-chat" data-identity="${esc(id)}">
            <span class="roster-dot ${statusDot(agent)}"></span>
            <span class="roster-copy">
              <div class="roster-name">${esc(id)}</div>
              <div class="roster-kind">live worker</div>
            </span>
            <span class="roster-status">${esc(statusText(agent))}</span>
          </button>`;
        }
      }
    }
    body.innerHTML = html;
  }

export function renderProviders(body) {
    const list = providers();
    let html = `<div class="roster-toolbar">
      <button type="button" class="roster-back" data-action="back-home">← Back</button>
      <span class="roster-step">1 · Provider type</span>
    </div>
    <div class="provider-grid">`;
    for (const p of list) {
      html += `<button type="button" class="provider-card" data-action="pick-provider" data-provider="${esc(p.id)}">
        ${providerLogoHtml(p)}
        <span class="provider-copy">
          <span class="provider-name">${esc(p.label)}</span>
          <span class="provider-blurb">${esc(p.blurb)}</span>
        </span>
      </button>`;
    }
    html += `</div>
    <div class="roster-hint">Enabled providers = hub config. Override with localStorage <code>nats-hub.providers</code> (JSON array of ids).</div>`;
    body.innerHTML = html;
  }

export function renderCreate(body) {
    const p = providerById(selectedProviderId);
    if (!p) {
      view = 'providers';
      renderProviders(body);
      return;
    }
    const pets = Theme.VARIANTS || [];
    if (!selectedPetSlug && pets.length) selectedPetSlug = pets[0];

    let petHtml = '';
    for (const slug of pets) petHtml += variantThumbHtml(slug, slug === selectedPetSlug);
    if (!pets.length) petHtml = '<div class="roster-empty">No variants in theme</div>';

    body.innerHTML = `<div class="roster-toolbar">
      <button type="button" class="roster-back" data-action="back-providers">← Providers</button>
      <span class="roster-step">2 · Name & variant</span>
    </div>
    <div class="create-provider-chip">
      ${providerLogoHtml(p)} 
      <span>${esc(p.label)}</span>
    </div>
    <label class="create-field">
      <span>Instance name</span>
      <input id="spawn-name" type="text" maxlength="40" placeholder="e.g. design, research, petdex" autocomplete="off" />
    </label>
    <label class="create-field">
      <span>Model</span>
      <select id="spawn-model-select" class="model-select">
        ${modelOptionsHtml(p.id, p.defaultModel || '')}
      </select>
      <input id="spawn-model-custom" class="model-custom" type="text" maxlength="120"
        placeholder="${esc(p.defaultModel || 'provider/model')}" autocomplete="off" style="display:none;margin-top:6px" />
      <div id="spawn-model-hint" class="roster-hint" style="padding:0;margin-top:4px">Loading models from provider…</div>
    </label>
    <div class="create-field">
      <span>Variant / character</span>
      <div class="variant-grid">${petHtml}</div>
    </div>
    <button type="button" class="roster-primary create-go" data-action="spawn">Create on floor</button>
    <div class="roster-hint">Makes identity like <code>design-agent</code> and parks it on the floor. Click the sprite to chat. A worker process with that identity still needs to be running to answer.</div>`;

    const input = body.querySelector('#spawn-name');
    if (input) setTimeout(() => input.focus(), 30);
    const modelSel = body.querySelector('#spawn-model-select');
    if (modelSel) {
      modelSel.addEventListener('change', () => {
        syncModelCustomVisibility('spawn-model-select', 'spawn-model-custom');
      });
      syncModelCustomVisibility('spawn-model-select', 'spawn-model-custom');
    }
    // Live list for this provider (any CLI/ACP source registered in model_catalog)
    requestModels(p.id);
  }

export function updateChrome() {
    const countEl = document.getElementById('roster-count');
    const dockSub = document.getElementById('dock-sub');
    const title = document.querySelector('#agent-roster .roster-header h4');
    const nInst = loadInstances().length;
    const nLive = true ? agents.size : 0;
    if (countEl) {
      if (view === 'providers') countEl.textContent = `${providers().length} types`;
      else if (view === 'create') countEl.textContent = 'configure';
      else countEl.textContent = `${nInst} agent${nInst === 1 ? '' : 's'}`;
    }
    if (dockSub) {
      dockSub.textContent = nLive ? `${nLive} on floor · new or chat` : 'New agent from provider';
    }
    if (title) {
      title.textContent =
        view === 'providers' ? 'PROVIDERS' : view === 'create' ? 'NEW AGENT' : 'AGENTS';
    }
  }

export function render() {
    const body = document.getElementById('agent-roster-list');
    if (!body) return;
    if (view === 'providers') renderProviders(body);
    else if (view === 'create') renderCreate(body);
    else renderHome(body);
    updateChrome();
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
    const p = providerById(selectedProviderId);
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
      selectedPetSlug ||
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
    view = 'home';
    selectedProviderId = null;
    selectedPetSlug = null;
    render();
  }

export function onListClick(ev) {
    const t = ev.target.closest('[data-action]');
    if (!t) return;
    ev.preventDefault();
    ev.stopPropagation();
    const action = t.getAttribute('data-action');

    if (action === 'new-agent') {
      view = 'providers';
      render();
      return;
    }
    if (action === 'back-home') {
      view = 'home';
      selectedProviderId = null;
      render();
      return;
    }
    if (action === 'back-providers') {
      view = 'providers';
      selectedProviderId = null;
      selectedPetSlug = null;
      render();
      return;
    }
    if (action === 'pick-provider') {
      selectedProviderId = t.getAttribute('data-provider');
      selectedPetSlug =
        Theme.VARIANTS[0] || null;
      view = 'create';
      render();
      return;
    }
    if (action === 'pick-variant') {
      const nameEl = document.getElementById('spawn-name');
      const kept = nameEl ? nameEl.value : '';
      selectedPetSlug = t.getAttribute('data-pet');
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
    if (wired) return;
    const toggleBtn = document.getElementById('agent-dock-toggle');
    const list = document.getElementById('agent-roster-list');
    const dock = document.getElementById('agent-dock');
    if (!toggleBtn || !list) return;
    wired = true;

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
    if (isOpen() && view === 'home') render();
  }
