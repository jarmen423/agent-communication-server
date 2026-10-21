import { S, agents, particleEffects, COLORS } from './state.js';
export class Particle {
  constructor(x, y, color) {
    this.x = x; this.y = y;
    this.vx = (Math.random() - 0.5) * 3.5;
    this.vy = (Math.random() - 0.5) * 3.5;
    this.life = 1.0;
    this.color = color;
    this.size = Math.random() * 3 + 1;
  }
  update() {
    this.x += this.vx;
    this.y += this.vy;
    this.vx *= 0.95;
    this.vy *= 0.95;
    this.life -= 0.025;
  }
  display() {
    noStroke();
    fill(this.color[0], this.color[1], this.color[2], this.life * 180);
    circle(this.x, this.y, this.size * this.life);
  }
}

export function spawnParticles(x, y, color, count) {
  const room = MAX_PARTICLES - particleEffects.length;
  const n = Math.min(count, Math.max(0, room));
  for (let i = 0; i < n; i++) {
    particleEffects.push(new Particle(x, y, color));
  }
}

// === Active Real Transmission System (Connecting lines) ===
export class Transmission {
  constructor(fromId, toId, kind) {
    this.fromId = fromId;
    this.toId = toId;
    this.kind = kind;
    this.startTime = Date.now();
    this.duration = 1200; // Animation takes 1.2 seconds
    
    // Choose color token based on NATS message kind
    if (kind === 'message') this.color = COLORS.agent.ready;
    else if (kind === 'event') this.color = COLORS.agent.thinking;
    else if (kind === 'status') this.color = COLORS.agent.working;
    else if (kind === 'error') this.color = COLORS.agent.error;
    else this.color = [200, 200, 220];
  }

  isExpired() {
    return Date.now() - this.startTime > this.duration;
  }

  display() {
    const fromAgent = agents.get(this.fromId);
    const toAgent = agents.get(this.toId);
    if (!fromAgent || !toAgent) return;

    let progress = (Date.now() - this.startTime) / this.duration;
    if (progress > 1.0) progress = 1.0;

    // Fade out opacity at end of transition
    let alpha = 180;
    if (progress > 0.8) {
      alpha = map(progress, 0.8, 1.0, 180, 0);
    }

    push();
    // 1. Draw connecting link line
    stroke(this.color[0], this.color[1], this.color[2], alpha * 0.35);
    strokeWeight(1.5);
    line(fromAgent.x, fromAgent.y, toAgent.x, toAgent.y);

    // 2. Interpolate packet position
    let px = lerp(fromAgent.x, toAgent.x, progress);
    let py = lerp(fromAgent.y, toAgent.y, progress);

    // 3. Draw moving glow packet
    noStroke();
    fill(this.color[0], this.color[1], this.color[2], alpha);
    drawingContext.shadowBlur = 10;
    drawingContext.shadowColor = `rgb(${this.color[0]}, ${this.color[1]}, ${this.color[2]})`;
    circle(px, py, 6);
    drawingContext.shadowBlur = 0;
    pop();
  }
}

// Draw a temporary broadcast ripple expanding from worker
export class BroadcastRipple {
  constructor(x, y, color) {
    this.x = x; this.y = y;
    this.color = color;
    this.startTime = Date.now();
    this.duration = 1000;
  }
  isExpired() {
    return Date.now() - this.startTime > this.duration;
  }
  display() {
    let progress = (Date.now() - this.startTime) / this.duration;
    let diameter = progress * 150;
    let alpha = map(progress, 0, 1, 150, 0);

    push();
    noFill();
    stroke(this.color[0], this.color[1], this.color[2], alpha);
    strokeWeight(2);
    circle(this.x, this.y, diameter);
    pop();
  }
}

// === Arrange free (unpinned) agents in a stable circle ===
