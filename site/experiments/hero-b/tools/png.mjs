// PNG encoders for the baked stills (no dependencies).
//  - encodeIndexed: trails over transparency as an 8-bit indexed PNG. Every
//    pixel is the sent colour, the received colour or a mix of the two at
//    some alpha, so a 256-entry palette (3 hues × 85 alphas + transparent)
//    holds it almost exactly. Input is premultiplied RGBA.
//  - encodePng: plain RGBA (previews).
import { deflateSync } from "node:zlib";

export function encodeIndexed(w, h, px, { teal, orange }) {
  const HUES = 3;
  const ALPHAS = 85;
  const d = teal.map((t, i) => orange[i] - t);
  const dd = d.reduce((s, v) => s + v * v, 0);
  const plte = [0, 0, 0];
  const trns = [0];
  for (let s = 0; s < HUES; s++) {
    for (let k = 1; k <= ALPHAS; k++) {
      const share = s / (HUES - 1);
      for (let c = 0; c < 3; c++) plte.push(Math.round((teal[c] + d[c] * share) * 255));
      trns.push(Math.round((k / ALPHAS) * 255));
    }
  }
  const idx = new Uint8Array(w * h);
  for (let i = 0, p = 0; p < idx.length; i += 4, p++) {
    const a = px[i + 3] / 255;
    const k = Math.round(a * ALPHAS);
    if (k === 0) continue;
    // Project the un-premultiplied colour onto the teal → orange line.
    let dot = 0;
    for (let c = 0; c < 3; c++) dot += (px[i + c] / 255 / a - teal[c]) * d[c];
    const share = Math.min(1, Math.max(0, dot / dd));
    idx[p] = 1 + Math.round(share * (HUES - 1)) * ALPHAS + (k - 1);
  }
  const raw = Buffer.alloc((w + 1) * h);
  for (let y = 0; y < h; y++) raw.set(idx.subarray(y * w, (y + 1) * w), y * (w + 1) + 1);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr.set([8, 3, 0, 0, 0], 8);
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk("IHDR", ihdr),
    chunk("PLTE", Buffer.from(plte)),
    chunk("tRNS", Buffer.from(trns)),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

export function encodePng(w, h, rgba) {
  const raw = Buffer.alloc((w * 4 + 1) * h);
  for (let y = 0; y < h; y++) {
    // Filter 1 (sub) per row: soft light compresses much better as deltas.
    const row = y * (w * 4 + 1);
    raw[row] = 1;
    for (let x = 0; x < w * 4; x++) {
      const v = rgba[y * w * 4 + x];
      const left = x >= 4 ? rgba[y * w * 4 + x - 4] : 0;
      raw[row + 1 + x] = (v - left) & 255;
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr.set([8, 6, 0, 0, 0], 8);
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
}

function crc32(buf) {
  let c = ~0;
  for (const b of buf) {
    c ^= b;
    for (let k = 0; k < 8; k++) c = (c >>> 1) ^ (0xedb88320 & -(c & 1));
  }
  return ~c >>> 0;
}
