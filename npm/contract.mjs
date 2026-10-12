#!/usr/bin/env node
// Rein's contract with rein-remote, checked against the installed npm packages (run from a project
// that has @rein-harness/remote and @rein-harness/remote-server installed). Rein
// (rein-harness src/preview/remote.ts) starts the binary itself and relies on exactly this:
//
//   - `serve … --listen 127.0.0.1:0 --webtransport off`, the token in REIN_REMOTE_TOKEN, and a stderr
//     line with "on http://127.0.0.1:PORT" once it listens;
//   - GET /rein-remote.js (the web client, the same file as @rein-harness/remote), and /stream and
//     /stats refused without ?token=;
//   - the stream: the server's Hello, then after the client's Hello a Config and a keyframe;
//   - `serve --pipe WxH`: u32le-length-prefixed JPEGs on stdin, the viewer's input as JSON lines on stdout;
//   - `windows --json`: [{id, app, title, width, height}], or a non-zero exit with "rein-remote: why".
//
//   node contract.mjs [--windows required]

import {spawn} from 'node:child_process';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import path from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';

const require = createRequire(path.join(process.cwd(), 'noop.js'));
const here = path.dirname(fileURLToPath(import.meta.url));
const {binaryPath} = await import(pathToFileURL(require.resolve('@rein-harness/remote-server')).href);
const webJs = readFileSync(require.resolve('@rein-harness/remote/rein-remote.js'), 'utf8');
const version = JSON.parse(readFileSync(require.resolve('@rein-harness/remote-server/package.json'), 'utf8')).version;
const windowsRequired = process.argv.includes('--windows') && process.argv[process.argv.indexOf('--windows') + 1] === 'required';
const bin = binaryPath();
const token = 'contract-' + Math.random().toString(36).slice(2);
let failed = 0;
const ok = (cond, what) => (console.log(`${cond ? 'ok  ' : 'FAIL'} ${what}`), cond || failed++);

function run(args, opts = {}) {
  return new Promise((resolve) => {
    const p = spawn(bin, args, {stdio: ['ignore', 'pipe', 'pipe'], ...opts});
    let out = '', err = '';
    p.stdout.on('data', (d) => (out += d));
    p.stderr.on('data', (d) => (err += d));
    p.on('close', (code) => resolve({code, out, err}));
  });
}

/** Started as Rein starts it: resolves with the port from its stderr line. */
function serve(args, stdin = 'ignore') {
  const p = spawn(bin, ['serve', ...args, '--listen', '127.0.0.1:0', '--webtransport', 'off'], {stdio: [stdin, 'pipe', 'pipe'], env: {...process.env, REIN_REMOTE_TOKEN: token}});
  const port = new Promise((resolve, reject) => {
    let err = '';
    const timer = setTimeout(() => reject(new Error(`no port line in 10 s: ${err}`)), 10_000);
    p.on('close', (code) => reject(new Error(`exited ${code}: ${err}`)));
    p.stderr.setEncoding('utf8');
    p.stderr.on('data', (c) => {
      err += c;
      const m = err.match(/on http:\/\/127\.0\.0\.1:(\d+)/);
      if (m) clearTimeout(timer), resolve(Number(m[1]));
    });
  });
  return {p, port};
}

// The protocol's frames: kind u8, version u8, flags u8, len u32le, body.
const frame = (kind, body) => {
  const out = new Uint8Array(7 + body.length);
  out[0] = kind;
  new DataView(out.buffer).setUint32(3, body.length, true);
  out.set(body, 7);
  return out;
};
const hello = () => {
  const name = new TextEncoder().encode('contract');
  // versions 0..0, name, one codec (H.264, software), max 1920×1080 @ 60, no audio, pointer|keyboard|touch
  return frame(1, new Uint8Array([0, 0, 0, 0, name.length, 0, ...name, 1, 1, 0, 0x80, 7, 0x38, 4, 60, 0, 0, 7]));
};
function* frames(buf) {
  for (let at = 0; at + 7 <= buf.length; ) {
    const len = new DataView(buf.buffer, buf.byteOffset + at + 3, 4).getUint32(0, true);
    yield {kind: buf[at], flags: buf[at + 2], body: buf.subarray(at + 7, at + 7 + len)};
    at += 7 + len;
  }
}

/** Connects, says hello, and collects message kinds until `until(kinds)` or the timeout. */
function stream(port, {until, then} = {}) {
  return new Promise((resolve) => {
    const ws = new WebSocket(`ws://127.0.0.1:${port}/stream?token=${token}`);
    ws.binaryType = 'arraybuffer';
    const seen = [];
    let config;
    const done = () => (clearTimeout(timer), ws.close(), resolve({seen, config}));
    const timer = setTimeout(done, 10_000);
    ws.onopen = () => ws.send(hello());
    ws.onerror = () => done();
    ws.onmessage = (e) => {
      for (const f of frames(new Uint8Array(e.data))) {
        seen.push(f.kind === 3 && f.flags & 1 ? 'key' : f.kind);
        if (f.kind === 2) config = {codec: f.body[0], width: f.body[1] | (f.body[2] << 8), height: f.body[3] | (f.body[4] << 8)};
      }
      if (then && seen.includes('key')) then(ws), (then = undefined);
      if (until?.(seen)) done();
    };
  });
}

// 1. The binary and its version.
const v = await run(['--version']);
ok(v.code === 0 && v.out.trim() === `rein-remote ${version}`, `--version says ${version} (${v.out.trim()})`);

// 2. A stream as Rein starts it.
{
  const {p, port: portP} = serve(['--test-pattern', '320x240']);
  try {
    const port = await portP;
    ok(port > 0, `serve printed its port (${port})`);
    const js = await fetch(`http://127.0.0.1:${port}/rein-remote.js`).then((r) => r.text());
    ok(js === webJs, '/rein-remote.js is @rein-harness/remote\'s rein-remote.js');
    ok((await fetch(`http://127.0.0.1:${port}/stats`)).status === 403, '/stats without the token: 403');
    ok((await fetch(`http://127.0.0.1:${port}/stream`)).status === 403, '/stream without the token: 403');
    ok((await fetch(`http://127.0.0.1:${port}/stats?token=wrong${token}`)).status === 403, 'a wrong token: 403');
    ok((await fetch(`http://127.0.0.1:${port}/stats?token=${token}`)).status === 200, '/stats with the token: 200');
    const {seen, config} = await stream(port, {until: (s) => s.includes('key')});
    ok(seen[0] === 1, 'the server says Hello first');
    ok(config?.codec === 1 && config.width === 320 && config.height === 240, `Config: H.264 320×240 (${JSON.stringify(config)})`);
    ok(seen.includes('key'), 'a keyframe follows');
  } catch (err) {
    ok(false, `serve --test-pattern: ${err.message}`);
  } finally {
    p.kill();
  }
}

// 3. Frames from another program (Rein's headless browser): JPEGs in, input out as JSON lines.
{
  const {p, port: portP} = serve(['--pipe', '64x48'], 'pipe');
  try {
    const jpeg = readFileSync(path.join(here, 'fixtures', 'frame-64x48.jpg'));
    const len = Buffer.alloc(4);
    len.writeUInt32LE(jpeg.length);
    const feed = setInterval(() => p.stdin.write(Buffer.concat([len, jpeg])), 50);
    const port = await portP;
    let lines = '';
    p.stdout.on('data', (d) => (lines += d));
    const {seen} = await stream(port, {
      then: (ws) => {
        // Move to (10, 20), press and release the left button.
        ws.send(frame(6, new Uint8Array([1, 10, 0, 20, 0])));
        ws.send(frame(6, new Uint8Array([3, 0, 1])));
        ws.send(frame(6, new Uint8Array([3, 0, 0])));
        setTimeout(() => ws.close(), 500);
      },
      until: () => lines.includes('"up"'),
    });
    clearInterval(feed);
    ok(seen.includes('key'), '--pipe: JPEGs in, video out');
    const events = lines.trim().split('\n').filter(Boolean).map((l) => JSON.parse(l));
    ok(events.some((e) => e.type === 'mouse' && e.action === 'down' && e.x === 10 && e.y === 20 && e.button === 'left'), `--pipe: a click comes back as JSON (${events.length} events)`);
  } catch (err) {
    ok(false, `serve --pipe: ${err.message}`);
  } finally {
    p.kill();
  }
}

// 4. The windows it can show.
{
  const w = await run(['windows', '--json']);
  if (w.code === 0) {
    let list;
    try {
      list = JSON.parse(w.out);
    } catch {}
    ok(Array.isArray(list), `windows --json is a JSON array (${list?.length} windows)`);
    const shape = (x) => typeof x.id === 'number' && typeof x.app === 'string' && typeof x.title === 'string' && typeof x.width === 'number' && typeof x.height === 'number';
    ok((list ?? []).every(shape), 'each window has id, app, title, width, height');
    if (windowsRequired) ok(list?.length > 0, 'windows --json lists at least one window');
  } else {
    ok(!windowsRequired && /^rein-remote: /m.test(w.err), `windows --json failed, saying why (${w.err.trim().split('\n').pop()})`);
  }
}

console.log(failed ? `${failed} failed` : 'Rein contract: all good');
process.exit(failed ? 1 : 0);
