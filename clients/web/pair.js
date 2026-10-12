// Rein Remote pairing and sign-in for the web client: the same protocol as crates/rr-proto/src/pair.rs
// (SPAKE2 over P-256, as RFC 9382; ECDSA P-256 with SHA-256), in plain JavaScript, because a page on
// a plain-http:// local address gets no WebCrypto (browsers keep it for secure pages) and pairing is
// most needed exactly there. Checked against vectors the Rust side writes (clients/web/test).
//
//   import {pair, signIn} from './pair.js';
//   await pair(origin, '123-456');         // once: the code the server shows; keys kept here
//   const ticket = await signIn(origin);   // later: a ticket for /stream?ticket=…, nothing to type
//
// Timing: the big-number math here isn't constant-time. It runs once per pairing or sign-in, on the
// client's own device; the keys it protects are this browser's own.

// ---- SHA-256, HMAC, HKDF ----

const K256 = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
  0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
  0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
  0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

export function sha256(data) {
  const bytes = data instanceof Uint8Array ? data : new TextEncoder().encode(data);
  const bits = bytes.length * 8;
  const padded = new Uint8Array(((bytes.length + 9 + 63) >> 6) << 6);
  padded.set(bytes);
  padded[bytes.length] = 0x80;
  const dv = new DataView(padded.buffer);
  dv.setUint32(padded.length - 8, Math.floor(bits / 2 ** 32));
  dv.setUint32(padded.length - 4, bits >>> 0);
  const h = new Uint32Array([0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19]);
  const w = new Uint32Array(64);
  const rotr = (x, n) => (x >>> n) | (x << (32 - n));
  for (let off = 0; off < padded.length; off += 64) {
    for (let i = 0; i < 16; i++) w[i] = dv.getUint32(off + i * 4);
    for (let i = 16; i < 64; i++) {
      const s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >>> 3);
      const s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >>> 10);
      w[i] = (w[i - 16] + s0 + w[i - 7] + s1) >>> 0;
    }
    let [a, b, c, d, e, f, g, hh] = h;
    for (let i = 0; i < 64; i++) {
      const t1 = (hh + (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) + ((e & f) ^ (~e & g)) + K256[i] + w[i]) >>> 0;
      const t2 = ((rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) + ((a & b) ^ (a & c) ^ (b & c))) >>> 0;
      hh = g; g = f; f = e; e = (d + t1) >>> 0; d = c; c = b; b = a; a = (t1 + t2) >>> 0;
    }
    h[0] += a; h[1] += b; h[2] += c; h[3] += d; h[4] += e; h[5] += f; h[6] += g; h[7] += hh;
  }
  const out = new Uint8Array(32);
  const ov = new DataView(out.buffer);
  for (let i = 0; i < 8; i++) ov.setUint32(i * 4, h[i]);
  return out;
}

const concat = (...parts) => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) out.set(p, at), (at += p.length);
  return out;
};
const utf8 = (s) => new TextEncoder().encode(s);

export function hmac(key, msg) {
  let k = key.length > 64 ? sha256(key) : key;
  const block = new Uint8Array(64);
  block.set(k);
  const ipad = block.map((b) => b ^ 0x36), opad = block.map((b) => b ^ 0x5c);
  return sha256(concat(opad, sha256(concat(ipad, msg))));
}

/** HKDF-SHA256, no salt. */
export function hkdf(ikm, info, len) {
  const prk = hmac(new Uint8Array(32), ikm);
  let t = new Uint8Array(0), out = new Uint8Array(0);
  for (let i = 1; out.length < len; i++) {
    t = hmac(prk, concat(t, info, new Uint8Array([i])));
    out = concat(out, t);
  }
  return out.slice(0, len);
}

export const hex = (b) => [...b].map((x) => x.toString(16).padStart(2, '0')).join('');
export const unhex = (s) => Uint8Array.from(s.match(/../g) ?? [], (x) => parseInt(x, 16));
const equal = (a, b) => a.length === b.length && a.reduce((d, x, i) => d | (x ^ b[i]), 0) === 0;

// ---- P-256 ----

const P = 0xffffffff00000001000000000000000000000000ffffffffffffffffffffffffn;
const N = 0xffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551n;
const B = 0x5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604bn;
const G = [0x6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296n, 0x4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5n];
const mod = (a, m = P) => ((a % m) + m) % m;
function pow(b, e, m) {
  let r = 1n;
  b = mod(b, m);
  for (; e > 0n; e >>= 1n, b = (b * b) % m) if (e & 1n) r = (r * b) % m;
  return r;
}
const inv = (a, m = P) => pow(a, m - 2n, m);
const big = (bytes) => BigInt('0x' + (hex(bytes) || '0'));
const bytes32 = (n) => unhex(n.toString(16).padStart(64, '0'));

// Points in Jacobian coordinates [X, Y, Z]; Z = 0 is the identity.
const INF = [1n, 1n, 0n];
function dbl([x, y, z]) {
  if (z === 0n || y === 0n) return INF;
  const yy = mod(y * y), s = mod(4n * x * yy), zz = mod(z * z);
  const m = mod(3n * (x - zz) * (x + zz)); // a = -3
  const x3 = mod(m * m - 2n * s);
  return [x3, mod(m * (s - x3) - 8n * yy * yy), mod(2n * y * z)];
}
function add(p, q) {
  if (p[2] === 0n) return q;
  if (q[2] === 0n) return p;
  const z1z1 = mod(p[2] * p[2]), z2z2 = mod(q[2] * q[2]);
  const u1 = mod(p[0] * z2z2), u2 = mod(q[0] * z1z1);
  const s1 = mod(p[1] * q[2] * z2z2), s2 = mod(q[1] * p[2] * z1z1);
  if (u1 === u2) return s1 === s2 ? dbl(p) : INF;
  const h = mod(u2 - u1), r = mod(s2 - s1), hh = mod(h * h), hhh = mod(h * hh), v = mod(u1 * hh);
  const x3 = mod(r * r - hhh - 2n * v);
  return [x3, mod(r * (v - x3) - s1 * hhh), mod(h * p[2] * q[2])];
}
function mul(p, k) {
  let r = INF;
  for (let i = BigInt(k.toString(2).length - 1); i >= 0n; i--) {
    r = dbl(r);
    if ((k >> i) & 1n) r = add(r, p);
  }
  return r;
}
const neg = ([x, y, z]) => [x, mod(-y), z];
function affine([x, y, z]) {
  if (z === 0n) return null;
  const zi = inv(z), zi2 = mod(zi * zi);
  return [mod(x * zi2), mod(y * zi2 * zi)];
}
const onCurve = ([x, y]) => mod(y * y) === mod(x * x * x - 3n * x + B);
/** SEC1 bytes (uncompressed 04‖x‖y, or compressed 02/03‖x) as a point, or null. */
export function decodePoint(b) {
  let x, y;
  if (b.length === 65 && b[0] === 4) [x, y] = [big(b.subarray(1, 33)), big(b.subarray(33))];
  else if (b.length === 33 && (b[0] === 2 || b[0] === 3)) {
    x = big(b.subarray(1));
    y = pow(mod(x * x * x - 3n * x + B), (P + 1n) / 4n, P);
    if ((y & 1n) !== BigInt(b[0] & 1)) y = mod(-y);
  } else return null;
  if (x >= P || y >= P || !onCurve([x, y])) return null;
  return [x, y, 1n];
}
export function encodePoint(p) {
  const a = affine(p);
  return a && concat(new Uint8Array([4]), bytes32(a[0]), bytes32(a[1]));
}

const M = decodePoint(unhex('02886e2f97ace46e55ba9dd7242579f2993b64e16ef3dcab95afd497333d8fa12f'));
const NPT = decodePoint(unhex('03d8bbd6c639c62937b04d997f38c3770719c629d7014d49a24b4f98baa1292b49'));
const GEN = [G[0], G[1], 1n];

// ---- SPAKE2, the client's side (A) ----

export function codeScalar(code) {
  const digits = [...String(code)].filter((c) => /[0-9a-z]/i.test(c)).join('').toLowerCase();
  return mod(big(sha256(concat(utf8('rein-remote pair v1\0'), utf8(digits)))), N);
}
const le64 = (n) => {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, BigInt(n), true);
  return b;
};

/** Starts the client's side with 32 random bytes: {message, finish(serverPoint) → keys}. */
export function spakeClient(code, random) {
  const x = mod(big(random), N), w = codeScalar(code);
  const pA = encodePoint(add(mul(GEN, x), mul(M, w)));
  return {
    message: pA,
    finish(pBBytes) {
      const pB = decodePoint(pBBytes);
      if (!pB) throw new Error('the server sent a bad point');
      const k = mul(add(pB, neg(mul(NPT, w))), x);
      if (k[2] === 0n) throw new Error('the server sent a bad point');
      const parts = [utf8('rein-remote client'), utf8('rein-remote server'), pA, encodePoint(pB), encodePoint(k), bytes32(w)];
      const tt = concat(...parts.flatMap((p) => [le64(p.length), p]));
      const h = sha256(tt);
      const kc = hkdf(h.subarray(16), utf8('ConfirmationKeys'), 32);
      return {shared: h.slice(0, 16), confirmMine: hmac(kc.subarray(0, 16), tt), confirmTheirs: hmac(kc.subarray(16), tt)};
    },
  };
}

export const keyMac = (shared, label, pub) => hmac(shared, concat(utf8(`rein-remote pair key ${label}\0`), pub));
export const serverAuthMessage = (clientNonce, serverNonce) => concat(utf8('rein-remote server auth v1\0'), clientNonce, serverNonce);
export const clientAuthMessage = (serverNonce, serverPub) => concat(utf8('rein-remote client auth v1\0'), serverNonce, serverPub);

// ---- ECDSA P-256, SHA-256; signatures as r‖s ----

const random32 = () => crypto.getRandomValues(new Uint8Array(32));

/** The public key (SEC1 uncompressed) of a private key (32 bytes). */
export const publicKey = (priv) => encodePoint(mul(GEN, big(priv)));

export function newKey() {
  for (;;) {
    const d = mod(big(random32()), N);
    if (d > 0n) return {priv: bytes32(d), pub: encodePoint(mul(GEN, d))};
  }
}

export function sign(privBytes, msg, entropy = random32()) {
  const d = big(privBytes), z = mod(big(sha256(msg)), N);
  for (let i = 0; ; i++) {
    // k from the key, the message and fresh randomness: a weak random source alone can't reveal the key.
    const k = mod(big(sha256(concat(privBytes, sha256(msg), entropy, new Uint8Array([i])))), N);
    if (k === 0n) continue;
    const r = mod(affine(mul(GEN, k))[0], N);
    if (r === 0n) continue;
    let s = mod(inv(k, N) * (z + r * d), N);
    if (s === 0n) continue;
    if (s > N / 2n) s = N - s;
    return concat(bytes32(r), bytes32(s));
  }
}

export function verify(pubBytes, msg, sig) {
  const q = decodePoint(pubBytes);
  if (!q || sig.length !== 64) return false;
  const r = big(sig.subarray(0, 32)), s = big(sig.subarray(32));
  if (r <= 0n || r >= N || s <= 0n || s >= N) return false;
  const z = mod(big(sha256(msg)), N), si = inv(s, N);
  const p = affine(add(mul(GEN, mod(z * si, N)), mul(q, mod(r * si, N))));
  return !!p && mod(p[0], N) === r;
}

// ---- the flows, against a server at `origin` ----

const KEY = (id) => `rein-remote:paired:${id}`;
const store = typeof localStorage !== 'undefined' ? localStorage : new Map();
const get = (k) => (store.getItem ? store.getItem(k) : store.get(k));
const put = (k, v) => (store.setItem ? store.setItem(k, v) : store.set(k, v));

async function post(url, fields) {
  const r = await fetch(url, {method: 'POST', headers: {'content-type': 'application/x-www-form-urlencoded'}, body: new URLSearchParams(fields)});
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j.error || `${url}: HTTP ${r.status}`);
  return j;
}

/** What this browser keeps for the server whose fingerprint is `id`, if it's paired with it. */
export function paired(id) {
  try {
    return JSON.parse(get(KEY(id)) ?? 'null');
  } catch {
    return null;
  }
}

/** Pairs with the server at `origin` using the code it shows; keeps both keys. Resolves with its fingerprint. */
export async function pair(origin, code, name = (typeof navigator !== 'undefined' && navigator.platform) || 'browser') {
  const me = newKey();
  const s = spakeClient(code, random32());
  const start = await post(`${origin}/pair/start`, {x: hex(s.message), name});
  const keys = s.finish(unhex(start.y));
  if (!equal(keys.confirmTheirs, unhex(start.confirm))) throw new Error("the code didn't match");
  const serverPub = unhex(start.server_key);
  if (!equal(keyMac(keys.shared, 'server', serverPub), unhex(start.server_key_mac))) throw new Error("the server's key didn't check out");
  await post(`${origin}/pair/finish`, {session: start.session, confirm: hex(keys.confirmMine), key: hex(me.pub), mac: hex(keyMac(keys.shared, 'client', me.pub)), name});
  put(KEY(start.id), JSON.stringify({priv: hex(me.priv), pub: hex(me.pub), server: hex(serverPub)}));
  return start.id;
}

/**
 * Proves who this browser is to the server whose fingerprint is `id` (and checks the server is the
 * one it paired with): a ticket for /stream?ticket=…, good for a minute.
 */
export async function signIn(origin, id) {
  const p = paired(id);
  if (!p) throw new Error('not paired with this server');
  const cn = random32();
  const ch = await (await fetch(`${origin}/auth/challenge?cn=${hex(cn)}`)).json();
  const server = unhex(p.server), nonce = unhex(ch.nonce);
  if (ch.server_key !== p.server || !verify(server, serverAuthMessage(cn, nonce), unhex(ch.sig))) {
    throw new Error("this isn't the machine you paired with (its key is different)");
  }
  const {ticket} = await post(`${origin}/auth`, {nonce: ch.nonce, key: p.pub, sig: hex(sign(unhex(p.priv), clientAuthMessage(nonce, server)))});
  return ticket;
}
