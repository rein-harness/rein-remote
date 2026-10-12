#!/usr/bin/env node
// Pairing, end to end in a real browser: starts `rein-remote serve` (listening on the network, so it
// pairs), opens its page in headless Chrome on the plain-http:// local-network address (no WebCrypto
// there: pair.js does it all), enters a wrong code, then the right one, waits for video; then opens
// the page again with no code and no token (it must sign in with the key it kept) and waits for video.
//
//   node tools/pair-e2e.mjs [path/to/rein-remote] [--host 192.168.1.20]

import {spawn} from 'node:child_process';
import {existsSync, mkdtempSync, rmSync} from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const argv = process.argv.slice(2);
const bin = argv.find((a) => !a.startsWith('--') && existsSync(a)) ?? 'target/release/rein-remote';
const lan =
  argv[argv.indexOf('--host') + 1] && argv.includes('--host')
    ? argv[argv.indexOf('--host') + 1]
    : Object.values(os.networkInterfaces()).flat().find((i) => i && i.family === 'IPv4' && !i.internal)?.address;
const chrome = ['/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', '/usr/bin/google-chrome', '/usr/bin/chromium'].find((p) => existsSync(p));
const home = mkdtempSync(path.join(os.tmpdir(), 'rr-pair-home-'));
const steps = [];
const ok = (cond, what, extra = {}) => (steps.push({ok: !!cond, what, ...extra}), console.log(`${cond ? 'ok  ' : 'FAIL'} ${what}`));

const srv = spawn(bin, ['serve', '--test-pattern', '1280x720', '--listen', '0.0.0.0:0', '--tailscale-cert', 'off', '--mdns', 'off'], {
  env: {...process.env, REIN_REMOTE_HOME: home, REIN_REMOTE_TOKEN: ''},
  stdio: ['ignore', 'ignore', 'pipe'],
});
let log = '';
const {port, code} = await new Promise((resolve, reject) => {
  const t = setTimeout(() => reject(new Error(`no code in 15 s: ${log}`)), 15000);
  srv.stderr.on('data', (d) => {
    log += d;
    const p = log.match(/on http:\/\/0\.0\.0\.0:(\d+)/), c = log.match(/enter (\d{3}-\d{3})/);
    if (p && c) clearTimeout(t), resolve({port: +p[1], code: c[1]});
  });
});
const origin = `http://${lan}:${port}`;
console.log(`server on ${origin}, code ${code}`);

const profile = mkdtempSync(path.join(os.tmpdir(), 'rr-pair-chrome-'));
const ch = spawn(chrome, ['--headless=new', '--remote-debugging-port=0', `--user-data-dir=${profile}`, '--no-first-run', 'about:blank'], {stdio: ['ignore', 'ignore', 'pipe']});
const cleanup = () => (ch.kill(), srv.kill(), setTimeout(() => (rmSync(profile, {recursive: true, force: true}), rmSync(home, {recursive: true, force: true})), 500));
process.on('exit', cleanup);
const ws0 = await new Promise((r) => {
  let e = '';
  ch.stderr.on('data', (d) => ((e += d), e.match(/ws:\/\/\S+/) && r(e.match(/ws:\/\/\S+/)[0])));
});
const page = (await (await fetch(`http://127.0.0.1:${new URL(ws0).port}/json/list`)).json()).find((t) => t.type === 'page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let id = 0;
const waits = new Map();
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (waits.has(m.id)) waits.get(m.id)(m.result), waits.delete(m.id);
};
const send = (method, params = {}) => new Promise((r) => (waits.set(++id, r), ws.send(JSON.stringify({id, method, params}))));
const evaluate = async (e) => (await send('Runtime.evaluate', {expression: e, returnByValue: true, awaitPromise: true})).result?.value;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const until = async (expr, ms = 10000) => {
  for (const end = Date.now() + ms; Date.now() < end; await sleep(100)) if (await evaluate(expr)) return true;
  return false;
};
const frames = `window.reinRemote?.stats?.frames ?? 0`;

// 1. No token, not paired: the page asks for the code. A wrong one is refused.
await send('Page.navigate', {url: `${origin}/`});
ok(await until(`getComputedStyle(document.getElementById('pair')).display === 'flex'`), 'the page asks for a code');
ok(await evaluate(`!window.isSecureContext && !crypto.subtle`), 'plain http on the local network: no WebCrypto (pair.js does the math)');
await evaluate(`(document.getElementById('code').value = '000-001', document.getElementById('pair').requestSubmit(), true)`);
ok(await until(`/match|code/i.test(document.getElementById('why').textContent) && !/pairing…/.test(document.getElementById('why').textContent)`), 'a wrong code is refused', {why: await evaluate(`document.getElementById('why').textContent`)});
// 2. The right code: paired, signed in, video.
await evaluate(`(document.getElementById('code').value = '${code}', document.getElementById('pair').requestSubmit(), true)`);
ok(await until(`${frames} > 10`, 15000), 'the right code: paired, signed in, and video plays', {frames: await evaluate(frames)});
ok(/paired with/.test(log), 'the server says it paired');
// 3. Again, nothing typed: the kept key signs in.
await send('Page.navigate', {url: `${origin}/`});
await sleep(500);
ok(await until(`${frames} > 10`, 15000), 'opened again: signs in with its key, no code, no token', {frames: await evaluate(frames)});
ok(await evaluate(`getComputedStyle(document.getElementById('pair')).display === 'none'`), 'no code asked the second time');
// 4. The stream refuses anyone without a token or ticket.
const r = await fetch(`${origin}/stats`);
ok(r.status === 403, 'without a token or ticket: 403');
const failed = steps.filter((s) => !s.ok).length;
console.log(JSON.stringify({steps}, null, 1));
console.log(failed ? `${failed} failed` : 'pairing e2e: all good');
process.exit(failed ? 1 : 0);
