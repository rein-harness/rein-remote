// The phone: a 390×844 touch screen (DPR 3) in headless Chromium, reaching Rein only through Caddy.
// Signs in, starts a chat, adds a preview of display :1 (an xterm) with "+ Preview", checks that the
// FIRST open streams through Rein Remote (video, decoded here) with the bar's "· video" matching what
// plays, taps into the xterm and types a command, measures, checks /preview :2 streams the same way, that
// a display Rein Remote can't stream (:3, with a VNC password) falls back to frames, and records
// everything the page received (page-received.jsonl) for run.sh to search for secrets. Writes /out/result.json and screenshots to /out. Env: URL, REIN_USER, REIN_PASS, MEASURE_S.
import {mkdirSync, writeFileSync} from 'node:fs';
import puppeteer from 'puppeteer-core';

const URL0 = process.env.URL ?? 'https://rein.test:8443/';
const USER = process.env.REIN_USER ?? 'p2';
const PASS = process.env.REIN_PASS ?? 'phase-two-proof';
const MEASURE_S = Number(process.env.MEASURE_S ?? 10);
const OUT = '/out';
mkdirSync(OUT, {recursive: true});
const t0 = Date.now();
const log = (...a) => console.log(`[${((Date.now() - t0) / 1000).toFixed(1)}s]`, ...a);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const result = {url: URL0, viewport: null, steps: [], network: {}, client: {}};
const step = (name, ok, extra = {}) => (result.steps.push({name, ok, ...extra}), log(ok ? 'OK  ' : 'FAIL', name, Object.keys(extra).length ? JSON.stringify(extra) : ''));
const save = () => writeFileSync(`${OUT}/result.json`, JSON.stringify(result, null, 2));

const browser = await puppeteer.launch({
  executablePath: '/usr/bin/chromium',
  acceptInsecureCerts: true, // Caddy's own CA (tls internal): a real phone would have a real certificate
  args: ['--no-sandbox', '--headless=new', '--ignore-certificate-errors', '--use-angle=swiftshader', '--enable-unsafe-swiftshader'],
});
const page = await browser.newPage();
const viewport = {width: 390, height: 844, deviceScaleFactor: Number(process.env.DSF ?? 3), isMobile: true, hasTouch: true};
await page.setViewport(viewport);
await page.setUserAgent('Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/154.0.0.0 Mobile Safari/537.36');
result.viewport = viewport;
page.on('console', (m) => log('console', m.type(), m.text().slice(0, 300)));
page.on('pageerror', (e) => log('pageerror', e.message));

// Everything on the wire, from the browser's side (CDP Network).
const cdp = await page.createCDPSession();
await cdp.send('Network.enable');
const reqs = [];
const requestOps = [];
const opsById = new Map(); // CDP requestId → {op, args}
const addReplies = []; // the preview-add replies, in order
// Everything the page received from Rein: every /api response body and every event-stream message.
const received = [];
const apiUrls = new Map();
cdp.on('Network.requestWillBeSent', (e) => {
  if (/\/api\//.test(e.request.url) && !/\/events(\?|$)/.test(e.request.url)) apiUrls.set(e.requestId, e.request.url);
  if (!/\/request$/.test(e.request.url) || !e.request.postData) return;
  try {
    const {op, args} = JSON.parse(e.request.postData);
    requestOps.push(op);
    opsById.set(e.requestId, {op, args: op === 'preview-open' ? {id: args?.id} : args});
  } catch {}
});
cdp.on('Network.loadingFinished', async (e) => {
  const url = apiUrls.get(e.requestId);
  if (!url || /rein-remote\.js$/.test(url)) return;
  try {
    const body = (await cdp.send('Network.getResponseBody', {requestId: e.requestId})).body;
    received.push({kind: 'reply', url: url.replace(/^https?:\/\/[^/]+/, ''), op: opsById.get(e.requestId)?.op, body});
    if (opsById.get(e.requestId)?.op === 'preview-add') addReplies.push(JSON.parse(body));
  } catch {}
});
cdp.on('Network.eventSourceMessageReceived', (e) => received.push({kind: 'event', body: e.data}));
const opens = (id) => [...opsById.values()].filter((o) => o.op === 'preview-open' && Number(o.args?.id) === id).length;
/** Keys the page must never get on a preview: where its stream listens, its token, a password. */
const secretKeys = (v, path = '$', out = []) => {
  if (v && typeof v === 'object') for (const [k, x] of Object.entries(v)) {
    if (/^(token|password|port)$/i.test(k)) out.push(`${path}.${k}`);
    secretKeys(x, `${path}.${k}`, out);
  }
  return out;
};
const sockets = new Map();
cdp.on('Network.responseReceived', (e) => reqs.push({url: e.response.url, status: e.response.status, protocol: e.response.protocol, remote: `${e.response.remoteIPAddress}:${e.response.remotePort}`, type: e.type}));
cdp.on('Network.webSocketCreated', (e) => sockets.set(e.requestId, {url: e.url, frames: 0, bytes: 0, sent: 0, status: null, firstAt: 0, lastAt: 0}));
cdp.on('Network.webSocketHandshakeResponseReceived', (e) => sockets.get(e.requestId) && (sockets.get(e.requestId).status = e.response.status));
cdp.on('Network.webSocketFrameReceived', (e) => {
  const s = sockets.get(e.requestId);
  if (!s) return;
  s.frames++;
  // Binary frames come base64-encoded.
  s.bytes += e.response.opcode === 2 ? Math.floor((e.response.payloadData.length * 3) / 4) : e.response.payloadData.length;
  s.lastAt = e.timestamp;
  s.firstAt ||= e.timestamp;
});
cdp.on('Network.webSocketFrameSent', (e) => sockets.get(e.requestId) && sockets.get(e.requestId).sent++);
cdp.on('Network.webSocketClosed', (e) => sockets.get(e.requestId) && (sockets.get(e.requestId).closed = true));

const shot = async (name) => {
  await page.screenshot({path: `${OUT}/${name}.png`});
  log('screenshot', `${OUT}/${name}.png`);
};

try {
  // 1. Sign in (password mode), as a person would: tap the fields, type, tap Sign in.
  await page.goto(URL0, {waitUntil: 'networkidle2'});
  await page.waitForSelector('input[autocomplete=username]', {timeout: 15000});
  await page.tap('input[autocomplete=username]');
  await page.keyboard.type(USER);
  await page.tap('input[autocomplete=current-password]');
  await page.keyboard.type(PASS);
  await page.tap('form.card button[type=submit]');
  await page.waitForFunction(() => !document.querySelector('input[autocomplete=current-password]'), {timeout: 15000});
  step('signed in through the proxy', true, {location: page.url()});

  // 2. A new chat (one-off: no project).
  const newBtn = await page.waitForSelector('.empty-actions .btn.primary', {timeout: 15000});
  await newBtn.tap();
  await page.waitForFunction(() => /^#\/chat\/[\da-f]+/.test(location.hash), {timeout: 30000});
  const chatId = await page.evaluate(() => location.hash.match(/^#\/chat\/([\da-f]+)/)[1]);
  await page.waitForSelector('.pchip.add', {timeout: 30000});
  step('chat open', true, {chatId});
  await shot('01-chat');

  // 3. "+ Preview" → ":1" (the page's prompt), as the docs say a user adds a VNC display. The FIRST open
  // must be Rein Remote: no frames path, the stream WebSocket used, and the bar's "· video" only while
  // video actually arrives (sampled together every 50 ms from the tap until it plays).
  const remoteJs = (id) => reqs.filter((r) => new RegExp(`/previews/${id}/rein-remote\\.js$`).test(r.url));
  const streams = (id) => [...sockets.values()].filter((s) => new RegExp(`/previews/${id}/stream$`).test(s.url));
  const watchOpen = async (id, target) => {
    const samples = [];
    const end = Date.now() + 30000;
    for (;;) {
      const bar = await page.$eval('#ppane .ptarget', (e) => e.textContent).catch(() => null);
      const ws = streams(id).find((s) => s.status === 101);
      samples.push({bar, wsFrames: ws?.frames ?? 0});
      if (bar?.includes(target) && /· video/.test(bar) && (ws?.frames ?? 0) > 2) break;
      if (Date.now() > end) break;
      await sleep(50);
    }
    const lies = samples.filter((x) => /· video/.test(x.bar ?? '') && !x.wsFrames);
    const ws = streams(id).find((s) => s.status === 101);
    return {
      bar: samples.at(-1)?.bar,
      samples: samples.length,
      samplesBeforeVideo: samples.filter((x) => !/· video/.test(x.bar ?? '')).length,
      barSaidVideoWithoutFrames: lies.length,
      reinRemoteJs: remoteJs(id).length,
      stream: ws && {url: ws.url, status: ws.status, framesReceived: ws.frames},
      framesPathOpens: opens(id),
    };
  };
  page.once('dialog', (d) => (log('dialog', d.message().slice(0, 80)), d.accept(':1')));
  await (await page.$('.pchip.add')).tap();
  await page.waitForSelector('#ppane', {timeout: 20000});
  result.firstOpen = await watchOpen(1, 'localhost:5901');
  const r = addReplies[0]?.value;
  result.firstOpen.previewAddReply = r && {keys: Object.keys(r), remote: r.remote, secretKeys: secretKeys(r)};
  log('preview-add reply', JSON.stringify(result.firstOpen.previewAddReply));
  step('preview-add reply has no port, token or password', !!r && r.remote === true && !secretKeys(r).length, result.firstOpen.previewAddReply ?? {});
  const fo = result.firstOpen;
  step('FIRST open after "+ Preview" is a Rein Remote stream (no frames path)', fo.reinRemoteJs > 0 && fo.stream?.status === 101 && fo.stream.framesReceived > 0 && fo.framesPathOpens === 0 && /localhost:5901 · video/.test(fo.bar ?? ''), fo);
  step('bar says "· video" only while video arrives', fo.barSaidVideoWithoutFrames === 0, {samples: fo.samples, samplesBeforeVideo: fo.samplesBeforeVideo, lies: fo.barSaidVideoWithoutFrames});
  await shot('02a-first-open');
  const ws = streams(1).find((s) => s.status === 101);

  // The client's own stats object (rein-remote.js connect().stats), found in the heap through CDP,
  // read as is: the client under test isn't changed or wrapped.
  const objs = await page.queryObjects(await page.evaluateHandle(() => Object.prototype));
  const statsH = await page.evaluateHandle((arr) => arr.filter((o) => o && typeof o.frames === 'number' && 'keyframes' in o && 'glassMs' in o && 'decoder' in o).sort((a, b) => b.frames - a.frames)[0], objs);
  const stats = () => statsH.evaluate((s) => s && {frames: s.frames, keyframes: s.keyframes, bytes: s.bytes, decoder: s.decoder, rttMs: s.rttMs, state: s.state, transport: s.transport, error: s.error});
  result.client.first = await stats();
  step('client stats found', !!result.client.first, result.client.first ?? {});
  const canvas = await page.$eval('#pscreen canvas', (c) => ({w: c.width, h: c.height, cssW: c.getBoundingClientRect().width, cssH: c.getBoundingClientRect().height}));
  result.client.canvas = canvas;
  await sleep(1500);
  await shot('02-xterm-streaming');

  // 4. Input: tap into the xterm, open the keyboard (the pane's keyboard button), type a command.
  const box = await (await page.$('#pscreen canvas')).boundingBox();
  await page.touchscreen.tap(box.x + box.width / 2, box.y + box.height / 2);
  await sleep(300);
  await (await page.$('#ppane .pkb')).tap();
  await sleep(300);
  const focused = await page.evaluate(() => document.activeElement?.getAttribute('aria-label'));
  step('keyboard button focuses the stream\'s text field', focused === 'Type into the remote screen', {focused});
  const opsBefore = requestOps.length, sentBefore = ws.sent;
  await page.keyboard.type('echo hi > /tmp/p2-proof', {delay: 15});
  await page.keyboard.press('Enter');
  await sleep(500);
  result.client.input = {wsMessagesSent: ws.sent - sentBefore, previewInputRequests: requestOps.slice(opsBefore).filter((o) => o === 'preview-input').length};
  step('typing went over the Rein Remote WebSocket (not the frames path\'s preview-input)', result.client.input.wsMessagesSent > 0 && result.client.input.previewInputRequests === 0, result.client.input);
  await sleep(1500);
  await shot('03-typed');

  // 5. Motion for the measurement: a clock printing ~30 times a second, typed the same way.
  const before = await stats();
  const ws0 = ws;
  const wsBefore = {frames: ws0?.frames ?? 0, bytes: ws0?.bytes ?? 0};
  const idleT = Date.now();
  await sleep(5000);
  const idle = await stats();
  result.client.idle = {seconds: (Date.now() - idleT) / 1000, frames: idle.frames - before.frames, bytes: idle.bytes - before.bytes};
  step('idle (still screen) for 5 s', true, result.client.idle);

  await page.keyboard.type('for i in $(seq 500); do date +%T.%N; sleep 0.03; done', {delay: 10});
  await page.keyboard.press('Enter');
  await sleep(1000);
  const m0 = await stats();
  const w0 = {...ws0};
  const mt = Date.now();
  await sleep(MEASURE_S * 1000);
  const m1 = await stats();
  const secs = (Date.now() - mt) / 1000;
  result.client.motion = {
    seconds: secs,
    framesDecoded: m1.frames - m0.frames,
    fps: +((m1.frames - m0.frames) / secs).toFixed(1),
    videoBytes: m1.bytes - m0.bytes,
    kbps: +(((m1.bytes - m0.bytes) * 8) / 1000 / secs).toFixed(0),
    keyframes: m1.keyframes - m0.keyframes,
    rttMs: m1.rttMs,
    decoder: m1.decoder,
    transport: m1.transport,
  };
  result.network.motionWsFrames = {received: ws0.frames - w0.frames, bytes: ws0.bytes - w0.bytes};
  step(`motion for ${MEASURE_S} s`, m1.frames > m0.frames, result.client.motion);
  await shot('04-motion');
  await sleep(2500); // the host samples rein-remote's /stats while this is still connected
  result.client.last = await stats();

  // 6. /preview :2 typed into the chat (the slash command), a second Xvnc display: it streams through
  // Rein Remote too, from its first open. On a phone the pane covers the chat (that's the layout, not a
  // workaround): hide it first (its ×), then use the message box.
  const hidePane = async () => {
    await (await page.$('#ppane .ptabs button.icon-btn[title="Hide the preview"]')).tap();
    await page.waitForFunction(() => !document.querySelector('#ppane'), {timeout: 10000});
  };
  await hidePane();
  const ta = await page.waitForSelector('textarea[placeholder^="Message Rein"]', {visible: true, timeout: 10000});
  await ta.tap();
  await page.keyboard.type('/preview :2', {delay: 10});
  await (await page.$('button.send')).tap();
  await page.waitForSelector('#ppane', {timeout: 20000});
  result.slashPreview = await watchOpen(2, 'localhost:5902');
  const sp = result.slashPreview;
  step('/preview :2 (slash command): first open is a Rein Remote stream', sp.reinRemoteJs > 0 && sp.stream?.status === 101 && sp.stream.framesReceived > 0 && sp.framesPathOpens === 0 && /localhost:5902 · video/.test(sp.bar ?? ''), sp);
  step('/preview :2: bar says "· video" only while video arrives', sp.barSaidVideoWithoutFrames === 0, {samples: sp.samples, lies: sp.barSaidVideoWithoutFrames});
  await sleep(1000);
  await shot('05-slash-preview-2');

  // 7. The fallback still works: display :3 has a VNC password and "+ Preview" gives Rein Remote none,
  // so rein-remote can't stream it; the pane uses frames, asks for the password, and shows the screen.
  await hidePane();
  page.once('dialog', (d) => d.accept(':3'));
  await (await page.$('.pchip.add')).tap();
  await page.waitForSelector('#ppane', {timeout: 20000});
  const pw = await page.waitForSelector('#pstatus input[type=password]', {timeout: 30000});
  await pw.tap();
  await page.keyboard.type(process.env.VNC_PW ?? 'p2-vnc-pw', {delay: 10});
  await (await page.$('#pstatus .pw-ask button')).tap();
  const drawn = await page.waitForFunction(() => {
    const cv = document.querySelector('#pscreen canvas');
    return cv && cv.width > 0 && !document.querySelector('#pstatus')?.textContent.trim();
  }, {timeout: 30000}).then(() => true, () => false);
  await sleep(1500);
  const bar3 = await page.$eval('#ppane .ptarget', (e) => e.textContent).catch(() => null);
  const r3 = addReplies[1]?.value;
  result.fallback = {bar: bar3, drawn, framesPathOpens: opens(r3?.id ?? 3), reinRemoteJs: remoteJs(r3?.id ?? 3).length, reply: r3 && {keys: Object.keys(r3), remote: r3.remote ?? false}};
  step('a display Rein Remote can\'t stream (VNC password) falls back to frames, asks for the password, shows it', drawn && result.fallback.framesPathOpens > 0 && !/video/.test(bar3 ?? ''), result.fallback);
  await shot('06-password-display-frames');

  // 8. The same display added again, now that Rein has its password: the reply (the old code's leak
  // returned the registry's object itself, password included) must carry none.
  await hidePane();
  page.once('dialog', (d) => d.accept(':3'));
  await (await page.$('.pchip.add')).tap();
  await page.waitForSelector('#ppane', {timeout: 20000});
  await sleep(4000);
  const again = addReplies[2]?.value;
  result.readd = {reply: again && {keys: Object.keys(again), remote: again.remote ?? false, secretKeys: secretKeys(again)}, bar: await page.$eval('#ppane .ptarget', (e) => e.textContent).catch(() => null)};
  step('re-adding the password display: reply has no password, port or token', !!again && !secretKeys(again).length && !JSON.stringify(again).includes(process.env.VNC_PW ?? 'p2-vnc-pw'), result.readd);
  await shot('07-password-display-again');

  // 9. Every preview the page got (replies, events, snapshots): none with a port, token or password.
  const bad = [];
  for (const x of received) {
    try {
      const v = JSON.parse(x.body);
      const found = secretKeys(v).filter((k) => /previews|value/.test(k));
      if (found.length) bad.push({kind: x.kind, url: x.url, op: x.op, found});
    } catch {}
  }
  result.leakScan = {bodies: received.length, events: received.filter((x) => x.kind === 'event').length, previewsWithSecrets: bad.slice(0, 10)};
  step('no preview the page received has a port, token or password', received.length > 0 && !bad.length, result.leakScan);
} catch (err) {
  step('error', false, {message: err.message});
  await shot('error').catch(() => {});
} finally {
  result.network.requests = reqs.filter((r) => /rein-remote\.js|\/api\/chats|app\.js|\/api\/login/.test(r.url));
  result.network.websockets = [...sockets.values()];
  writeFileSync(`${OUT}/page-received.jsonl`, received.map((x) => JSON.stringify(x)).join('\n') + '\n');
  save();
  await browser.close();
}
process.exit(result.steps.some((s) => !s.ok) ? 1 : 0);
