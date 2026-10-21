// Dock model picker: live per-provider model lists via the WS bridge.
import { S } from './state.js';
import { escapeHtml } from './state.js';
function esc(s) { return escapeHtml(String(s)); }
import { providerById } from './dock.js';
  // Live model lists come from supervisor via WS `list_models` → hub.worker.models.
  // Provider catalog is provider-agnostic (kilo/opencode/cursor/agy/hermes/… + config).
  // Cache shape: { models:[{value,label}], reason?, source?, loading?, loadedAt }
export const modelCache = Object.create(null);
export const modelWaiters = Object.create(null); // providerId → [resolve]


export function modelsForProvider(providerId) {
    const entry = modelCache[providerId];
    if (entry && Array.isArray(entry.models)) return entry.models.slice();
    return [];
  }

export function defaultModelForProvider(providerId) {
    const p = providerById(providerId);
    if (p && p.defaultModel) return p.defaultModel;
    const models = modelsForProvider(providerId);
    return (models[0] && models[0].value) || '';
  }

export function requestModels(providerId, { refresh = false } = {}) {
    const id = String(providerId || '').trim();
    if (!id) return Promise.resolve({ models: [] });
    const cached = modelCache[id];
    if (!refresh && cached && !cached.loading && Array.isArray(cached.models)) {
      return Promise.resolve(cached);
    }
    if (!refresh && cached && cached.loading && modelWaiters[id]) {
      return new Promise((resolve) => modelWaiters[id].push(resolve));
    }
    modelCache[id] = Object.assign({}, cached || {}, { loading: true, models: (cached && cached.models) || [] });
    if (!modelWaiters[id]) modelWaiters[id] = [];
    return new Promise((resolve) => {
      modelWaiters[id].push(resolve);
      if (!S.ws || S.ws.readyState !== WebSocket.OPEN) {
        // No WS yet — resolve empty; UI keeps Provider default + Other
        ingestModels(id, {
          ok: false,
          models: [],
          reason: 'WebSocket not connected — supervisor model list unavailable',
        });
        return;
      }
      try {
        S.ws.send(JSON.stringify({ type: 'list_models', provider: id, refresh: !!refresh }));
      } catch (e) {
        ingestModels(id, { ok: false, models: [], error: String(e) });
      }
    });
  }

  /** Called from index.html when WS returns type=models */
export function ingestModels(providerId, detail) {
    const id = String(providerId || '').trim();
    if (!id) return;
    const models = [];
    const raw = (detail && detail.models) || [];
    for (const m of raw) {
      if (!m) continue;
      if (typeof m === 'string') models.push({ value: m, label: m });
      else if (m.value) models.push({ value: String(m.value), label: String(m.label || m.value) });
    }
    const entry = {
      models,
      loading: false,
      loadedAt: Date.now(),
      ok: !(detail && detail.ok === false),
      source: detail && detail.source,
      reason: (detail && (detail.reason || detail.error)) || '',
      cmd: detail && detail.cmd,
    };
    modelCache[id] = entry;
    const waiters = modelWaiters[id] || [];
    delete modelWaiters[id];
    for (const w of waiters) {
      try { w(entry); } catch (_) { /* ignore */ }
    }
    // Refresh open create form / chat picker if this provider is active
    if (view === 'create' && selectedProviderId === id) {
      const body = document.getElementById('agent-roster-list');
      if (body) fillModelPicker('spawn-model-select', 'spawn-model-custom', id, defaultModelForProvider(id));
      const hint = document.getElementById('spawn-model-hint');
      if (hint) {
        if (entry.reason && !models.length) hint.textContent = entry.reason;
        else if (models.length) hint.textContent = `${models.length} models from ${entry.source || 'provider'}`;
        else hint.textContent = 'No models listed — use Other… or Provider default';
      }
    }
    if (S.openLogAgent && S.openLogAgent.providerId === id) {
      fillModelPicker('chat-model-select', 'chat-model-custom', id, S.openLogAgent.model || '');
    }
  }

  /** Build <select> options HTML. Includes blank + known models + Other… */
export function modelOptionsHtml(providerId, selectedModel) {
    const models = modelsForProvider(providerId);
    const selected = selectedModel || defaultModelForProvider(providerId) || '';
    const knownValues = new Set(models.map((m) => m.value));
    const isCustom = selected && !knownValues.has(selected);
    const loading = modelCache[providerId] && modelCache[providerId].loading;

    let html = `<option value="">Provider default</option>`;
    if (loading && !models.length) {
      html += `<option value="" disabled>Loading models…</option>`;
    }
    for (const m of models) {
      const sel = !isCustom && m.value === selected ? ' selected' : '';
      html += `<option value="${esc(m.value)}"${sel}>${esc(m.label)}</option>`;
    }
    html += `<option value="__other__"${isCustom ? ' selected' : ''}>Other…</option>`;
    return html;
  }

  /** Read select + optional custom input → model string (or ''). */
export function readModelPicker(selectId, customInputId) {
    const sel = document.getElementById(selectId);
    const custom = document.getElementById(customInputId);
    if (!sel) return (custom && custom.value.trim()) || '';
    const v = sel.value;
    if (v === '__other__') return (custom && custom.value.trim()) || '';
    return v || '';
  }

export function syncModelCustomVisibility(selectId, customInputId) {
    const sel = document.getElementById(selectId);
    const custom = document.getElementById(customInputId);
    if (!sel || !custom) return;
    const show = sel.value === '__other__';
    custom.style.display = show ? '' : 'none';
    if (show) setTimeout(() => custom.focus(), 20);
  }

export function fillModelPicker(selectId, customInputId, providerId, selectedModel) {
    const sel = document.getElementById(selectId);
    const custom = document.getElementById(customInputId);
    if (!sel) return;
    // kick a fetch if we have nothing yet
    if (!modelCache[providerId] || modelCache[providerId].loading === undefined) {
      requestModels(providerId);
    } else if (!modelCache[providerId].loading && !(modelCache[providerId].models || []).length) {
      // still empty after load is fine; allow refresh-less empty
    }
    const models = modelsForProvider(providerId);
    const selected = selectedModel || defaultModelForProvider(providerId) || '';
    const knownValues = new Set(models.map((m) => m.value));
    const isCustom = selected && !knownValues.has(selected);
    sel.innerHTML = modelOptionsHtml(providerId, selected);
    if (custom) {
      custom.value = isCustom ? selected : '';
      custom.style.display = isCustom ? '' : 'none';
      custom.placeholder = defaultModelForProvider(providerId) || 'provider/model';
    }
  }

