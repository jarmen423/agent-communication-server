import { S, agents, reducedMotion, COLORS, STATUS_LABELS, BUBBLE_MAX_W, MAX_BUBBLE_LINES } from './state.js';
import { agentPalette } from './state.js';
import { Theme } from './theme.js';
import { wrapText } from './layout.js';
import { agentGlyph } from './state.js';
export class Agent {
  constructor(identity) {
    this.identity = identity;
    this.x = 0; this.y = 0;
    this.targetX = 0; this.targetY = 0;
    this.pinned = false;
    this.dragging = false;
    this.status = 'idle';
    this.statusHoldUntil = 0;
    this.snippet = null;
    this.snippetTime = 0;
    this.size = 32;
    this.bobOffset = Math.random() * Math.PI * 2;
    this.pulsePhase = 0;
    this.baseColor = agentPalette(identity);
    this.color = this.baseColor;
    this.lastSeen = Date.now();
    this.sessionId = null;
    this.lastTaskChannel = null;
    this.messageCount = 0;
    this.trail = [];
    this.stopped = false;
    this.glyph = agentGlyph(identity);
    this.petSlug = Theme.resolveVariant(identity);
  }

  update(dt) {
    // While dragging, stick to pointer path (targets set in mouse handlers).
    if (!this.dragging) {
      this.x += (this.targetX - this.x) * 0.05;
      this.y += (this.targetY - this.y) * 0.05;

      // Gentle drift only for auto-layout (unpinned) agents.
      if (!this.pinned && !reducedMotion) {
        this.x += Math.sin(this.bobOffset + Date.now() * 0.0008) * 0.25;
        this.y += Math.cos(this.bobOffset + Date.now() * 0.0012) * 0.15;
      }
    }

    // Smooth movement trail
    this.trail.push({ x: this.x, y: this.y, life: 1.0 });
    if (this.trail.length > 12) this.trail.shift();
    this.trail.forEach(t => t.life -= 0.08);
    this.trail = this.trail.filter(t => t.life > 0);

    const active = this.status === 'working' || this.status === 'thinking' || this.status === 'ready' || this.status === 'error';
    this.pulsePhase += dt * (active ? 0.006 : 0.002);

    // After status window hold, decay status back to idle
    if (this.statusHoldUntil && Date.now() > this.statusHoldUntil && this.status !== 'error') {
      this.status = 'idle';
      this.statusHoldUntil = 0;
    }

    if (this.stopped) {
      this.color = COLORS.agent.stopped;
    } else if (this.status === 'working' || this.status === 'thinking' || this.status === 'ready' || this.status === 'error') {
      this.color = COLORS.agent[this.status] || this.baseColor;
    } else {
      // Idle: identity hue at ~65% saturation — crisp, not muddy gray
      const breathe = reducedMotion ? 1 : Math.sin(Date.now() * 0.0018) * 0.06 + 0.94;
      this.color = [
        Math.round((this.baseColor[0] * 0.62 + 28) * breathe),
        Math.round((this.baseColor[1] * 0.62 + 32) * breathe),
        Math.round((this.baseColor[2] * 0.62 + 42) * breathe),
      ];
    }

    // Fade snippet speech bubbles after 7 seconds
    if (this.snippet && Date.now() - this.snippetTime > 7000) {
      this.snippet = null;
    }
  }

  // Draw custom octagonal micro-chip
  drawOctagon(x, y, size) {
    let offset = size / 2;
    let chamfer = size * 0.25;
    beginShape();
    vertex(x - offset + chamfer, y - offset);
    vertex(x + offset - chamfer, y - offset);
    vertex(x + offset, y - offset + chamfer);
    vertex(x + offset, y + offset - chamfer);
    vertex(x + offset - chamfer, y + offset);
    vertex(x - offset + chamfer, y + offset);
    vertex(x - offset, y + offset - chamfer);
    vertex(x - offset, y - offset + chamfer);
    endShape(CLOSE);
  }

  display() {
    push();

    // 1. Draw trail
    noFill();
    for (let i = 0; i < this.trail.length; i++) {
      const t = this.trail[i];
      const alpha = t.life * 25;
      stroke(this.color[0], this.color[1], this.color[2], alpha);
      strokeWeight(1);
      if (i > 0) {
        const prev = this.trail[i - 1];
        line(prev.x, prev.y, t.x, t.y);
      }
    }

    const active = this.status === 'working' || this.status === 'thinking' || this.status === 'ready' || this.status === 'error';

    // 1b. Active pulse ring (compositor-friendly scale via radius)
    if (active && !this.stopped && !reducedMotion) {
      const pulse = Math.sin(this.pulsePhase * 2.5) * 0.5 + 0.5;
      noFill();
      stroke(this.color[0], this.color[1], this.color[2], 40 + pulse * 80);
      strokeWeight(1.5);
      circle(this.x, this.y, this.size * (1.6 + pulse * 0.35));
    }

    // 2. Active status orbiters (status dots — not the pet body)
    if (active && !this.stopped && !reducedMotion) {
      let orbCount = this.status === 'working' ? 2 : 1;
      // rad/sec; Date.now()*0.04 was hyperspin
      let speed = this.status === 'working' ? 2.2 : 1.1;
      let radius = Math.max(this.size * 0.95, Theme.metrics().hitRadius * 1.15);
      for (let i = 0; i < orbCount; i++) {
        let angle = (millis() * 0.001 * speed) + (i * Math.PI);
        let ox = this.x + Math.cos(angle) * radius;
        let oy = this.y + Math.sin(angle) * radius;

        noStroke();
        fill(this.color[0], this.color[1], this.color[2], 180);
        circle(ox, oy, 4);
      }
    }

    // 3. Glow + body (pet sprite or chip fallback)
    drawingContext.shadowBlur = active ? 26 : 4;
    drawingContext.shadowColor = `rgba(${this.color[0]}, ${this.color[1]}, ${this.color[2]}, ${active ? 0.85 : 0.18})`;

    const drewPet = Theme.drawAgent(this);
    const petMetrics = Theme.metrics();
    if (!drewPet) {
      noStroke();
      fill(this.color[0], this.color[1], this.color[2]);
      this.drawOctagon(this.x, this.y, this.size);
      drawingContext.shadowBlur = 0;
      fill(10, 10, 20);
      this.drawOctagon(this.x, this.y, this.size - 6);
      fill(this.color[0], this.color[1], this.color[2]);
      textAlign(CENTER, CENTER);
      textFont(COLORS.fontBody || 'monospace');
      textSize(13);
      textStyle(BOLD);
      text(this.glyph, this.x, this.y + 1);
      textStyle(NORMAL);
    } else {
      drawingContext.shadowBlur = 0;
    }

    // 7. Blinking status LED (top-right of body or sprite)
    let ledX = drewPet ? this.x + petMetrics.width * 0.32 : this.x + this.size * 0.28;
    let ledY = drewPet ? this.y - petMetrics.height * 0.36 : this.y - this.size * 0.28;
    let ledOn = (Math.floor(Date.now() / 300) % 2 === 0);
    if (this.stopped) {
      fill(40, 40, 50); // off
    } else if (!ledOn && active) {
      fill(this.color[0] * 0.4, this.color[1] * 0.4, this.color[2] * 0.4);
    } else {
      fill(this.color[0], this.color[1], this.color[2]);
    }
    circle(ledX, ledY, 5);

    // 8. Hover ring
    if (S.hoveredAgent === this) {
      noFill();
      stroke(255, 255, 255, 120);
      strokeWeight(1.5);
      if (drewPet) {
        circle(this.x, this.y, petMetrics.hitRadius * 3.6);
      } else {
        this.drawOctagon(this.x, this.y, this.size * 2.0);
      }
    }

    // 9. Selection indicator bracket
    if (S.selectedAgent === this) {
      noFill();
      stroke(0, 217, 255, 200);
      strokeWeight(2);
      let s = this.size * 2.2;
      let d = 8;
      // Draw corners of selection box
      line(this.x - s/2, this.y - s/2, this.x - s/2 + d, this.y - s/2);
      line(this.x - s/2, this.y - s/2, this.x - s/2, this.y - s/2 + d);
      
      line(this.x + s/2, this.y - s/2, this.x + s/2 - d, this.y - s/2);
      line(this.x + s/2, this.y - s/2, this.x + s/2, this.y - s/2 + d);

      line(this.x - s/2, this.y + s/2, this.x - s/2 + d, this.y + s/2);
      line(this.x - s/2, this.y + s/2, this.x - s/2, this.y + s/2 - d);

      line(this.x + s/2, this.y + s/2, this.x + s/2 - d, this.y + s/2);
      line(this.x + s/2, this.y + s/2, this.x + s/2, this.y + s/2 - d);
    }

    // 10. Identity label + status chip
    const labelY = this.y + (drewPet ? petMetrics.labelOffset : this.size * 0.75 + 4);
    fill(this.baseColor[0], this.baseColor[1], this.baseColor[2], 220);
    textAlign(CENTER, TOP);
    textSize(10);
    textFont('Share Tech Mono');
    text(this.identity.substring(0, 18).toUpperCase(), this.x, labelY);

    this.drawStatusChip(labelY + 13);

    if (this.sessionId) {
      fill(COLORS.agent.ready[0], COLORS.agent.ready[1], COLORS.agent.ready[2], 160);
      textSize(8);
      text(`SES ${this.sessionId.substring(0, 8)}`, this.x, labelY + 28);
    }

    // 11. Custom Speech Thought Bubble
    if (this.snippet) {
      this.drawBubble();
    }

    pop();
  }

  drawStatusChip(y) {
    const label = this.stopped ? STATUS_LABELS.stopped : (STATUS_LABELS[this.status] || 'IDLE');
    const chipColor = this.stopped ? COLORS.agent.stopped : (COLORS.agent[this.status] || this.baseColor);
    textSize(7);
    textFont('Share Tech Mono');
    const chipW = textWidth(label) + 14;
    const chipH = 11;
    rectMode(CENTER);
    noStroke();
    fill(chipColor[0], chipColor[1], chipColor[2], 45);
    rect(this.x, y + chipH / 2, chipW, chipH, 3);
    stroke(chipColor[0], chipColor[1], chipColor[2], 140);
    strokeWeight(1);
    noFill();
    rect(this.x, y + chipH / 2, chipW, chipH, 3);
    noStroke();
    fill(chipColor[0], chipColor[1], chipColor[2], 230);
    textAlign(CENTER, CENTER);
    text(label, this.x, y + chipH / 2 + 0.5);
  }

  // Thought bubble — max width, capped lines, fade-out
  drawBubble() {
    const paddingX = 11;
    const paddingY = 7;
    const lineSpacing = 14;

    push();
    textSize(11);
    textFont('JetBrains Mono');
    textStyle(NORMAL);

    let lines = wrapText(this.snippet, BUBBLE_MAX_W - paddingX * 2);
    if (lines.length > MAX_BUBBLE_LINES) {
      lines = lines.slice(0, MAX_BUBBLE_LINES);
      const last = lines[MAX_BUBBLE_LINES - 1];
      lines[MAX_BUBBLE_LINES - 1] = last.length > 28 ? last.substring(0, 26) + '…' : last + '…';
    }

    let w = Math.min(BUBBLE_MAX_W, 72);
    for (const line of lines) {
      w = Math.max(w, Math.min(BUBBLE_MAX_W, textWidth(line) + paddingX * 2));
    }
    const h = lines.length * lineSpacing + paddingY * 2;
    const bubbleY = this.y - this.size - h / 2 - 18;

    const age = Date.now() - this.snippetTime;
    let alpha = 245;
    if (age > 5000) alpha = Math.max(0, map(age, 5000, 7000, 245, 0));

    fill(COLORS.bubbleBg[0], COLORS.bubbleBg[1], COLORS.bubbleBg[2], alpha);
    stroke(this.baseColor[0], this.baseColor[1], this.baseColor[2], alpha * 0.55);
    strokeWeight(1.5);
    rectMode(CENTER);

    if (!reducedMotion) {
      drawingContext.shadowBlur = 6;
      drawingContext.shadowColor = `rgba(${this.baseColor[0]}, ${this.baseColor[1]}, ${this.baseColor[2]}, ${alpha / 255 * 0.25})`;
    }
    rect(this.x, bubbleY, w, h, 7);
    
    // Small pointer triangle
    noStroke();
    fill(COLORS.bubbleBg[0], COLORS.bubbleBg[1], COLORS.bubbleBg[2], alpha);
    triangle(
      this.x - 5, bubbleY + h * 0.5,
      this.x + 5, bubbleY + h * 0.5,
      this.x, bubbleY + h * 0.5 + 6
    );
    
    // Re-draw overlapping pointer borders
    stroke(this.color[0], this.color[1], this.color[2], alpha * 0.6);
    strokeWeight(1.5);
    line(this.x - 5, bubbleY + h * 0.5, this.x, bubbleY + h * 0.5 + 6);
    line(this.x + 5, bubbleY + h * 0.5, this.x, bubbleY + h * 0.5 + 6);
    
    drawingContext.shadowBlur = 0; // reset
    
    noStroke();
    fill(228, 234, 244, alpha);
    textAlign(LEFT, CENTER);
    const startY = bubbleY - h / 2 + paddingY + lineSpacing / 2;
    for (let i = 0; i < lines.length; i++) {
      text(lines[i], this.x - w / 2 + paddingX, startY + i * lineSpacing);
    }

    pop();
  }

  setSnippet(text) {
    this.snippet = text;
    this.snippetTime = Date.now();
  }

  contains(mx, my) {
    const r = Theme.hitRadius(this.petSlug) || this.size;
    return Math.abs(mx - this.x) < r && Math.abs(my - this.y) < r;
  }
}

// === Particle Effect ===
