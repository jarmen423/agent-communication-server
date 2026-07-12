/**
 * Agent dock — provider-first spawn wizard for the arcade visualizer.
 *
 * Cold start (config only, no workers yet):
 *   AGENTS chevron → provider types (Cursor, Claude Code, Codex, AGY, Grok…)
 *   → name the instance + pick petdex character
 *   → agent appears on the floor → click sprite/row to chat and start a task
 *
 * Persistence:
 *   localStorage nats-hub.providers = string[] enabled provider ids (optional)
 *   localStorage nats-hub.instances = [{ identity, providerId, petSlug, label, createdAt }]
 *
 * Globals expected from index.html: agents, Agent, layoutAgents, showSessionDetail,
 * selectedAgent, showToast, escapeHtml, Petdex, STATUS_LABELS
 */
const AgentDock = (() => {
  const LS_PROVIDERS = 'nats-hub.providers';
  const LS_INSTANCES = 'nats-hub.instances';

  const DEFAULT_PROVIDERS = [
    { id: 'cursor', label: 'Cursor', monogram: 'Cu', color: '#00ff88', blurb: 'Cursor agent CLI', backend: 'headless' },
    { id: 'claude', label: 'Claude Code', monogram: 'CC', color: '#d4a27f', blurb: 'Anthropic Claude Code', backend: 'headless' },
    { id: 'codex', label: 'Codex', monogram: 'Cx', color: '#a78bfa', blurb: 'OpenAI Codex CLI', backend: 'headless' },
    { id: 'agy', label: 'Antigravity', monogram: 'AG', color: '#38bdf8', blurb: 'agy headless agent', backend: 'headless' },
    { id: 'grok', label: 'Grok', monogram: 'Gk', color: '#00d9ff', blurb: 'Grok ACP stdio', backend: 'acp' },
    { id: 'hermes', label: 'Hermes', monogram: 'He', color: '#f472b6', blurb: 'Hermes ACP / headless', backend: 'acp' },
    { id: 'echo', label: 'Echo', monogram: 'Ec', color: '#94a3b8', blurb: 'Dogfood reverse worker', backend: 'sdk' },
  ];

  let view = 'home'; // home | providers | create
  let selectedProviderId = null;
  let selectedPetSlug = null;
  let wired = false;

  function esc(s) {
    if (typeof escapeHtml === 'function') return escapeHtml(s);
    return String(s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  function providers() {
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

  function loadInstances() {
    try {
      const raw = localStorage.getItem(LS_INSTANCES);
      if (!raw) return [];
      const arr = JSON.parse(raw);
      return Array.isArray(arr) ? arr.filter((i) => i && i.identity) : [];
    } catch (_) {
      return [];
    }
  }

  function saveInstances(list) {
    try {
      localStorage.setItem(LS_INSTANCES, JSON.stringify(list));
    } catch (_) { /* quota */ }
  }

  function providerById(id) {
    return DEFAULT_PROVIDERS.find((p) => p.id === id) || null;
  }

  function slugifyName(name) {
    return String(name || '')
      .trim()
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .slice(0, 40);
  }

  function makeIdentity(name) {
    let base = slugifyName(name);
    if (!base) base = 'agent';
    if (!base.endsWith('-agent') && !base.includes('worker')) base = base + '-agent';
    let id = base;
    let n = 2;
    const taken = new Set(loadInstances().map((i) => i.identity));
    if (typeof agents !== 'undefined') {
      for (const k of agents.keys()) taken.add(k);
    }
    while (taken.has(id)) id = `${base}-${n++}`;
    return id;
  }

  function ensureAgentOnFloor(identity, opts = {}) {
    if (typeof agents === 'undefined' || typeof Agent === 'undefined') return null;
    let agent = agents.get(identity);
    if (!agent) {
      agent = new Agent(identity);
      agent.status = 'idle';
      agent.lastSeen = Date.now();
      agents.set(identity, agent);
    }
    if (opts.petSlug) {
      agent.petSlug = opts.petSlug;
      if (typeof Petdex !== 'undefined' && Petdex.setExplicitPet) {
        Petdex.setExplicitPet(identity, opts.petSlug);
      }
    }
    if (opts.providerId) agent.providerId = opts.providerId;
    if (opts.label) agent.displayLabel = opts.label;
    if (typeof layoutAgents === 'function') layoutAgents();
    const countEl = document.getElementById('agent-count');
    if (countEl) countEl.textContent = String(agents.size);
    return agent;
  }

  function hydrateInstancesOntoFloor() {
    for (const inst of loadInstances()) {
      ensureAgentOnFloor(inst.identity, {
        petSlug: inst.petSlug,
        providerId: inst.providerId,
        label: inst.label,
      });
    }
  }

  function setOpen(open) {
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

  function isOpen() {
    const roster = document.getElementById('agent-roster');
    return !!(roster && roster.classList.contains('open'));
  }

  function liveAgent(identity) {
    return typeof agents !== 'undefined' ? agents.get(identity) : null;
  }

  function statusDot(agent) {
    if (!agent) return 'off';
    if (agent.stopped) return 'off';
    if (agent.status === 'error') return 'err';
    if (agent.status === 'working' || agent.status === 'thinking') return 'busy';
    if (agent.status === 'ready') return 'live';
    return 'idle';
  }

  function statusText(agent) {
    if (!agent) return 'ON FLOOR';
    if (agent.stopped) return 'STOPPED';
    if (typeof STATUS_LABELS !== 'undefined' && STATUS_LABELS[agent.status]) {
      return STATUS_LABELS[agent.status];
    }
    return (agent.status || 'IDLE').toUpperCase();
  }

  function providerLogoHtml(p) {
    return (
      `<span class="provider-logo" style="--pcolor:${esc(p.color)}" aria-hidden="true">` +
      `<span class="provider-mono">${esc(p.monogram)}</span></span>`
    );
  }

  function petThumbHtml(slug, selected) {
    return (
      `<button type="button" class="pet-pick${selected ? ' selected' : ''}" data-action="pick-pet" data-pet="${esc(slug)}" title="${esc(slug)}">` +
      `<span class="pet-pick-label">${esc(slug)}</span></button>`
    );
  }

  function renderHome(body) {
    const instances = loadInstances();
    const liveIds = typeof agents !== 'undefined' ? new Set(agents.keys()) : new Set();
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
        Pick a provider type, name it, choose a pet — it lands on the floor.
        Then click the sprite to open chat and start a task.
      </div>`;
    } else {
      if (instances.length) {
        html += `<div class="roster-section-label">Your agents</div>`;
        for (const inst of instances) {
          const p = providerById(inst.providerId);
          const agent = liveAgent(inst.identity);
          const kind = p ? p.label : (inst.providerId || 'agent');
          html += `<button type="button" class="roster-item" data-action="open-chat" data-identity="${esc(inst.identity)}">
            <span class="roster-dot ${statusDot(agent)}"></span>
            <span class="roster-copy">
              <div class="roster-name">${esc(inst.label || inst.identity)}</div>
              <div class="roster-kind">${esc(kind)} · ${esc(inst.identity)}${inst.petSlug ? ' · ' + esc(inst.petSlug) : ''}</div>
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

  function renderProviders(body) {
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

  function renderCreate(body) {
    const p = providerById(selectedProviderId);
    if (!p) {
      view = 'providers';
      renderProviders(body);
      return;
    }
    const pets = (typeof Petdex !== 'undefined' && Petdex.INSTALLED_PETS) || [];
    if (!selectedPetSlug && pets.length) selectedPetSlug = pets[0];

    let petHtml = '';
    for (const slug of pets) petHtml += petThumbHtml(slug, slug === selectedPetSlug);
    if (!pets.length) petHtml = '<div class="roster-empty">No pets in visualizer/pets/</div>';

    body.innerHTML = `<div class="roster-toolbar">
      <button type="button" class="roster-back" data-action="back-providers">← Providers</button>
      <span class="roster-step">2 · Name & pet</span>
    </div>
    <div class="create-provider-chip">
      ${providerLogoHtml(p)}
      <span>${esc(p.label)}</span>
    </div>
    <label class="create-field">
      <span>Instance name</span>
      <input id="spawn-name" type="text" maxlength="40" placeholder="e.g. design, research, petdex" autocomplete="off" />
    </label>
    <div class="create-field">
      <span>Pet / character</span>
      <div class="pet-grid">${petHtml}</div>
    </div>
    <button type="button" class="roster-primary create-go" data-action="spawn">Create on floor</button>
    <div class="roster-hint">Makes identity like <code>design-agent</code> and parks it on the floor. Click the sprite to chat. A worker process with that identity still needs to be running to answer.</div>`;

    const input = body.querySelector('#spawn-name');
    if (input) setTimeout(() => input.focus(), 30);
  }

  function updateChrome() {
    const countEl = document.getElementById('roster-count');
    const dockSub = document.getElementById('dock-sub');
    const title = document.querySelector('#agent-roster .roster-header h4');
    const nInst = loadInstances().length;
    const nLive = typeof agents !== 'undefined' ? agents.size : 0;
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

  function render() {
    const body = document.getElementById('agent-roster-list');
    if (!body) return;
    if (view === 'providers') renderProviders(body);
    else if (view === 'create') renderCreate(body);
    else renderHome(body);
    updateChrome();
  }

  function openChat(identity) {
    const agent = ensureAgentOnFloor(identity);
    if (!agent) {
      if (typeof showToast === 'function') showToast('Could not open agent', true);
      return;
    }
    try {
      selectedAgent = agent;
    } catch (_) { /* non-writable */ }
    setOpen(false);
    if (typeof showSessionDetail === 'function') showSessionDetail(agent);
    if (typeof showToast === 'function') showToast(`Chat · ${identity}`);
  }

  function spawnFromForm() {
    const p = providerById(selectedProviderId);
    if (!p) return;
    const input = document.getElementById('spawn-name');
    const name = (input && input.value) || '';
    if (!slugifyName(name)) {
      if (typeof showToast === 'function') showToast('Give the agent a name', true);
      if (input) input.focus();
      return;
    }
    const identity = makeIdentity(name);
    const label = String(name).trim();
    const petSlug =
      selectedPetSlug ||
      (typeof Petdex !== 'undefined' && Petdex.INSTALLED_PETS[0]) ||
      null;

    const list = loadInstances();
    list.push({
      identity,
      providerId: p.id,
      petSlug,
      label,
      createdAt: Date.now(),
    });
    saveInstances(list);

    const agent = ensureAgentOnFloor(identity, {
      petSlug,
      providerId: p.id,
      label,
    });
    if (agent) {
      if (agent.setSnippet) agent.setSnippet(`${p.label} · ready for task`);
      agent.status = 'ready';
      agent.statusHoldUntil = Date.now() + 4000;
    }

    if (typeof showToast === 'function') {
      showToast(`Spawned ${label} (${identity}) — click it to chat`);
    }
    view = 'home';
    selectedProviderId = null;
    selectedPetSlug = null;
    render();
  }

  function onListClick(ev) {
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
        (typeof Petdex !== 'undefined' && Petdex.INSTALLED_PETS[0]) || null;
      view = 'create';
      render();
      return;
    }
    if (action === 'pick-pet') {
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

  function wire() {
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

  function refresh() {
    updateChrome();
    if (isOpen() && view === 'home') render();
  }

  return {
    DEFAULT_PROVIDERS,
    wire,
    refresh,
    setOpen,
    isOpen,
    hydrateInstancesOntoFloor,
    ensureAgentOnFloor,
  };
})();
