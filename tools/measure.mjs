#!/usr/bin/env node
// Measure a stream the way a viewer sees it: opens a rein-remote page in headless Chrome (over the
// DevTools protocol, no dependencies), lets it play, and prints what its client measured: frames
// shown a second, glass to glass (with `serve --test-pattern` and `?measure`), the decoder, the
// display's refresh rate, and the server's own /stats.
//
//   node tools/measure.mjs 'http://127.0.0.1:7979/?measure' [--seconds 10] [--warmup 2]
//        [--chrome PATH] [--viewport 390x844] [--mobile] [--flags '--foo --bar']
//
// Prints one JSON object. Needs Node 22+ (WebSocket) and Chrome.

import {spawn} from 'node:child_process';
import {existsSync, mkdtempSync, rmSync} from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const argv = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : dflt;
};
const url = argv.find((a) => /^https?:/.test(a));
if (!url) {
  console.error('usage: node tools/measure.mjs URL [--seconds N] [--warmup N] [--chrome PATH] [--viewport WxH] [--mobile]');
  process.exit(2);
}
const seconds = Number(opt('seconds', 10));
const warmup = Number(opt('warmup', 2));
const chrome =
  opt('chrome') ??
  process.env.CHROME ??
  [
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/usr/bin/google-chrome',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
    'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
  ].find((p) => existsSync(p));
if (!chrome) throw new Error('no Chrome found (--chrome PATH)');
const [vw, vh] = (opt('viewport', '1400x900')).split('x').map(Number);

const profile = mkdtempSync(path.join(os.tmpdir(), 'rr-measure-'));
const proc = spawn(
  chrome,
  [
    '--headless=new',
    '--remote-debugging-port=0',
    `--user-data-dir=${profile}`,
    '--no-first-run',
    '--no-default-browser-check',
    '--autoplay-policy=no-user-gesture-required',
    // Keep animation frames and timers running at full rate in the background.
    '--disable-background-timer-throttling',
    '--disable-renderer-backgrounding',
    '--disable-backgrounding-occluded-windows',
    ...(opt('flags', '').split(' ').filter(Boolean)),
    'about:blank',
  ],
  {stdio: ['ignore', 'ignore', 'pipe']},
);
const cleanup = () => {
  proc.kill();
  setTimeout(() => rmSync(profile, {recursive: true, force: true}), 500);
};
process.on('exit', cleanup);

const wsUrl = await new Promise((resolve, reject) => {
  let err = '';
  const t = setTimeout(() => reject(new Error(`Chrome didn't start: ${err}`)), 15000);
  proc.stderr.on('data', (d) => {
    err += d;
    const m = err.match(/DevTools listening on (ws:\/\/\S+)/);
    if (m) clearTimeout(t), resolve(m[1]);
  });
});
const port = new URL(wsUrl).port;
const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
const page = targets.find((t) => t.type === 'page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let id = 0;
const waiting = new Map();
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && waiting.has(m.id)) waiting.get(m.id)(m), waiting.delete(m.id);
};
const send = (method, params = {}) =>
  new Promise((resolve, reject) => {
    const n = ++id;
    waiting.set(n, (m) => (m.error ? reject(new Error(`${method}: ${m.error.message}`)) : resolve(m.result)));
    ws.send(JSON.stringify({id: n, method, params}));
  });
const evaluate = async (expr) => (await send('Runtime.evaluate', {expression: expr, returnByValue: true, awaitPromise: true})).result.value;

await send('Emulation.setDeviceMetricsOverride', {width: vw, height: vh, deviceScaleFactor: argv.includes('--mobile') ? 3 : 1, mobile: argv.includes('--mobile')});
if (argv.includes('--mobile')) await send('Emulation.setTouchEmulationEnabled', {enabled: true, maxTouchPoints: 5});
await send('Page.navigate', {url});
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const statsExpr = `(() => { const s = window.reinRemote?.stats; return s && {...s, glassMs: [...s.glassMs]}; })()`;
// Until it's streaming.
for (let i = 0; i < 100 && (await evaluate(statsExpr))?.state !== 'streaming'; i++) await sleep(100);
await sleep(warmup * 1000);
const a = await evaluate(statsExpr);
const t0 = Date.now();
await sleep(seconds * 1000);
const b = await evaluate(statsExpr);
const dt = (Date.now() - t0) / 1000;
const glass = b?.glassMs?.slice(Math.max(0, b.glassMs.length - Math.max(0, (b.frames ?? 0) - (a?.frames ?? 0)))) ?? [];
glass.sort((x, y) => x - y);
const pct = (v, p) => (v.length ? v[Math.min(v.length - 1, Math.floor(v.length * p))] : null);
let server = null;
try {
  const u = new URL(url);
  server = await (await fetch(`${u.origin}/stats${u.search.includes('token=') ? '?' + u.search.slice(1).split('&').filter((kv) => kv.startsWith('token=')).join('&') : ''}`)).json();
} catch {}
console.log(
  JSON.stringify(
    {
      url,
      seconds: dt,
      state: b?.state,
      transport: b?.transport,
      decoder: b?.decoder,
      refreshHz: b?.refreshHz,
      framesShown: (b?.frames ?? 0) - (a?.frames ?? 0),
      fps: +(((b?.frames ?? 0) - (a?.frames ?? 0)) / dt).toFixed(1),
      mbit: +((((b?.bytes ?? 0) - (a?.bytes ?? 0)) * 8) / dt / 1e6).toFixed(2),
      glassMs: glass.length ? {p50: pct(glass, 0.5), p95: pct(glass, 0.95), samples: glass.length} : null,
      error: b?.error || undefined,
      server,
    },
    null,
    2,
  ),
);
ws.close();
cleanup();
process.exit(0);
