import { S, agents, transmissions, broadcastRipples, particleEffects, COLORS } from './state.js';
import { refreshAgentRoster } from './chat.js';
export function layoutAgents() {
  const free = [...agents.values()].filter((a) => !a.pinned && !a.dragging);
  const count = free.length;
  if (count === 0) return;

  const cx = width / 2;
  const cy = height / 2;
  const radius = Math.min(width, height) * 0.28;

  free.forEach((agent, i) => {
    if (count === 1) {
      agent.targetX = cx;
      agent.targetY = cy;
    } else {
      const angle = (i / count) * Math.PI * 2 - Math.PI / 2;
      agent.targetX = cx + Math.cos(angle) * radius;
      agent.targetY = cy + Math.sin(angle) * radius;
    }
  });
}

// === p5.js Setup ===
// === p5.js Draw Loop ===
export function draw() {
  const dt = deltaTime;

  // Background (optional user image, cover-fit) + floor grid
  background(COLORS.bg[0], COLORS.bg[1], COLORS.bg[2]);
  if (S.sceneBgImage && S.sceneBgImage.width) {
    const iw = S.sceneBgImage.width;
    const ih = S.sceneBgImage.height;
    const scale = Math.max(width / iw, height / ih);
    const dw = iw * scale;
    const dh = ih * scale;
    push();
    imageMode(CENTER);
    image(S.sceneBgImage, width / 2, height / 2, dw, dh);
    // Dim so sprites/grid stay readable
    noStroke();
    fill(6, 6, 10, 120);
    rect(0, 0, width, height);
    pop();
  }

  // Floor grid — light accent every 4th line
  const gridSize = 45;
  const gridAlpha = S.sceneBgActive ? 36 : 45;
  const accentAlpha = S.sceneBgActive ? 55 : 70;
  strokeWeight(0.5);
  for (let x = 0; x < width; x += gridSize) {
    const accent = (x / gridSize) % 4 === 0;
    const c = accent ? COLORS.gridAccent : COLORS.grid;
    stroke(c[0], c[1], c[2], accent ? accentAlpha : gridAlpha);
    line(x, 0, x, height);
  }
  for (let y = 0; y < height; y += gridSize) {
    const accent = (y / gridSize) % 4 === 0;
    const c = accent ? COLORS.gridAccent : COLORS.grid;
    stroke(c[0], c[1], c[2], accent ? accentAlpha : gridAlpha);
    line(0, y, width, y);
  }

  // Remove stale agents (60s timeout)
  const now = Date.now();
  let deletedAny = false;
  for (const [id, agent] of agents) {
    if (now - agent.lastSeen > 900000) { // 15m — long model runs go silent mid-turn
      agents.delete(id);
      deletedAny = true;
    }
  }
  if (deletedAny) {
    layoutAgents();
    document.getElementById('agent-count').textContent = agents.size;
    refreshAgentRoster();
  }

  // Toggle empty state panel visibility
  const emptyStateEl = document.getElementById('empty-state');
  if (agents.size === 0) {
    if (emptyStateEl && emptyStateEl.style.opacity !== '1') {
      emptyStateEl.style.opacity = '1';
    }
  } else {
    if (emptyStateEl && emptyStateEl.style.opacity !== '0') {
      emptyStateEl.style.opacity = '0';
    }
  }

  // Update + Draw agents
  for (const agent of agents.values()) {
    agent.update(dt);
    agent.display();
  }

  // Draw active real-time transmission lines & packet pulses
  for (let i = transmissions.length - 1; i >= 0; i--) {
    if (transmissions[i].isExpired()) {
      transmissions.splice(i, 1);
    } else {
      transmissions[i].display();
    }
  }

  // Draw broadcast ripples
  for (let i = broadcastRipples.length - 1; i >= 0; i--) {
    if (broadcastRipples[i].isExpired()) {
      broadcastRipples.splice(i, 1);
    } else {
      broadcastRipples[i].display();
    }
  }

  // Draw visual burst particles
  for (let i = particleEffects.length - 1; i >= 0; i--) {
    particleEffects[i].update();
    particleEffects[i].display();
    if (particleEffects[i].life <= 0) particleEffects.splice(i, 1);
  }

  // Mouse hover tracking + grab cursor
  S.hoveredAgent = null;
  if (!(S.pointerGesture && S.pointerGesture.mode === 'dragging')) {
    for (const agent of agents.values()) {
      if (agent.contains(mouseX, mouseY)) {
        S.hoveredAgent = agent;
        break;
      }
    }
  }
  if (S.pointerGesture && S.pointerGesture.mode === 'dragging') {
    cursor('grabbing');
  } else if (S.hoveredAgent) {
    cursor('grab');
  } else {
    cursor('default');
  }
}

