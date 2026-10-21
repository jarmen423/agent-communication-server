import { S, agents } from './state.js';
import { Theme } from './theme.js';
import { Agent } from './agent.js';
import { draw, layoutAgents } from './scene.js';
import { connectWS } from './ws.js';
import * as Dock from './dock.js';
import { placeAgent, clampAgentToFloor, saveAgentPosition, applySavedPosition, rescueAgentsFromUi, reflowAllAgents, handleBackgroundFile, clearSceneBackground, restoreSceneBackgroundFromStorage, updateBgStatus, beginAgentDrag, cancelPointerGestureTimers } from './layout.js';
import { showToast } from './state.js';
import { showActionPopup, popupAction } from './panels.js';
import { showSessionDetail, closeModal, sendChatMessage, applyChatModel, updateChatSendEnabled, refreshAgentRoster } from './chat.js';
import { BG_STORAGE_KEY, AGENT_HOLD_MS, AGENT_DRAG_PX } from './state.js';
export function preload() {
  Theme.preloadAll();
}

export function setup() {
  createCanvas(windowWidth, windowHeight);
  pixelDensity(1);
  frameRate(60);

  restoreSceneBackgroundFromStorage();
  // Re-apply saved pins once canvas size is known (dock may hydrate before setup).
  for (const agent of agents.values()) {
    if (!applySavedPosition(agent) && !agent.pinned) {
      // leave for layoutAgents
    }
  }
  layoutAgents();
  // Eject anyone restored under HUD/dock (pointer-events steal grabs).
  rescueAgentsFromUi({ silent: true });

  // Connect WebSocket
  connectWS();
}

// === Pointer / Interaction Handlers ===
export function pointerOnUi(event) {
  const t = (event && event.target) || document.elementFromPoint(mouseX, mouseY);
  if (!t || !t.closest) return false;
  return !!(
    t.closest('#action-popup') ||
    t.closest('#session-detail') ||
    t.closest('#modal-backdrop') ||
    t.closest('#toast') ||
    t.closest('#hud') ||
    t.closest('#agent-dock')
  );
}

export function mousePressed(event) {
  if (pointerOnUi(event)) return;

  // Prefer topmost (later map insert ≈ newer); convert to array reverse for hit test
  const list = [...agents.values()].reverse();
  for (const agent of list) {
    if (agent.contains(mouseX, mouseY)) {
      cancelPointerGestureTimers(S.pointerGesture);
      S.pointerGesture = {
        agent,
        startX: mouseX,
        startY: mouseY,
        offsetX: mouseX - agent.x,
        offsetY: mouseY - agent.y,
        mode: 'pending', // pending → click menu; dragging → move, no menu
        holdTimer: setTimeout(() => {
          if (S.pointerGesture && S.pointerGesture.mode === 'pending' && S.pointerGesture.agent === agent) {
            beginAgentDrag(S.pointerGesture);
          }
        }, AGENT_HOLD_MS),
      };
      return;
    }
  }

  // Clicking empty canvas closes popup
  S.selectedAgent = null;
  document.getElementById('action-popup').style.display = 'none';
}

export function mouseDragged(event) {
  if (!S.pointerGesture) return;
  if (pointerOnUi(event) && S.pointerGesture.mode !== 'dragging') return;

  const g = S.pointerGesture;
  const dx = mouseX - g.startX;
  const dy = mouseY - g.startY;
  if (g.mode === 'pending' && (dx * dx + dy * dy) >= AGENT_DRAG_PX * AGENT_DRAG_PX) {
    cancelPointerGestureTimers(g);
    beginAgentDrag(g);
  }
  if (g.mode === 'dragging') {
    g.agent.x = mouseX - g.offsetX;
    g.agent.y = mouseY - g.offsetY;
    clampAgentToFloor(g.agent);
  }
}

export function mouseReleased(event) {
  if (!S.pointerGesture) return;
  const g = S.pointerGesture;
  cancelPointerGestureTimers(g);

  if (g.mode === 'pending') {
    // Pure click → action menu (Open chat / Stop / Resume)
    S.selectedAgent = g.agent;
    showActionPopup(g.agent);
  } else if (g.mode === 'dragging') {
    g.agent.dragging = false;
    g.agent.pinned = true;
    clampAgentToFloor(g.agent);
    saveAgentPosition(g.agent);
    // No menu after hold/drag move
  }

  S.pointerGesture = null;
}

// If pointer is released outside the canvas, still finish the gesture.
window.addEventListener('mouseup', () => {
  if (S.pointerGesture) mouseReleased();
});
window.addEventListener('blur', () => {
  if (!S.pointerGesture) return;
  cancelPointerGestureTimers(S.pointerGesture);
  if (S.pointerGesture.mode === 'dragging' && S.pointerGesture.agent) {
    S.pointerGesture.agent.dragging = false;
    S.pointerGesture.agent.pinned = true;
    clampAgentToFloor(S.pointerGesture.agent);
    saveAgentPosition(S.pointerGesture.agent);
  }
  S.pointerGesture = null;
});


// === Global Keyboard Affordances ===
document.addEventListener('keydown', (ev) => {
  const popup = document.getElementById('action-popup');
  const detail = document.getElementById('session-detail');

  if (ev.key === 'Escape') {
    const roster = document.getElementById('agent-roster');
    if (detail.classList.contains('open')) {
      closeModal();
    } else if (roster && roster.classList.contains('open')) {
      setAgentRosterOpen(false);
    } else if (popup.style.display === 'block') {
      popup.style.display = 'none';
      S.selectedAgent = null;
    } else {
      S.selectedAgent = null;
    }
    return;
  }

  // Handle popup keyboard shortcuts when visible
  if (popup.style.display === 'block' && S.selectedAgent) {
    if (ev.target.tagName === 'INPUT' || ev.target.tagName === 'TEXTAREA') return;

    const key = ev.key.toLowerCase();
    if (key === 'c' || key === 'v' || key === 'm') {
      ev.preventDefault();
      popupAction('view');
    } else if (key === 'r') {
      ev.preventDefault();
      popupAction('resume');
    } else if (key === 's') {
      ev.preventDefault();
      popupAction('stop');
    }
  }
});

// Wire click interactions (safe if script runs after DOMContentLoaded)
export function wireUiInteractions() {
  const popup = document.getElementById('action-popup');
  if (!popup || popup.dataset.wired === '1') return;
  popup.dataset.wired = '1';

  popup.addEventListener('click', (ev) => {
    ev.stopPropagation();
    const btn = ev.target.closest('button[data-action]');
    if (!btn) return;
    popupAction(btn.getAttribute('data-action'));
  });
  popup.addEventListener('mousedown', (ev) => ev.stopPropagation());

  const detail = document.getElementById('session-detail');
  if (detail) detail.addEventListener('mousedown', (ev) => ev.stopPropagation());

  const closeBtn = document.getElementById('detail-close');
  if (closeBtn) closeBtn.addEventListener('click', closeModal);
  const backdrop = document.getElementById('modal-backdrop');
  if (backdrop) backdrop.addEventListener('click', closeModal);

  const composer = document.getElementById('chat-composer');
  const input = document.getElementById('chat-input');
  if (composer) {
    composer.addEventListener('submit', (ev) => {
      ev.preventDefault();
      sendChatMessage();
    });
  }
  if (input) {
    input.addEventListener('input', updateChatSendEnabled);
    input.addEventListener('keydown', (ev) => {
      if (ev.key === 'Enter' && !ev.shiftKey) {
        ev.preventDefault();
        sendChatMessage();
      }
    });
  }

  const modelApply = document.getElementById('chat-model-apply');
  const modelSel = document.getElementById('chat-model-select');
  const modelCustom = document.getElementById('chat-model-custom');
  if (modelApply) modelApply.addEventListener('click', applyChatModel);
  if (modelSel) {
    modelSel.addEventListener('change', () => {
      if (Dock.syncModelCustomVisibility) {
        Dock.syncModelCustomVisibility('chat-model-select', 'chat-model-custom');
      }
    });
  }
  if (modelCustom) {
    modelCustom.addEventListener('keydown', (ev) => {
      if (ev.key === 'Enter') {
        ev.preventDefault();
        applyChatModel();
      }
    });
  }

  const bgUploadBtn = document.getElementById('bg-upload-btn');
  const bgClearBtn = document.getElementById('bg-clear-btn');
  const bgFile = document.getElementById('bg-file');
  const reflowBtn = document.getElementById('reflow-agents-btn');
  const unstickBtn = document.getElementById('unstick-agents-btn');
  if (bgUploadBtn && bgFile) {
    bgUploadBtn.addEventListener('click', () => bgFile.click());
    bgFile.addEventListener('change', () => {
      const file = bgFile.files && bgFile.files[0];
      bgFile.value = '';
      if (file) handleBackgroundFile(file);
    });
  }
  if (bgClearBtn) bgClearBtn.addEventListener('click', clearSceneBackground);
  if (reflowBtn) reflowBtn.addEventListener('click', reflowAllAgents);
  if (unstickBtn) unstickBtn.addEventListener('click', () => {
    const n = rescueAgentsFromUi();
    if (!n) showToast('No agents under UI');
  });

  // Restore status label if a stored bg exists (image itself loads in setup/p5).
  try {
    if (localStorage.getItem(BG_STORAGE_KEY)) {
      updateBgStatus('Custom image · cover fit under grid');
    }
  } catch (_) {}

  if (true) Dock.wire();
  else refreshAgentRoster();
}
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', wireUiInteractions);
} else {
  wireUiInteractions();
}

export function windowResized() {
  resizeCanvas(windowWidth, windowHeight);
  // Keep pinned agents at their normalized scene coords; reflow free ones.
  for (const agent of agents.values()) {
    if (agent.pinned) applySavedPosition(agent) || clampAgentToFloor(agent);
  }
  layoutAgents();
  rescueAgentsFromUi({ silent: true });
}

// p5 global-mode entry points.
if (window.p5) window.p5.disableFriendlyErrors = true;
window.preload = preload;
window.setup = setup;
window.draw = draw;
window.mousePressed = mousePressed;
window.mouseDragged = mouseDragged;
window.mouseReleased = mouseReleased;
window.windowResized = windowResized;
