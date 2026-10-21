// Dock views: roster / provider / create-form rendering for the agent dock.
import { S, agents, STATUS_LABELS } from './state.js';
import { escapeHtml } from './state.js';
import { Theme } from './theme.js';
import { providers, loadInstances, providerById, dockState } from './dock.js';
import { modelOptionsHtml, requestModels, syncModelCustomVisibility } from './dock-models.js';
export function esc(s) { return escapeHtml(String(s)); }
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
    const p = providerById(dockState.selectedProviderId);
    if (!p) {
      dockState.view = 'providers';
      renderProviders(body);
      return;
    }
    const pets = Theme.VARIANTS || [];
    if (!dockState.selectedPetSlug && pets.length) dockState.selectedPetSlug = pets[0];

    let petHtml = '';
    for (const slug of pets) petHtml += variantThumbHtml(slug, slug === dockState.selectedPetSlug);
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
      if (dockState.view === 'providers') countEl.textContent = `${providers().length} types`;
      else if (dockState.view === 'create') countEl.textContent = 'configure';
      else countEl.textContent = `${nInst} agent${nInst === 1 ? '' : 's'}`;
    }
    if (dockSub) {
      dockSub.textContent = nLive ? `${nLive} on floor · new or chat` : 'New agent from provider';
    }
    if (title) {
      title.textContent =
        dockState.view === 'providers' ? 'PROVIDERS' : dockState.view === 'create' ? 'NEW AGENT' : 'AGENTS';
    }
  }

export function render() {
    const body = document.getElementById('agent-roster-list');
    if (!body) return;
    if (dockState.view === 'providers') renderProviders(body);
    else if (dockState.view === 'create') renderCreate(body);
    else renderHome(body);
    updateChrome();
  }

