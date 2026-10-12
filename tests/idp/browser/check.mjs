// Phase 6 check, the browser's side: each user signs in at Keycloak in a browser of their own (a fresh
// profile: no shared sign-in), through OIDC (rr.test:7979) and SAML (rr.test:7980). alice (rr-admins)
// streams and types a command into the xterm; bob (rr-viewers) streams and types one too (the server
// must drop it); carol (no group) is refused with the reason. Writes /out/result.json and screenshots;
// run.sh then checks inside the machine which commands ran, the audit logs and the recordings.
import {mkdirSync, writeFileSync} from 'node:fs';
import puppeteer from 'puppeteer-core';

const OUT = '/out';
mkdirSync(OUT, {recursive: true});
const t0 = Date.now();
const log = (...a) => console.log(`[${((Date.now() - t0) / 1000).toFixed(1)}s]`, ...a);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const result = {steps: []};
const step = (name, ok, extra = {}) => (result.steps.push({name, ok, ...extra}), log(ok ? 'OK  ' : 'FAIL', name, JSON.stringify(extra)));
const save = () => writeFileSync(`${OUT}/result.json`, JSON.stringify(result, null, 2));
const PASSWORDS = {alice: 'alice-pw', bob: 'bob-pw', carol: 'carol-pw'};

const browser = await puppeteer.launch({
  executablePath: '/usr/bin/chromium',
  // Plain HTTP in this rig: the two origins treated as secure (WebCodecs, WebTransport), as HTTPS would be.
  args: ['--no-sandbox', '--headless=new', '--use-angle=swiftshader', '--enable-unsafe-swiftshader',
    '--unsafely-treat-insecure-origin-as-secure=http://rr.test:7979,http://rr.test:7980'],
});

/** One user, one protocol: opens the page, signs in at Keycloak, and what came of it. */
async function visit(proto, user, type) {
  const base = proto === 'oidc' ? 'http://rr.test:7979' : 'http://rr.test:7980';
  const ctx = await browser.createBrowserContext();
  const page = await ctx.newPage();
  await page.setViewport({width: 1100, height: 760});
  // Keycloak in a small VM: pages can take a while.
  page.setDefaultNavigationTimeout(120000);
  page.setDefaultTimeout(120000);
  page.on('pageerror', (e) => log(user, 'pageerror', e.message));
  const tag = `${proto}-${user}`;
  try {
    await page.goto(`${base}/`, {waitUntil: 'domcontentloaded'});
    // The page found no sign-in and went to Keycloak's.
    await page.waitForSelector('#username', {timeout: 120000});
    const atIdp = page.url();
    await page.type('#username', user);
    await page.type('#password', PASSWORDS[user]);
    await page.click('#kc-login');
    // Back at rein-remote (SAML: through Keycloak's self-posting form): the stream, or a refusal page.
    await page.waitForFunction(
      (host) => location.host === host && (window.reinRemote || document.querySelector('main > h2')),
      {timeout: 120000, polling: 200},
      new URL(base).host,
    );
    // rein-remote's own page about the sign-in (refused, failed), not the stream's.
    if (await page.$('main > h2')) {
      const why = await page.$eval('main > p#why', (e) => e.textContent);
      await page.screenshot({path: `${OUT}/${tag}.png`});
      return {user, proto, refused: why, idp: new URL(atIdp).host};
    }
    await page.waitForFunction(() => window.reinRemote.stats.frames > 10, {timeout: 60000, polling: 200});
    let typed = null;
    if (type) {
      // Into the xterm: a click there (focus), then a command that leaves a file behind if it runs.
      const box = await (await page.$('canvas')).boundingBox();
      await page.mouse.click(box.x + box.width / 4, box.y + box.height / 4);
      await sleep(300);
      typed = `touch /tmp/proof-${tag}`;
      await page.keyboard.type(typed);
      await page.keyboard.press('Enter');
      await sleep(1500);
    }
    const s = await page.evaluate(() => {
      const t = window.reinRemote.stats;
      return {frames: t.frames, fps: t.fps, transport: t.transport, decoder: t.decoder, access: t.access, error: t.error, bar: document.getElementById('stats').textContent};
    });
    await page.screenshot({path: `${OUT}/${tag}.png`});
    return {user, proto, stream: s, typed, url: page.url(), idp: new URL(atIdp).host};
  } catch (e) {
    await page.screenshot({path: `${OUT}/${tag}-error.png`}).catch(() => {});
    return {user, proto, error: String(e.message ?? e), url: page.url()};
  } finally {
    await ctx.close();
  }
}

for (const proto of ['oidc', 'saml']) {
  const a = await visit(proto, 'alice', true);
  step(`${proto}: alice signs in and streams with control`, !!a.stream && a.stream.access?.user === 'alice@corp.example' && !a.stream.access.viewOnly && a.stream.access.recorded, a);
  const b = await visit(proto, 'bob', true);
  step(`${proto}: bob signs in and streams, view only`, !!b.stream && b.stream.access?.user === 'bob@corp.example' && b.stream.access.viewOnly && !b.stream.access.recorded, b);
  const c = await visit(proto, 'carol', false);
  step(`${proto}: carol is refused`, !!c.refused && /no rule in the policy lets carol@corp.example in/.test(c.refused), c);
  save();
}

// No sign-in, no stream: the stream and a ticket are refused without the cookie.
const probe = await browser.newPage();
await probe.goto('http://rr.test:7979/info');
const noAuth = await probe.evaluate(async () => {
  const t = await fetch('/sso/ticket', {method: 'POST'});
  const ws = await new Promise((done) => {
    const w = new WebSocket('ws://rr.test:7979/stream');
    w.onopen = () => done('opened');
    w.onerror = () => done('refused');
  });
  return {ticket: t.status, websocket: ws};
});
step('without signing in: no ticket, no stream', noAuth.ticket === 401 && noAuth.websocket === 'refused', noAuth);
save();
await browser.close();
log(result.steps.every((s) => s.ok) ? 'ALL OK' : 'SOME FAILED');
