// Playwright smoke test for the visualizer.
// Env: VIS_URL (page URL incl. ?token=), NATS_URL, HUB_DELEGATE (binary path),
//      ARTIFACTS (dir for the screenshot).
// Asserts: page loads with no external network fetches, WS connects with the
// token, the echo agent appears, and a delegated message shows in the feed.
import { chromium } from 'playwright';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import path from 'node:path';

const run = promisify(execFile);
const VIS_URL = process.env.VIS_URL;
const NATS_URL = process.env.NATS_URL || 'nats://127.0.0.1:4222';
const DELEGATE = process.env.HUB_DELEGATE;
const ARTIFACTS = process.env.ARTIFACTS || 'artifacts';
if (!VIS_URL || !DELEGATE) throw new Error('VIS_URL and HUB_DELEGATE are required');

// The bridge serves a strict CSP (no eval), and Playwright's waitForFunction
// evaluates its predicate in the page, so poll from Node with page.evaluate
// (CDP calls are not subject to the page's CSP).
async function waitFor(fn, arg, { timeout = 15000 } = {}) {
  const end = Date.now() + timeout;
  for (;;) {
    if (await page.evaluate(fn, arg)) return;
    if (Date.now() > end) throw new Error(`timed out after ${timeout} ms: ${fn}`);
    await new Promise((r) => setTimeout(r, 200));
  }
}

const external = [];
const browser = await chromium.launch({ headless: true });
const page = await browser.newPage();
page.on('request', (req) => {
  const u = new URL(req.url());
  if (!['127.0.0.1', 'localhost', '[::1]'].includes(u.hostname) && u.protocol.startsWith('http')) {
    external.push(req.url());
  }
});
page.on('pageerror', (err) => console.error('PAGE ERROR:', err.message));
const cspViolations = [];
page.on('console', (msg) => {
  if (msg.type() === 'error' && /Content Security Policy/i.test(msg.text())) {
    cspViolations.push(msg.text());
  }
});

await page.goto(VIS_URL, { waitUntil: 'networkidle' });
await page.waitForSelector('canvas', { timeout: 15000 });

// WS connected with token.
await waitFor(
  () => document.getElementById('ws-text')?.textContent === 'LIVE',
  null, { timeout: 15000 },
);
console.log('ok: websocket LIVE');

// Delegate a real task to the echo worker (started by run_smoke.sh); its
// traffic must put the agent on the floor and show in the bus feed.
const { stdout } = await run(
  DELEGATE,
  ['--nats-url', NATS_URL, '--to', 'echo-1', '--prompt', 'visualizer smoke ping', '--timeout', '30'],
  { env: { ...process.env, NATS_URL }, timeout: 45000 },
);
console.log('delegate stdout:', stdout.trim().slice(0, 200));
await waitFor(
  () => Number(document.getElementById('agent-count')?.textContent || 0) >= 1,
  null, { timeout: 30000 },
);
console.log('ok: agent appeared');
await waitFor(
  () => (document.getElementById('event-ticker')?.textContent || '').includes('echo-1')
     || Number(document.getElementById('routed-count')?.textContent || 0) >= 2,
  null, { timeout: 30000 },
);
console.log('ok: delegated message visible in feed');

// UI path: open the AGENTS dock, open chat for echo-1, send a task through
// the bridge (ensure_worker + send_message), and see the echo reply.
await page.click('#agent-dock-toggle');
await page.click('#agent-roster-list [data-identity="echo-1"]');
await page.waitForSelector('#session-detail.open', { timeout: 10000 });
await page.fill('#chat-input', 'ui smoke hello');
const rowsBefore = await page.locator('#chat-messages .chat-row').count();
await page.click('#chat-send');
// New worker progress/terminal rows stream into the conversation.
await waitFor(
  (n) => document.querySelectorAll('#chat-messages .chat-row').length > n,
  rowsBefore, { timeout: 30000 },
);
console.log('ok: chat task via UI shows in conversation');

// XSS regression: any agent can broadcast; envelope text must render as
// text in the bus feed, never as markup (≤32 chars so it isn't truncated).
const XSS = '<img src=x onerror=__xss=1>';
await run(
  path.join(path.dirname(DELEGATE), 'hub-publish'),
  ['--nats-url', NATS_URL, '--channel', 'general', '--from', 'mallory',
   '--json', JSON.stringify({ message: XSS })],
  { env: { ...process.env, NATS_URL }, timeout: 20000 },
);
await waitFor(
  (x) => (document.getElementById('event-ticker')?.textContent || '').includes(x),
  XSS, { timeout: 15000 },
);
if (await page.evaluate(() => typeof window.__xss !== 'undefined')) {
  throw new Error('XSS: broadcast markup executed in the bus feed');
}
if (await page.locator('#event-ticker img').count()) {
  throw new Error('XSS: broadcast markup rendered as an element');
}
console.log('ok: broadcast markup renders as text (no XSS)');

if (cspViolations.length) throw new Error(`CSP violations: ${cspViolations.join(' | ')}`);
console.log('ok: no CSP violations');

if (external.length) throw new Error(`external network fetches: ${external.join(', ')}`);
console.log('ok: no external network fetches');

await page.screenshot({ path: path.join(ARTIFACTS, 'default-theme.png') });
console.log(`screenshot: ${path.join(ARTIFACTS, 'default-theme.png')}`);
await browser.close();
console.log('SMOKE PASS');
