import { S, agents } from './state.js';
import { Theme } from './theme.js';
import { showToast } from './state.js';
export function loadPositionsMap() {
  try {
    const raw = localStorage.getItem(POS_STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    return parsed && typeof parsed === 'object' ? parsed : {};
  } catch (_) {
    return {};
  }
}

export function saveAgentPosition(agent) {
  if (!agent || !width || !height) return;
  try {
    const map = loadPositionsMap();
    map[agent.identity] = {
      nx: Math.min(1, Math.max(0, agent.x / width)),
      ny: Math.min(1, Math.max(0, agent.y / height)),
      pinned: true,
    };
    localStorage.setItem(POS_STORAGE_KEY, JSON.stringify(map));
  } catch (_) { /* quota / private mode */ }
}

export function clearAllPinnedPositions() {
  try { localStorage.removeItem(POS_STORAGE_KEY); } catch (_) {}
}

export function applySavedPosition(agent) {
  if (!agent || !width || !height) return false;
  const pos = loadPositionsMap()[agent.identity];
  if (!pos || typeof pos.nx !== 'number' || typeof pos.ny !== 'number') return false;
  agent.pinned = true;
  agent.x = pos.nx * width;
  agent.y = pos.ny * height;
  agent.targetX = agent.x;
  agent.targetY = agent.y;
  // Self-heal positions saved under HUD/dock before occlusion clamp existed.
  clampAgentToFloor(agent);
  return true;
}

export function clampAgentToFloor(agent) {
  if (!agent || typeof width !== 'number' || width <= 0) return;
  const pad = agentBodyPad();
  // Canvas bounds first
  agent.x = Math.min(width - pad, Math.max(pad, agent.x));
  agent.y = Math.min(height - pad, Math.max(pad, agent.y));

  // Push out of HTML overlays that steal pointer events (HUD / dock / chat).
  // That's how sprites "disappear" behind menus and become un-grabbable.
  const rects = uiOcclusionRects();
  for (let iter = 0; iter < 5; iter++) {
    let moved = false;
    for (const r of rects) {
      // Treat agent as a circle of radius pad against the rect
      const nearestX = Math.min(r.right, Math.max(r.left, agent.x));
      const nearestY = Math.min(r.bottom, Math.max(r.top, agent.y));
      const dx = agent.x - nearestX;
      const dy = agent.y - nearestY;
      const dist2 = dx * dx + dy * dy;
      if (dist2 >= pad * pad && !pointInExpandedRect(agent.x, agent.y, r, 0)) {
        continue;
      }
      // Inside or overlapping — push to nearest exterior side + pad
      const dl = Math.abs(agent.x - r.left);
      const dr = Math.abs(r.right - agent.x);
      const dt = Math.abs(agent.y - r.top);
      const db = Math.abs(r.bottom - agent.y);
      const m = Math.min(dl, dr, dt, db);
      if (m === dl) agent.x = r.left - pad;
      else if (m === dr) agent.x = r.right + pad;
      else if (m === dt) agent.y = r.top - pad;
      else agent.y = r.bottom + pad;

      agent.x = Math.min(width - pad, Math.max(pad, agent.x));
      agent.y = Math.min(height - pad, Math.max(pad, agent.y));
      moved = true;
    }
    if (!moved) break;
  }
  agent.targetX = agent.x;
  agent.targetY = agent.y;
}

export function agentBodyPad() {
  try {
    if (Theme.metrics) {
      return Math.max(52, Theme.metrics().hitRadius * 1.55);
    }
  } catch (_) {}
  return 52;
}

export function pointInExpandedRect(x, y, r, expand) {
  return (
    x >= r.left - expand &&
    x <= r.right + expand &&
    y >= r.top - expand &&
    y <= r.bottom + expand
  );
}

export function uiOcclusionRects() {
  const rects = [];
  const margin = 6;

  function addVisible(el) {
    if (!el) return;
    const style = window.getComputedStyle(el);
    if (!style || style.display === 'none' || style.visibility === 'hidden') return;
    if (parseFloat(style.opacity || '1') < 0.05) return;
    const r = el.getBoundingClientRect();
    if (r.width < 8 || r.height < 8) return;
    // Skip if fully off-canvas
    if (r.right < 0 || r.bottom < 0 || r.left > window.innerWidth || r.top > window.innerHeight) return;
    rects.push({
      left: r.left - margin,
      top: r.top - margin,
      right: r.right + margin,
      bottom: r.bottom + margin,
    });
  }

  addVisible(document.getElementById('hud'));
  addVisible(document.getElementById('agent-dock-toggle'));
  const roster = document.getElementById('agent-roster');
  if (roster && roster.classList.contains('open')) addVisible(roster);
  const detail = document.getElementById('session-detail');
  if (detail && detail.classList.contains('open')) addVisible(detail);
  const popup = document.getElementById('action-popup');
  if (popup && popup.style.display === 'block') addVisible(popup);
  return rects;
}

/** Eject any agents currently under UI chrome without destroying pins/layout intent. */
export function rescueAgentsFromUi(opts = {}) {
  if (typeof width !== 'number' || width <= 0) return 0;
  let n = 0;
  for (const agent of agents.values()) {
    if (agent.dragging) continue;
    const bx = agent.x;
    const by = agent.y;
    clampAgentToFloor(agent);
    if (Math.hypot(agent.x - bx, agent.y - by) > 0.5) {
      agent.pinned = true;
      saveAgentPosition(agent);
      n++;
    }
  }
  if (n && !opts.silent) {
    showToast(n === 1 ? 'Unstuck 1 agent from under UI' : `Unstuck ${n} agents from under UI`);
  }
  return n;
}

/** Place a newly created agent: restore pin, else center then ring-layout free agents. */
export function placeAgent(agent) {
  if (!agent) return;
  if (applySavedPosition(agent)) return;
  if (typeof width === 'number' && width > 0) {
    agent.x = width / 2;
    agent.y = height / 2;
    agent.targetX = agent.x;
    agent.targetY = agent.y;
  }
  layoutAgents();
}

export function updateBgStatus(text) {
  const el = document.getElementById('bg-status');
  if (el) el.textContent = text;
}

export function setSceneBackgroundFromDataUrl(dataUrl, opts = {}) {
  if (!dataUrl) {
    S.sceneBgImage = null;
    S.sceneBgActive = false;
    if (!opts.silent) updateBgStatus('Default grid · hold/drag agents to place');
    return;
  }
  loadImage(
    dataUrl,
    (img) => {
      S.sceneBgImage = img;
      S.sceneBgActive = true;
      updateBgStatus(opts.statusText || 'Custom image · cover fit under grid');
    },
    () => {
      S.sceneBgImage = null;
      S.sceneBgActive = false;
      if (!opts.silent) showToast('Failed to load background image', true);
      updateBgStatus('Default grid · hold/drag agents to place');
    }
  );
}

export function compressImageFileToDataUrl(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error('read failed'));
    reader.onload = () => {
      const img = new Image();
      img.onerror = () => reject(new Error('decode failed'));
      img.onload = () => {
        const maxEdge = 1920;
        let w = img.naturalWidth || img.width;
        let h = img.naturalHeight || img.height;
        if (!w || !h) {
          reject(new Error('bad dimensions'));
          return;
        }
        if (Math.max(w, h) > maxEdge) {
          const s = maxEdge / Math.max(w, h);
          w = Math.max(1, Math.round(w * s));
          h = Math.max(1, Math.round(h * s));
        }
        const canvas = document.createElement('canvas');
        canvas.width = w;
        canvas.height = h;
        const ctx = canvas.getContext('2d');
        ctx.drawImage(img, 0, 0, w, h);
        // JPEG keeps localStorage under typical 5MB quotas better than PNG.
        resolve(canvas.toDataURL('image/jpeg', 0.82));
      };
      img.src = reader.result;
    };
    reader.readAsDataURL(file);
  });
}

async function handleBackgroundFile(file) {
  if (!file) return;
  if (!file.type || !file.type.startsWith('image/')) {
    showToast('Pick an image file', true);
    return;
  }
  updateBgStatus('Compressing image…');
  try {
    const dataUrl = await compressImageFileToDataUrl(file);
    setSceneBackgroundFromDataUrl(dataUrl);
    try {
      localStorage.setItem(BG_STORAGE_KEY, dataUrl);
      showToast('Background saved');
    } catch (_) {
      // Still show for this session even if storage is full.
      showToast('Background applied (not persisted — image too large for storage)', true);
      updateBgStatus('Custom image (session only — storage full)');
    }
  } catch (e) {
    showToast('Could not use that image', true);
    updateBgStatus('Default grid · hold/drag agents to place');
  }
}

export function clearSceneBackground() {
  S.sceneBgImage = null;
  S.sceneBgActive = false;
  try { localStorage.removeItem(BG_STORAGE_KEY); } catch (_) {}
  updateBgStatus('Default grid · hold/drag agents to place');
  showToast('Background cleared');
}

export function reflowAllAgents() {
  for (const agent of agents.values()) {
    agent.pinned = false;
    agent.dragging = false;
  }
  clearAllPinnedPositions();
  layoutAgents();
  // After ring targets settle one frame they still might land under chrome on tiny viewports.
  setTimeout(() => rescueAgentsFromUi({ silent: true }), 0);
  showToast('Agents reflowed');
}

export function restoreSceneBackgroundFromStorage() {
  try {
    const dataUrl = localStorage.getItem(BG_STORAGE_KEY);
    if (dataUrl) setSceneBackgroundFromDataUrl(dataUrl, { silent: true });
  } catch (_) {}
}

export function beginAgentDrag(gesture) {
  if (!gesture || gesture.mode === 'dragging') return;
  gesture.mode = 'dragging';
  const agent = gesture.agent;
  agent.dragging = true;
  agent.pinned = true;
  S.selectedAgent = agent;
  const popup = document.getElementById('action-popup');
  if (popup) popup.style.display = 'none';
  agent.x = mouseX - gesture.offsetX;
  agent.y = mouseY - gesture.offsetY;
  clampAgentToFloor(agent);
}

export function cancelPointerGestureTimers(gesture) {
  if (gesture && gesture.holdTimer) {
    clearTimeout(gesture.holdTimer);
    gesture.holdTimer = null;
  }
}

// Helper to word-wrap thought bubble text in p5.js
export function wrapText(str, maxW) {
  if (!str) return [];
  let words = str.split(' ');
  let lines = [];
  let currentLine = '';
  
  for (let i = 0; i < words.length; i++) {
    let word = words[i];
    let testLine = currentLine + (currentLine === '' ? '' : ' ') + word;
    if (textWidth(testLine) > maxW) {
      if (currentLine !== '') {
        lines.push(currentLine);
        currentLine = word;
      } else {
        lines.push(word);
        currentLine = '';
      }
    } else {
      currentLine = testLine;
    }
  }
  if (currentLine !== '') {
    lines.push(currentLine);
  }
  return lines;
}

// Make echo-worker reversed payloads readable in bubbles.
