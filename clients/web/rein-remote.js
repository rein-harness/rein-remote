// Rein Remote web client: connect to a rein-remote server, show its screen, send input back.
// No dependencies; decodes with WebCodecs (hardware where the device has it). Speaks the protocol in
// crates/rr-proto, with the same compatibility rules: every frame is kind, version, flags, len, body;
// unknown kinds and values are skipped; fields added in later versions are read only when present.
//
//   import {connect} from './rein-remote.js';
//   const session = connect('wss://host/stream', canvas);
//
// or, embedded: <rein-remote src="wss://host/stream"></rein-remote>

const K = {HELLO: 1, CONFIG: 2, VIDEO: 3, AUDIO: 4, CURSOR: 5, INPUT: 6, FEEDBACK: 7, KEYFRAME_REQUEST: 8, CLIPBOARD: 9, RESIZE: 10, PING: 11, PONG: 12};
const HEADER = 7;
const FLAG_KEYFRAME = 1;
const CODECS = {1: 'h264', 2: 'h265', 3: 'av1', 4: 'vp9', 5: 'vp8'};
// HEVC Main, level 5.1 (up to 4K at 60): Annex B, as the server sends it.
const HEVC = 'hvc1.1.6.L153.B0';

// ---- frames ----

class W {
  constructor() { this.b = []; }
  u8(v) { this.b.push(v & 0xff); }
  u16(v) { this.b.push(v & 0xff, (v >> 8) & 0xff); }
  i16(v) { this.u16(v < 0 ? v + 0x10000 : v); }
  u32(v) { this.u16(v & 0xffff); this.u16((v >>> 16) & 0xffff); }
  u64(v) { const lo = Number(BigInt(v) & 0xffffffffn), hi = Number(BigInt(v) >> 32n); this.u32(lo); this.u32(hi); }
  str(s) { const e = new TextEncoder().encode(s).slice(0, 65535); this.u16(e.length); for (const x of e) this.b.push(x); }
  frame(kind, version = 0, flags = 0) {
    const out = new Uint8Array(HEADER + this.b.length);
    out[0] = kind; out[1] = version; out[2] = flags;
    new DataView(out.buffer).setUint32(3, this.b.length, true);
    out.set(this.b, HEADER);
    return out;
  }
}

class R {
  constructor(u8) { this.v = new DataView(u8.buffer, u8.byteOffset, u8.byteLength); this.u = u8; this.at = 0; }
  need(n) { if (this.at + n > this.u.length) throw new Error('short body'); }
  u8() { this.need(1); return this.u[this.at++]; }
  u16() { this.need(2); const x = this.v.getUint16(this.at, true); this.at += 2; return x; }
  i16() { this.need(2); const x = this.v.getInt16(this.at, true); this.at += 2; return x; }
  u32() { this.need(4); const x = this.v.getUint32(this.at, true); this.at += 4; return x; }
  u64() { this.need(8); const x = this.v.getBigUint64(this.at, true); this.at += 8; return x; }
  str() { const n = this.u16(); this.need(n); const s = new TextDecoder().decode(this.u.subarray(this.at, this.at + n)); this.at += n; return s; }
  bytes() { const n = this.u32(); this.need(n); const b = this.u.subarray(this.at, this.at + n); this.at += n; return b; }
}

/** Every complete frame in `buf` (a WebSocket message may hold several). */
function* frames(buf) {
  let at = 0;
  while (at + HEADER <= buf.length) {
    const kind = buf[at], version = buf[at + 1], flags = buf[at + 2];
    const len = new DataView(buf.buffer, buf.byteOffset + at + 3, 4).getUint32(0, true);
    if (at + HEADER + len > buf.length) return;
    yield {kind, version, flags, body: buf.subarray(at + HEADER, at + HEADER + len)};
    at += HEADER + len;
  }
}

/**
 * This display's refresh rate, measured (not assumed): the median gap between animation frames over
 * ~30 of them, in millihertz (0 when the page can't animate now, e.g. a background tab).
 */
export function measureRefresh(frames = 30, timeoutMs = 1000) {
  if (typeof requestAnimationFrame === 'undefined') return Promise.resolve(0);
  return new Promise((resolve) => {
    const gaps = [];
    let last = 0;
    const timer = setTimeout(() => done(), timeoutMs);
    const done = () => {
      clearTimeout(timer);
      if (gaps.length < 5) return resolve(0);
      gaps.sort((a, b) => a - b);
      resolve(Math.round(1e6 / gaps[gaps.length >> 1]));
    };
    const tick = (t) => {
      if (last) gaps.push(t - last);
      last = t;
      if (gaps.length >= frames) return done();
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
}

const nowUs = () => BigInt(Math.round((performance.timeOrigin + performance.now()) * 1000));

/** The WebCodecs codec string for an H.264 stream, from its SPS (profile, constraints, level). */
function avcCodec(data) {
  for (let i = 0; i + 7 < data.length; i++) {
    if (data[i] === 0 && data[i + 1] === 0 && data[i + 2] === 1 && (data[i + 3] & 0x1f) === 7) {
      const hex = (x) => x.toString(16).padStart(2, '0');
      return `avc1.${hex(data[i + 4])}${hex(data[i + 5])}${hex(data[i + 6])}`;
    }
  }
  return 'avc1.42e01f';
}

// ---- video as datagrams: forward error correction (the same scheme as crates/rr-proto/src/fec.rs) ----

const FEC_MAGIC = 0xd7, FEC_HEADER = 16;
const GF_EXP = new Uint8Array(512), GF_LOG = new Uint8Array(256);
for (let i = 0, x = 1; i < 255; i++) {
  GF_EXP[i] = x;
  GF_LOG[x] = i;
  x <<= 1;
  if (x & 0x100) x ^= 0x11d;
}
for (let i = 255; i < 512; i++) GF_EXP[i] = GF_EXP[i - 255];
const gfMul = (a, b) => (a && b ? GF_EXP[GF_LOG[a] + GF_LOG[b]] : 0);
const gfInv = (a) => GF_EXP[255 - GF_LOG[a]];
const cauchy = (j, i, k) => gfInv(((k + j) & 255) ^ i);
function mulAdd(dst, c, src) {
  if (!c) return;
  const lc = GF_LOG[c];
  for (let i = 0; i < dst.length; i++) if (src[i]) dst[i] ^= GF_EXP[lc + GF_LOG[src[i]]];
}

/** The k data shards from any k of the k + m (missing: undefined), or null when too few came. */
export function fecReconstruct(shards, k) {
  const have = [];
  for (let i = 0; i < shards.length && have.length < k; i++) if (shards[i]) have.push(i);
  if (have.length < k) return null;
  let missing = false;
  for (let i = 0; i < k; i++) if (!shards[i]) missing = true;
  if (!missing) return shards.slice(0, k);
  // Invert the k×k matrix of the shards that came (identity rows for data, Cauchy rows for parity).
  const a = have.map((r) => Uint8Array.from({length: k}, (_, c) => (r < k ? +(r === c) : cauchy(r - k, c, k))));
  const inv = Array.from({length: k}, (_, r) => Uint8Array.from({length: k}, (_, c) => +(r === c)));
  for (let col = 0; col < k; col++) {
    let p = col;
    while (p < k && !a[p][col]) p++;
    if (p === k) return null;
    [a[col], a[p]] = [a[p], a[col]];
    [inv[col], inv[p]] = [inv[p], inv[col]];
    const f = gfInv(a[col][col]);
    for (let c = 0; c < k; c++) (a[col][c] = gfMul(a[col][c], f)), (inv[col][c] = gfMul(inv[col][c], f));
    for (let r = 0; r < k; r++) {
      const g = a[r][col];
      if (r !== col && g) mulAdd(a[r], g, a[col]), mulAdd(inv[r], g, inv[col]);
    }
  }
  const len = shards[have[0]].length;
  const out = [];
  for (let i = 0; i < k; i++) {
    if (shards[i]) out.push(shards[i]);
    else {
      const d = new Uint8Array(len);
      have.forEach((r, j) => mulAdd(d, inv[i][j], shards[r]));
      out.push(d);
    }
  }
  return out;
}

/**
 * Frames put back together from datagrams in any order, lost shards rebuilt from parity. A frame not
 * whole once a newer one is, or `window` frames behind the newest, is given up (lost). Counts the
 * gaps in the datagrams' sequence numbers, for the loss the server sets its parity by.
 */
export class FecReassembler {
  constructor(window = 8) {
    this.window = window;
    this.partial = new Map(); // frame → {blocks, got: Map(block → {shards, k, len, data})}
    this.doneUpTo = undefined;
    this.lost = 0;
    this.recovered = 0;
    this.received = 0;
    this.span = null; // [first, highest] datagram sequence since the last lossPermille()
  }
  push(d) {
    if (d.length < FEC_HEADER || d[0] !== FEC_MAGIC || d[1] !== 0) return null;
    const v = new DataView(d.buffer, d.byteOffset, d.byteLength);
    const frame = v.getUint32(2, true), block = d[6], blocks = d[7], index = d[8], k = d[9], m = d[10];
    const len = d[11] | (d[12] << 8) | (d[13] << 16);
    const seq = v.getUint16(14, true);
    if (!k || k + m > 255 || index >= k + m || block >= blocks) return null;
    this.received++;
    // Datagrams can come out of order: the span grows either way.
    if (!this.span) this.span = [seq, seq];
    else if ((((seq - this.span[1]) << 16) >> 16) > 0) this.span[1] = seq;
    else if ((((seq - this.span[0]) << 16) >> 16) < 0) this.span[0] = seq;
    if (this.doneUpTo !== undefined && ((frame - this.doneUpTo) | 0) <= 0) return null; // late
    let p = this.partial.get(frame);
    if (!p) this.partial.set(frame, (p = {blocks, got: new Map(), shards: 0}));
    let b = p.got.get(block);
    if (!b) p.got.set(block, (b = {shards: new Array(k + m), k, n: k + m, len, data: null, count: 0}));
    if (!b.data && !b.shards[index]) {
      b.shards[index] = d.subarray(FEC_HEADER);
      b.count++;
      if (b.count >= k) {
        const missing = b.shards.slice(0, k).some((x) => !x);
        const data = fecReconstruct(b.shards, k);
        if (data) {
          if (missing) this.recovered++;
          const whole = new Uint8Array(data.reduce((n, x) => n + x.length, 0));
          let at = 0;
          for (const x of data) whole.set(x, at), (at += x.length);
          b.data = whole.subarray(0, len);
          b.shards = null;
        }
      }
    }
    // Give up on frames too far behind the newest.
    let newest = frame;
    for (const f of this.partial.keys()) if (((f - newest) | 0) > 0) newest = f;
    for (const [f, q] of this.partial) if (f !== frame && ((newest - f) | 0) > this.window) this.drop(f, q);
    const whole = p.got.size === p.blocks && [...p.got.values()].every((x) => x.data);
    if (!whole) return null;
    this.partial.delete(frame);
    for (const [f, q] of this.partial) if (((frame - f) | 0) > 0) this.drop(f, q);
    this.doneUpTo = frame;
    const parts = [...p.got.entries()].sort((x, y) => x[0] - y[0]).map(([, x]) => x.data);
    const out = new Uint8Array(parts.reduce((n, x) => n + x.length, 0));
    let at = 0;
    for (const x of parts) out.set(x, at), (at += x.length);
    return out;
  }
  drop(f) {
    this.partial.delete(f);
    this.lost++;
  }
  /** Of the datagrams sent since the last call (by their sequence numbers), the share that never came, in thousandths. */
  lossPermille() {
    if (!this.span) return 0;
    const expected = ((this.span[1] - this.span[0]) & 0xffff) + 1;
    const got = Math.min(this.received, expected);
    this.span = null;
    this.received = 0;
    return Math.floor(((expected - got) * 1000) / expected);
  }
}

// ---- video on plain HTTP: Media Source ----

/*
 * A browser gives its video decoder (WebCodecs) only to secure pages (HTTPS, localhost). On a plain
 * http:// page (a local-network address), the same H.264 goes through Media Source instead: each frame
 * wrapped as a fragment of MP4 for a <video> kept at the live edge, drawn onto the canvas as it plays.
 * A little more delay than WebCodecs; the same everywhere else (input, the pointer, touch).
 */
const MS = typeof window !== 'undefined' ? window.ManagedMediaSource ?? window.MediaSource : undefined;

const u32be = (v) => [(v >>> 24) & 255, (v >>> 16) & 255, (v >>> 8) & 255, v & 255];
const u16be = (v) => [(v >>> 8) & 255, v & 255];
function box(type, ...parts) {
  const body = parts.flatMap((p) => (p instanceof Uint8Array ? [...p] : p));
  return [...u32be(8 + body.length), ...[...type].map((c) => c.charCodeAt(0)), ...body];
}
const full = (type, version, flags, ...parts) => box(type, [version, (flags >> 16) & 255, (flags >> 8) & 255, flags & 255], ...parts);

/** Annex B (start codes) into NAL units. */
function nalUnits(data) {
  const out = [];
  let i = 0, start = -1;
  while (i + 3 <= data.length) {
    const sc = data[i] === 0 && data[i + 1] === 0 && (data[i + 2] === 1 || (data[i + 2] === 0 && data[i + 3] === 1));
    if (sc) {
      if (start >= 0) out.push(data.subarray(start, i));
      i += data[i + 2] === 1 ? 3 : 4;
      start = i;
    } else i++;
  }
  if (start >= 0) out.push(data.subarray(start));
  return out;
}

function initSegment(sps, pps, width, height) {
  const avcC = box('avcC', [1, sps[1], sps[2], sps[3], 0xff, 0xe1, ...u16be(sps.length), ...sps, 1, ...u16be(pps.length), ...pps]);
  const avc1 = box('avc1', [0, 0, 0, 0, 0, 0, 0, 1, ...new Array(16).fill(0), ...u16be(width), ...u16be(height), 0, 0x48, 0, 0, 0, 0x48, 0, 0, 0, 0, 0, 0, 0, 1, ...new Array(32).fill(0), 0, 0x18, 0xff, 0xff], avcC);
  const zero = (n) => new Array(n).fill(0);
  const matrix = [0, 1, 0, 0, ...zero(12), 0, 1, 0, 0, ...zero(12), 0x40, 0, 0, 0];
  return new Uint8Array([
    ...box('ftyp', [...'isom'].map((c) => c.charCodeAt(0)), u32be(1), [...'isomiso5avc1'].map((c) => c.charCodeAt(0))),
    ...box('moov',
      full('mvhd', 0, 0, zero(8), u32be(90000), zero(4), u32be(0x00010000), u16be(0x0100), zero(10), matrix, zero(24), u32be(2)),
      box('trak',
        full('tkhd', 0, 3, zero(8), u32be(1), zero(4), zero(4), zero(8), zero(4), zero(4), matrix, u32be(width << 16), u32be(height << 16)),
        box('mdia',
          full('mdhd', 0, 0, zero(8), u32be(90000), zero(4), u16be(0x55c4), zero(2)),
          full('hdlr', 0, 0, zero(4), [...'vide'].map((c) => c.charCodeAt(0)), zero(12), [...'video', '\0'].map((c) => c.charCodeAt(0))),
          box('minf',
            full('vmhd', 0, 1, zero(8)),
            box('dinf', full('dref', 0, 0, u32be(1), full('url ', 0, 1))),
            box('stbl', full('stsd', 0, 0, u32be(1), avc1), full('stts', 0, 0, zero(4)), full('stsc', 0, 0, zero(4)), full('stsz', 0, 0, zero(8)), full('stco', 0, 0, zero(4)))))),
      box('mvex', full('trex', 0, 0, u32be(1), u32be(1), zero(4), zero(4), zero(4)))),
  ]);
}

function fragment(seq, time90k, duration, key, sample) {
  const trunLen = 12 + 4 + 4 + 4 + 4 + 4; // full header, count, data offset, duration, size, flags
  const moofLen = 8 + 16 + (8 + 16 + 20 + trunLen);
  const traf = box('traf',
    full('tfhd', 0, 0x020000, u32be(1)),
    full('tfdt', 1, 0, u32be(Math.floor(time90k / 2 ** 32)), u32be(time90k >>> 0)),
    full('trun', 0, 0x000701, u32be(1), u32be(moofLen + 8), u32be(duration), u32be(sample.length), u32be(key ? 0x02000000 : 0x01010000)));
  const moof = box('moof', full('mfhd', 0, 0, u32be(seq)), traf);
  const out = new Uint8Array(moof.length + 8 + sample.length);
  out.set(moof);
  out.set(u32be(8 + sample.length), moof.length);
  out.set([109, 100, 97, 116], moof.length + 4); // mdat
  out.set(sample, moof.length + 8);
  return out;
}

/** The VideoDecoder interface this client uses, over Media Source (for plain http:// pages). */
class MseDecoder {
  constructor({output, error}) {
    Object.assign(this, {output, error, state: 'unconfigured', queue: [], seq: 0, times: new Map()});
  }
  configure({codec, width, height}) {
    this.size = [width, height];
    this.video = Object.assign(document.createElement('video'), {muted: true, playsInline: true, autoplay: true, disableRemotePlayback: true});
    this.ms = new MS();
    this.video.src = URL.createObjectURL(this.ms);
    this.ms.addEventListener('sourceopen', () => {
      this.sb = this.ms.addSourceBuffer(`video/mp4; codecs="${codec}"`);
      this.sb.addEventListener('updateend', () => this.pump());
      this.pump();
    });
    this.t0 = performance.now();
    this.state = 'configured';
    const draw = (_, meta) => {
      if (this.state === 'closed') return;
      // The frame now on screen: the nearest one appended by its time.
      let n = 0, best = Infinity;
      for (const [t, k] of this.times) if (Math.abs(t - meta.mediaTime) < best) ((best = Math.abs(t - meta.mediaTime)), (n = k));
      for (const t of this.times.keys()) if (t < meta.mediaTime - 1) this.times.delete(t);
      this.output({image: this.video, displayWidth: this.video.videoWidth, displayHeight: this.video.videoHeight, timestamp: n, close() {}});
      this.video.requestVideoFrameCallback(draw);
    };
    this.video.requestVideoFrameCallback(draw);
    this.video.play().catch(() => {});
  }
  decode({type, timestamp, data}) {
    const nals = nalUnits(data);
    const sps = nals.find((u) => (u[0] & 31) === 7), pps = nals.find((u) => (u[0] & 31) === 8);
    if (sps && pps && !this.inited) {
      this.queue.push(initSegment(sps, pps, ...this.size));
      this.inited = true;
    }
    if (!this.inited) return;
    // The frame's slices in MP4's form: each NAL unit after its length.
    const units = nals.filter((u) => ![7, 8, 9].includes(u[0] & 31));
    const sample = new Uint8Array(units.reduce((a, u) => a + 4 + u.length, 0));
    let at = 0;
    for (const u of units) (sample.set(u32be(u.length), at), sample.set(u, at + 4), (at += 4 + u.length));
    // A timeline with no holes (a hole stalls the video): each frame starts where the last ended and
    // lasts as long as the wait before it, so it plays at the pace frames come.
    const now = performance.now();
    const d = this.last === undefined ? 1500 : Math.max(90, Math.round((now - this.last) * 90));
    this.last = now;
    const t = this.next ?? 0;
    this.next = t + d;
    this.times.set(t / 90000, timestamp);
    this.queue.push(fragment(++this.seq, t, d, type === 'key', sample));
    this.pump();
  }
  pump() {
    if (!this.sb || this.sb.updating || !this.queue.length) return;
    try {
      this.sb.appendBuffer(this.queue.shift());
    } catch (e) {
      return this.error(e);
    }
    // Live: stay at the newest frame (a little behind it, for the decoder).
    const b = this.video.buffered;
    if (b.length && b.end(b.length - 1) - this.video.currentTime > 0.12) this.video.currentTime = b.end(b.length - 1) - 0.02;
  }
  close() {
    this.state = 'closed';
    try {
      this.ms?.endOfStream();
    } catch {}
    this.video?.removeAttribute('src');
  }
}

const webcodecs = typeof VideoDecoder !== 'undefined';

// ---- sound: Opus through WebCodecs into Web Audio, played in time with the picture ----

/*
 * Every Audio packet carries the capture time of its first sample on the server's clock, the clock
 * video frames are stamped with. The clocks aren't synced, so sound is placed by a measured offset:
 * for each frame shown, (local time − capture time) is how late after capture the picture is seen.
 * The offset both share is the median of the last ~2 s of those, or later if sound needs more time to
 * arrive and decode (its own p90, plus a little jitter buffer). Each packet plays at capture time +
 * offset, mapped onto the audio clock at the moment it's heard (getOutputTimestamp, so the output's
 * own latency counts). Packets in a row play back to back; a small error is taken up by playing a
 * touch fast or slow (≤1%), one over 20 ms by dropping packets (sound behind) or a gap (ahead).
 */

const median = (v) => {
  if (!v.length) return undefined;
  const s = [...v].sort((a, b) => a - b);
  return s[s.length >> 1];
};
const pct = (v, p) => {
  if (!v.length) return undefined;
  const s = [...v].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor(s.length * p))];
};
const localUs = () => (performance.timeOrigin + performance.now()) * 1000;

/** Whether this browser plays the server's sound (Opus, 48 kHz, stereo, through WebCodecs). */
export async function canPlaySound() {
  if (typeof AudioDecoder === 'undefined' || typeof AudioContext === 'undefined') return false;
  try {
    return !!(await AudioDecoder.isConfigSupported({codec: 'opus', sampleRate: 48000, numberOfChannels: 2})).supported;
  } catch {
    return false;
  }
}

/** The offset sound and picture share, from what each needs (µs, local clock − capture clock). */
export function sharedOffset(videoUs, audioReadyUs, jitterUs = 5000) {
  const v = median(videoUs);
  const a = audioReadyUs.length ? pct(audioReadyUs, 0.9) + jitterUs : undefined;
  if (v === undefined) return a;
  return a === undefined ? v : Math.max(v, a);
}

/**
 * Where a packet goes, given where the last one ended (`playhead`, audio-clock seconds, 0: none
 * yet), where it should be heard (`want`), how long it is, and the earliest it can still start.
 * Returns {at, rate, playhead} to play it, or {drop: true} to skip it.
 */
export function placePacket(playhead, want, duration, earliest) {
  const err = playhead ? playhead - want : Infinity; // > 0: the sound runs behind the picture
  if (Math.abs(err) > 0.02) {
    // Off by more than 20 ms: start again where it should be; too late for that, drop it (and the
    // ones after it, until the sound has caught up).
    if (want < earliest) return {drop: true, playhead: playhead && err > 0.02 ? playhead : 0};
    return {at: want, rate: 1, playhead: want + duration};
  }
  // Back to back, a little fast while behind, a little slow while ahead: 1% at most, inaudible.
  const at = Math.max(playhead, earliest);
  const rate = 1 + Math.max(-0.01, Math.min(0.01, (at - want) / 0.5));
  return {at, rate, playhead: at + duration / rate};
}

function makeSound(stats, measure, parent) {
  const ctx = new AudioContext({latencyHint: 'interactive', sampleRate: 48000});
  const out = ctx.createGain();
  out.connect(ctx.destination);
  const a = (stats.audio = {state: ctx.state, decoder: 'opus (WebCodecs)', packets: 0, played: 0, dropped: 0, gaps: 0, offsetMs: null, videoMs: null, audioMs: null, outputLatencyMs: 0});
  const videoUs = [], readyUs = [], arriveUs = [];
  let playhead = 0, offsetUs, base;
  const update = () => {
    const o = sharedOffset(videoUs, readyUs);
    if (o === undefined) return;
    // Moved only for a change worth it, so the sound doesn't wander with each frame.
    if (offsetUs === undefined || Math.abs(o - offsetUs) > 2000) offsetUs = o;
    a.offsetMs = +(offsetUs / 1000).toFixed(1);
  };
  // Audio-clock time at which a local time (µs) is heard, from the output's own timestamp.
  const toCtx = (us) => {
    const ts = ctx.getOutputTimestamp?.();
    const perf = us / 1000 - performance.timeOrigin;
    if (ts?.performanceTime) return ts.contextTime + (perf - ts.performanceTime) / 1000;
    return ctx.currentTime + (ctx.outputLatency || ctx.baseLatency || 0) + (perf - performance.now()) / 1000;
  };
  const toLocalUs = (ctxTime) => {
    const ts = ctx.getOutputTimestamp?.();
    if (ts?.performanceTime) return (performance.timeOrigin + ts.performanceTime + (ctxTime - ts.contextTime) * 1000) * 1000;
    return localUs() + (ctxTime - ctx.currentTime - (ctx.outputLatency || 0)) * 1e6;
  };
  const decoder = new AudioDecoder({
    output: (data) => {
      const n = data.numberOfFrames;
      const tsUs = data.timestamp + base;
      const buf = ctx.createBuffer(2, n, 48000);
      for (let c = 0; c < 2; c++) {
        const plane = new Float32Array(n);
        data.copyTo(plane, {planeIndex: Math.min(c, data.numberOfChannels - 1), format: 'f32-planar'});
        buf.copyToChannel(plane, c);
      }
      data.close();
      a.outputLatencyMs = +((ctx.outputLatency || ctx.baseLatency || 0) * 1000).toFixed(1);
      if (ctx.state !== 'running') return;
      // The earliest this could be heard: what sound needs, for the shared offset.
      const earliest = ctx.currentTime + 0.004;
      readyUs.push(toLocalUs(earliest) - tsUs);
      if (readyUs.length > 100) readyUs.shift();
      update();
      a.audioMs = +(pct(readyUs, 0.9) / 1000).toFixed(1);
      if (offsetUs === undefined) return;
      const placed = placePacket(playhead, toCtx(tsUs + offsetUs), n / 48000, earliest);
      if (placed.drop) {
        a.dropped++;
        playhead = placed.playhead;
        return;
      }
      if (playhead && Math.abs(placed.at - playhead) > 0.001) a.gaps++;
      const src = ctx.createBufferSource();
      src.buffer = buf;
      src.playbackRate.value = placed.rate;
      src.connect(out);
      src.start(placed.at);
      playhead = placed.playhead;
      a.played++;
    },
    error: (e) => (a.decoder = `error: ${e.message ?? e}`),
  });
  decoder.configure({codec: 'opus', sampleRate: 48000, numberOfChannels: 2});

  // Browsers start sound only after the person does something on the page: until then, a hint.
  let hint;
  const resume = () => ctx.resume().then(() => ((a.state = ctx.state), hint?.remove(), (hint = null)), () => {});
  const gestures = ['pointerdown', 'keydown', 'touchend'];
  const onGesture = () => {
    resume();
    if (ctx.state === 'running') gestures.forEach((g) => document.removeEventListener(g, onGesture, true));
  };
  gestures.forEach((g) => document.addEventListener(g, onGesture, true));
  ctx.onstatechange = () => ((a.state = ctx.state), ctx.state === 'running' && hint && (hint.remove(), (hint = null)));
  const showHint = () => {
    if (hint || ctx.state === 'running' || typeof document === 'undefined') return;
    hint = document.createElement('button');
    hint.textContent = 'Tap for sound';
    hint.setAttribute('aria-label', 'Turn on sound');
    hint.style.cssText = 'position:absolute;left:12px;top:12px;z-index:2147483646;font:13px system-ui,sans-serif;padding:6px 12px;border-radius:16px;border:0;background:rgba(0,0,0,.65);color:#fff;cursor:pointer';
    hint.onclick = (e) => (e.stopPropagation(), resume());
    if (parent && getComputedStyle(parent).position === 'static') parent.style.position = 'relative';
    (parent ?? document.body).append(hint);
  };

  // Measuring (the test pattern): when each beep starts, as heard, and when each flash was drawn.
  const beeps = [], flashes = [];
  stats.avSyncMs = [];
  const pair = () => {
    // Each beep with the flash nearest it (they come a second apart; within 300 ms is the same one).
    while (beeps.length && flashes.length) {
      const b = beeps[0];
      const f = flashes.reduce((x, y) => (Math.abs(y - b) < Math.abs(x - b) ? y : x));
      if (Math.abs(f - b) > 300) {
        // No flash for it yet, or none at all: wait a little, then forget it.
        if (performance.now() - b > 1000) beeps.shift();
        else break;
        continue;
      }
      stats.avSyncMs.push(+(b - f).toFixed(1));
      if (stats.avSyncMs.length > 1000) stats.avSyncMs.shift();
      beeps.shift();
      flashes.splice(flashes.indexOf(f), 1);
    }
    while (flashes.length && performance.now() - flashes[0] > 2000) flashes.shift();
  };
  if (measure && ctx.audioWorklet) {
    // Onsets found sample by sample on what goes out: |x| over 0.1 after 50 ms of quiet.
    const code = `registerProcessor('rr-onset', class extends AudioWorkletProcessor {
      constructor() { super(); this.quiet = 0; }
      process([input]) {
        const x = input[0];
        if (x) for (let i = 0; i < x.length; i++) {
          if (Math.abs(x[i]) > 0.1) { if (this.quiet > 2400) this.port.postMessage((currentFrame + i) / sampleRate); this.quiet = 0; }
          else this.quiet++;
        }
        return true;
      }
    });`;
    ctx.audioWorklet.addModule(URL.createObjectURL(new Blob([code], {type: 'text/javascript'}))).then(() => {
      const node = new AudioWorkletNode(ctx, 'rr-onset');
      out.connect(node);
      node.port.onmessage = (e) => (beeps.push(toLocalUs(e.data) / 1000 - performance.timeOrigin), pair());
    }, () => {});
  }

  return {
    packet(tsUs, data) {
      a.packets++;
      // How long after capture packets arrive (for the stats: the network and the server's share).
      arriveUs.push(localUs() - tsUs);
      if (arriveUs.length > 100) arriveUs.shift();
      a.arriveMs = +(median(arriveUs) / 1000).toFixed(1);
      showHint();
      // Timestamps from the first packet on (µs fits a double either way; small ones are tidier).
      base ??= tsUs;
      try {
        decoder.decode(new EncodedAudioChunk({type: 'key', timestamp: tsUs - base, data}));
      } catch (e) {
        a.decoder = `error: ${e.message ?? e}`;
      }
    },
    /** A frame was drawn now, captured at `captureUs` (server clock). */
    shown(captureUs, flash) {
      videoUs.push(localUs() - captureUs);
      if (videoUs.length > 120) videoUs.shift();
      a.videoMs = +(median(videoUs) / 1000).toFixed(1);
      update();
      if (flash) (flashes.push(performance.now()), pair());
    },
    close() {
      gestures.forEach((g) => document.removeEventListener(g, onGesture, true));
      hint?.remove();
      try {
        decoder.close();
      } catch {}
      ctx.close();
    },
  };
}

// ---- the connection ----

/**
 * WebTransport when it's offered (`wt`: {url, hash}, the certificate's SHA-256) and the browser has it:
 * QUIC, each video frame on a stream of its own, so a late one never holds up the next. Otherwise, or
 * if it doesn't open (UDP blocked, an old browser), a WebSocket, which works through anything.
 * `ondata` gets whole protocol frames.
 */
function openLink(url, wt) {
  const link = {kind: '', onopen: () => {}, ondata: () => {}, onclose: () => {}, send: () => false, close: () => {}};
  const websocket = () => {
    const ws = new WebSocket(url);
    ws.binaryType = 'arraybuffer';
    link.kind = 'websocket';
    link.send = (b) => ws.readyState === 1 && (ws.send(b), true);
    link.close = () => ws.close();
    ws.onopen = () => link.onopen();
    ws.onclose = () => link.onclose();
    ws.onmessage = (ev) => link.ondata(new Uint8Array(ev.data));
  };
  if (!wt || typeof WebTransport === 'undefined') {
    websocket();
    return link;
  }
  (async () => {
    let t;
    try {
      // A self-signed certificate is accepted by its hash; a real one is checked as for any site.
      t = new WebTransport(wt.url, wt.hash ? {serverCertificateHashes: [{algorithm: 'sha-256', value: new Uint8Array(wt.hash)}]} : {});
      await Promise.race([t.ready, new Promise((_, no) => setTimeout(() => no(new Error('timeout')), 3000))]);
    } catch {
      try {
        t?.close();
      } catch {}
      return websocket();
    }
    link.kind = 'webtransport';
    const control = await t.createBidirectionalStream();
    const writer = control.writable.getWriter();
    link.send = (b) => (writer.write(b).catch(() => {}), true);
    link.close = () => t.close();
    t.closed.then(() => link.onclose(), () => link.onclose());
    // Control messages: a byte stream, cut into whole frames.
    (async () => {
      const reader = control.readable.getReader();
      let buf = new Uint8Array(0);
      for (;;) {
        const {value, done} = await reader.read().catch(() => ({done: true}));
        if (done) return;
        const joined = new Uint8Array(buf.length + value.length);
        joined.set(buf);
        joined.set(value, buf.length);
        let at = 0;
        while (at + 7 <= joined.length) {
          const len = 7 + new DataView(joined.buffer, joined.byteOffset + at + 3, 4).getUint32(0, true);
          if (at + len > joined.length) break;
          link.ondata(joined.subarray(at, at + len));
          at += len;
        }
        buf = joined.slice(at);
      }
    })();
    // Video as datagrams (with parity), when the server sends it so: put back together here.
    link.fec = new FecReassembler(8);
    (async () => {
      const reader = t.datagrams?.readable.getReader();
      if (!reader) return;
      for (;;) {
        const {value, done} = await reader.read().catch(() => ({done: true}));
        if (done) return;
        // Sound comes whole, a protocol frame a datagram (its first byte its kind); video in shards.
        if (value[0] === K.AUDIO) {
          link.ondata(value);
          continue;
        }
        const frame = link.fec.push(value);
        if (frame) link.ondata(frame);
      }
    })();
    // Video: one stream a frame, read whole.
    (async () => {
      const streams = t.incomingUnidirectionalStreams.getReader();
      for (;;) {
        const {value: stream, done} = await streams.read().catch(() => ({done: true}));
        if (done) return;
        new Response(stream).arrayBuffer().then((b) => link.ondata(new Uint8Array(b)), () => {});
      }
    })();
    link.onopen();
  })();
  return link;
}

/**
 * A link whose every message is sealed (AES-256-GCM, keys derived with HKDF from the link's key and
 * the salt the machine sends first; one key a direction, a counter nonce): what's between (a relay)
 * can't read or change it. The same scheme as crates/rr-server/src/e2e.rs.
 */
function sealed(inner, keyB64) {
  const link = {kind: inner.kind, onopen: () => {}, ondata: () => {}, onclose: () => {}, send: () => true, close: () => inner.close()};
  if (!crypto?.subtle) {
    // WebCrypto needs a secure page (HTTPS or localhost).
    setTimeout(() => (link.onerror?.('end-to-end encryption needs the page over HTTPS'), link.onclose()));
    return link;
  }
  const raw = Uint8Array.from(atob(keyB64.replace(/-/g, '+').replace(/_/g, '/')), (c) => c.charCodeAt(0));
  const enc = new TextEncoder();
  let keys, sent = 0n, last = -1n;
  let ready;
  const keysReady = new Promise((r) => (ready = r));
  let inbound = Promise.resolve(), outbound = Promise.resolve();
  const iv = (n) => {
    const b = new Uint8Array(12);
    new DataView(b.buffer).setBigUint64(4, n);
    return b;
  };
  const derive = async (salt) => {
    const base = await crypto.subtle.importKey('raw', raw, 'HKDF', false, ['deriveKey']);
    const k = (info, use) => crypto.subtle.deriveKey({name: 'HKDF', hash: 'SHA-256', salt, info: enc.encode(info)}, base, {name: 'AES-GCM', length: 256}, false, [use]);
    keys = {send: await k('rein-remote e2e: viewer to machine', 'encrypt'), recv: await k('rein-remote e2e: machine to viewer', 'decrypt')};
    ready();
  };
  const prefix = enc.encode('RRE2E1');
  inner.onopen = () => {
    link.kind = inner.kind;
    link.onopen();
  };
  inner.onclose = () => link.onclose();
  inner.ondata = (bytes) => {
    // In order: each message waits for the one before it.
    inbound = inbound.then(async () => {
      if (!keys) {
        if (bytes.length === prefix.length + 16 && prefix.every((c, i) => bytes[i] === c)) await derive(bytes.slice(prefix.length));
        return;
      }
      const n = new DataView(bytes.buffer, bytes.byteOffset, 8).getBigUint64(0);
      if (n <= last) return; // a replay
      try {
        const plain = await crypto.subtle.decrypt({name: 'AES-GCM', iv: iv(n)}, keys.recv, bytes.subarray(8));
        last = n;
        link.ondata(new Uint8Array(plain));
      } catch {
        link.onerror?.("a message didn't open: the link's key is wrong, or something changed it on the way");
      }
    });
  };
  link.send = (bytes) => {
    outbound = outbound.then(async () => {
      await keysReady;
      sent += 1n;
      const ct = new Uint8Array(await crypto.subtle.encrypt({name: 'AES-GCM', iv: iv(sent)}, keys.send, bytes));
      const out = new Uint8Array(8 + ct.length);
      new DataView(out.buffer).setBigUint64(0, sent);
      out.set(ct, 8);
      inner.send(out);
    });
    return true;
  };
  return link;
}

// ---- the session ----

/**
 * Connect to a server and show its screen on `canvas`. Returns {close, stats}: stats has the frames
 * shown, keyframes, the decoder (hardware or software) and the last round trip.
 */
export function connect(url, canvas, opts = {}) {
  // `measure`: read the test pattern's barcode (the capture time) from each frame shown, for glass-
  // to-glass latency when the server shares this device's clock (rein-remote serve --test-pattern).
  const measure = opts.measure ?? (typeof location !== 'undefined' && new URLSearchParams(location.search).has('measure'));
  const ctx = canvas.getContext('2d', measure ? {willReadFrequently: true} : undefined);
  // Through a relay, sealed end to end: the key is in the page's address after # (never sent to a server).
  const e2eKey = opts.e2eKey ?? (typeof location !== 'undefined' ? new URLSearchParams(location.hash.slice(1)).get('k') : null);
  const link = e2eKey ? sealed(openLink(url, opts.webtransport), e2eKey) : openLink(url, opts.webtransport);
  const stats = {frames: 0, keyframes: 0, bytes: 0, decoder: '', rttMs: 0, state: 'connecting', error: '', glassMs: [], refreshHz: 0, fps: 0, audio: null};
  // Sound, unless turned off (`audio: false`) or the browser can't play it.
  const soundOk = opts.audio === false ? Promise.resolve(false) : canPlaySound();
  let sound = null;
  // Capture times of frames being decoded (frame number → µs, server clock), for the sound's offset.
  const captured = new Map();
  // Measured while the connection opens.
  const refresh = opts.refreshMhz !== undefined ? Promise.resolve(opts.refreshMhz) : measureRefresh();
  let config, decoder, waitKey = true, lost = 0, codec, wasLit = false;
  const sent = new Map(); // frame number → received time (µs, client clock)
  const shown = []; // when the last second's frames were shown
  const send = (bytes) => link.send(bytes);

  const helloFrame = async () => {
    const w = new W();
    w.u16(0); w.u16(0); // protocol versions spoken
    w.str(opts.name ?? (navigator.userAgentData?.platform || navigator.platform || 'browser'));
    // What this browser decodes (hardware first is the server's preference to honor).
    const caps = [];
    if (!webcodecs && MS?.isTypeSupported('video/mp4; codecs="avc1.42e01f"')) caps.push([1, 0]); // H.264 through Media Source
    for (const [id, c] of webcodecs ? [[1, 'avc1.42e01f'], [2, HEVC], [4, 'vp09.00.10.08'], [3, 'av01.0.04M.08']] : []) {
      try {
        const hw = await VideoDecoder.isConfigSupported({codec: c, hardwareAcceleration: 'prefer-hardware'});
        const sw = hw.supported ? hw : await VideoDecoder.isConfigSupported({codec: c});
        if (sw.supported) caps.push([id, hw.supported ? 1 : 0]);
      } catch {}
    }
    w.u8(caps.length);
    for (const [id, hw] of caps) (w.u8(id), w.u8(hw));
    w.u16(screen.width * devicePixelRatio); w.u16(screen.height * devicePixelRatio); w.u16(240);
    // Sound: this plays Opus.
    const plays = await soundOk;
    if (plays) {
      try {
        sound = makeSound(stats, measure, canvas.parentNode);
      } catch {}
    }
    w.u8(sound ? 1 : 0);
    w.u8(1 | 2 | 4); // pointer, keyboard, touch
    // Hello version 1: this display's refresh rate, measured, so the server paces to it.
    stats.refreshHz = (await refresh) / 1000;
    w.u32(await refresh);
    // Hello version 2: video as datagrams with parity, over WebTransport; sound as datagrams too.
    const dgrams = link.fec && opts.datagrams !== false;
    w.u32((dgrams ? 1 : 0) | (dgrams && sound ? 2 : 0));
    return w.frame(K.HELLO, 2);
  };

  const makeDecoder = () => {
    try {
      if (decoder && decoder.state !== 'closed') decoder.close();
    } catch {}
    decoder = new (webcodecs ? VideoDecoder : MseDecoder)({
      output: (frame) => {
        if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
          canvas.width = frame.displayWidth;
          canvas.height = frame.displayHeight;
        }
        ctx.drawImage(frame.image ?? frame, 0, 0);
        if (measure && canvas.height >= 24 && canvas.width >= 34 * 24) {
          const row = ctx.getImageData(0, canvas.height - 12, 34 * 24, 1).data;
          const bit = (k) => row[(k * 24 + 12) * 4 + 1] > 128;
          if (bit(0) && !bit(1)) {
            let ms = 0;
            for (let k = 2; k < 34; k++) ms = ms * 2 + (bit(k) ? 1 : 0);
            const now = Date.now() % 2 ** 32;
            const d = (now - ms + 2 ** 32) % 2 ** 32;
            if (d < 10_000) stats.glassMs.push(d), stats.glassMs.length > 1000 && stats.glassMs.shift();
          }
        }
        const n = Number(frame.timestamp);
        frame.close();
        // The flash in the test pattern's top left sixth (measuring sound against picture).
        let flash = false;
        if (measure && sound && canvas.width >= 60) {
          const px = ctx.getImageData(Math.floor(canvas.width / 12), Math.floor(canvas.height / 12), 1, 1).data;
          const lit = px[0] > 200 && px[1] > 200 && px[2] > 200;
          flash = lit && !wasLit;
          wasLit = lit;
        }
        const capturedUs = captured.get(n);
        captured.delete(n);
        if (sound && capturedUs !== undefined) sound.shown(capturedUs, flash);
        stats.frames++;
        // Frames shown in the last second.
        const t = performance.now();
        shown.push(t);
        while (shown[0] < t - 1000) shown.shift();
        stats.fps = shown.length;
        const received = sent.get(n);
        sent.delete(n);
        // Feedback: what arrived and when it was on screen (drives the server's rate control).
        const w = new W();
        w.u32(n); w.u64(received ?? nowUs()); w.u64(nowUs()); w.u16(lost);
        // Version 1: the share of datagrams lost (the server sends parity for it).
        w.u16(link.fec ? link.fec.lossPermille() : 0);
        if (link.fec) (stats.fecRecovered = link.fec.recovered), (stats.fecLost = link.fec.lost);
        lost = 0;
        send(w.frame(K.FEEDBACK, 1));
      },
      error: (e) => {
        stats.error = String(e.message ?? e);
        waitKey = true;
        send(new W().frame(K.KEYFRAME_REQUEST));
        makeDecoder();
        if (codec) decoder.configure(codec);
      },
    });
  };

  // Frames that came while the decoder was being set up, decoded once it is.
  const pending = [];
  const decodeOne = ({key, n, data}) => {
    waitKey = false;
    if (key) stats.keyframes++;
    sent.set(n, nowUs());
    try {
      const chunk = {type: key ? 'key' : 'delta', timestamp: n, data};
      decoder.decode(webcodecs ? new EncodedVideoChunk(chunk) : chunk);
    } catch (err) {
      stats.error = String(err.message ?? err);
      waitKey = true;
      send(new W().frame(K.KEYFRAME_REQUEST));
    }
  };

  let pingTimer = 0, pingId = 0;
  link.onopen = async () => {
    stats.state = 'handshake';
    stats.transport = link.kind;
    send(await helloFrame());
    // Our own round trip, each second (the server measures its own the same way).
    pingTimer = setInterval(() => {
      const w = new W();
      w.u32(++pingId); w.u64(nowUs());
      send(w.frame(K.PING));
    }, 1000);
  };
  link.onclose = () => ((stats.state = 'closed'), clearInterval(pingTimer));
  link.onerror = (e) => (stats.error = e);
  link.ondata = (bytes) => {
    // A sealed session (through a relay) opened without its key: say what's missing.
    if (!e2eKey && bytes.length === 22 && new TextDecoder().decode(bytes.subarray(0, 6)) === 'RRE2E1') {
      stats.error = "this link is missing its key (the part after #k=): open the whole link the machine printed";
      return link.close();
    }
    for (const f of frames(bytes)) {
      const r = new R(f.body);
      try {
        if (f.kind === K.CONFIG) {
          config = {codec: CODECS[r.u8()], width: r.u16(), height: r.u16(), fps: r.u16(), bitrateKbps: r.u32()};
          stats.state = 'streaming';
          codec = undefined;
          waitKey = true;
          makeDecoder();
        } else if (f.kind === K.VIDEO) {
          const key = (f.flags & FLAG_KEYFRAME) !== 0;
          const n = r.u32();
          const capturedUs = Number(r.u64()); // capture time, server clock
          const data = r.bytes();
          captured.set(n, capturedUs);
          if (captured.size > 300) captured.delete(captured.keys().next().value);
          stats.bytes += data.length;
          if (waitKey && !key) {
            lost++;
            continue;
          }
          if (!codec) {
            // Hardware decoding where the device has it, else whatever this browser decodes with.
            codec = {codec: config?.codec === 'h264' ? avcCodec(data) : config?.codec === 'h265' ? HEVC : 'vp09.00.10.08', optimizeForLatency: true, width: config?.width, height: config?.height};
            pending.push({key, n, data});
            if (!webcodecs) {
              decoder.configure(codec);
              stats.decoder = `${codec.codec} (Media Source: this page isn't HTTPS)`;
              for (const p of pending.splice(0)) decodeOne(p);
              continue;
            }
            VideoDecoder.isConfigSupported({...codec, hardwareAcceleration: 'prefer-hardware'}).then(
              (r) => {
                if (r.supported) codec.hardwareAcceleration = 'prefer-hardware';
                decoder.configure(codec);
                stats.decoder = `${codec.codec}${r.supported ? ' (hardware)' : ' (software)'}`;
                for (const p of pending.splice(0)) decodeOne(p);
              },
              () => (decoder.configure(codec), (stats.decoder = codec.codec), pending.splice(0).forEach(decodeOne)),
            );
            continue;
          }
          if (decoder.state !== 'configured') {
            pending.push({key, n, data});
            continue;
          }
          decodeOne({key, n, data});
        } else if (f.kind === K.AUDIO) {
          const ts = Number(r.u64());
          const data = r.bytes();
          sound?.packet(ts, data);
        } else if (f.kind === K.CURSOR) {
          const x = r.u16(), y = r.u16(), visible = r.u8() !== 0;
          if (r.u8() === 1) {
            const w = r.u16(), h = r.u16(), hx = r.u16(), hy = r.u16(), rgba = r.bytes();
            pointer.shape(w, h, hx, hy, rgba);
          }
          pointer.remote(x, y, visible);
        } else if (f.kind === K.PING) {
          // The server measures the round trip: answer at once.
          const w = new W();
          w.u32(r.u32()); w.u64(r.u64());
          send(w.frame(K.PONG));
        } else if (f.kind === K.PONG) {
          r.u32();
          stats.rttMs = Number(nowUs() - r.u64()) / 1000;
        }
      } catch (err) {
        stats.error = String(err.message ?? err);
      }
    }
  };

  // ---- the pointer, drawn here: at once where the local mouse is, else where the server says ----
  const pointer = (() => {
    const el = document.createElement('canvas');
    el.style.cssText = 'position:fixed;left:0;top:0;pointer-events:none;z-index:2147483647;display:none;image-rendering:pixelated';
    (canvas.parentNode ?? document.body).append(el);
    let hot = [0, 0], remoteAt = [0, 0], localAt = null, localTime = 0, visible = true, hasShape = false;
    const place = () => {
      const rect = canvas.getBoundingClientRect();
      if (!hasShape || !visible || !canvas.width) return (el.style.display = 'none');
      const k = Math.max(rect.width / canvas.width, 0.6);
      el.style.width = `${el.width * k}px`;
      el.style.height = `${el.height * k}px`;
      let [x, y] = localAt && performance.now() - localTime < 250 ? localAt : remoteAt.map((v, i) => (i ? rect.top + (v * rect.height) / canvas.height : rect.left + (v * rect.width) / canvas.width));
      el.style.transform = `translate(${x - hot[0] * k}px, ${y - hot[1] * k}px)`;
      el.style.display = '';
    };
    return {
      shape(w, h, hx, hy, rgba) {
        el.width = w;
        el.height = h;
        if (w && h) el.getContext('2d').putImageData(new ImageData(new Uint8ClampedArray(rgba), w, h), 0, 0);
        hot = [hx, hy];
        hasShape = w > 0 && h > 0;
        // The remote pointer is drawn: hide this device's over the screen.
        canvas.style.cursor = hasShape ? 'none' : '';
        place();
      },
      remote(x, y, v) {
        remoteAt = [x, y];
        visible = v;
        place();
      },
      local(e) {
        localAt = [e.clientX, e.clientY];
        localTime = performance.now();
        place();
      },
      leave() {
        localAt = null;
        place();
      },
      remove: () => el.remove(),
    };
  })();

  // ---- input ----
  const at = (e) => {
    const rect = canvas.getBoundingClientRect();
    return [Math.round(((e.clientX - rect.left) * canvas.width) / rect.width), Math.round(((e.clientY - rect.top) * canvas.height) / rect.height)];
  };
  const input = (fill) => {
    const w = new W();
    fill(w);
    send(w.frame(K.INPUT));
  };
  const move = (x, y) => input((w) => (w.u8(1), w.u16(Math.max(0, x)), w.u16(Math.max(0, y))));
  const button = (b, down) => input((w) => (w.u8(3), w.u8(b), w.u8(down ? 1 : 0)));
  const btn = (b) => (b === 2 ? 1 : b === 1 ? 2 : b === 3 ? 3 : b === 4 ? 4 : 0);
  canvas.tabIndex = 0;
  canvas.style.touchAction = 'none';
  let lastMove = 0;
  // A mouse or pen: straight through. A finger: gestures (below).
  canvas.addEventListener('pointerdown', (e) => {
    canvas.focus({preventScroll: true});
    e.preventDefault();
    canvas.setPointerCapture?.(e.pointerId);
    if (e.pointerType === 'touch') return touch.down(e);
    move(...at(e));
    button(btn(e.button), true);
  });
  canvas.addEventListener('pointerup', (e) => {
    if (e.pointerType === 'touch') return touch.up(e);
    move(...at(e));
    button(btn(e.button), false);
  });
  canvas.addEventListener('pointercancel', (e) => e.pointerType === 'touch' && touch.up(e, true));
  canvas.addEventListener('pointerleave', (e) => e.pointerType !== 'touch' && pointer.leave());
  canvas.addEventListener('pointermove', (e) => {
    if (e.pointerType === 'touch') return touch.move(e);
    pointer.local(e);
    if (performance.now() - lastMove < 8) return;
    lastMove = performance.now();
    move(...at(e));
  });

  // Touch, for a server that drives a mouse (as every desktop does): tap clicks, a swipe scrolls,
  // hold then move drags, a two-finger tap right-clicks.
  const touch = (() => {
    const fingers = new Map(); // pointerId → {x, y, x0, y0, t0}
    let mode = 'idle'; // idle | maybe | scroll | drag | two
    let holdTimer = 0;
    let scrollAt = null;
    const HOLD_MS = 400, SLOP = 10;
    const wheel = (dx, dy) => input((w) => (w.u8(4), w.i16(Math.round(dx)), w.i16(Math.round(dy))));
    return {
      down(e) {
        fingers.set(e.pointerId, {x: e.clientX, y: e.clientY, x0: e.clientX, y0: e.clientY, t0: performance.now()});
        if (fingers.size === 1) {
          mode = 'maybe';
          clearTimeout(holdTimer);
          holdTimer = setTimeout(() => {
            if (mode !== 'maybe') return;
            mode = 'drag';
            move(...at(e));
            button(0, true);
            navigator.vibrate?.(15);
          }, HOLD_MS);
        } else if (fingers.size === 2 && mode === 'maybe') {
          mode = 'two';
          clearTimeout(holdTimer);
        }
      },
      move(e) {
        const f = fingers.get(e.pointerId);
        if (!f) return;
        const [px, py] = [f.x, f.y];
        f.x = e.clientX;
        f.y = e.clientY;
        if (mode === 'drag') return move(...at(e));
        if (mode === 'maybe' && Math.hypot(f.x - f.x0, f.y - f.y0) > SLOP) {
          mode = 'scroll';
          clearTimeout(holdTimer);
          scrollAt = at(e);
          move(...scrollAt); // scroll what's under the finger
        }
        if (mode === 'scroll') {
          // Content follows the finger: moving up scrolls down. Canvas pixels, like a trackpad.
          const k = canvas.width / canvas.getBoundingClientRect().width;
          wheel((px - f.x) * k, (py - f.y) * k);
        }
      },
      up(e, cancelled = false) {
        const f = fingers.get(e.pointerId);
        fingers.delete(e.pointerId);
        clearTimeout(holdTimer);
        if (!f) return;
        if (mode === 'drag') {
          move(...at(e));
          button(0, false);
        } else if (mode === 'maybe' && !cancelled) {
          move(...at(e));
          button(0, true);
          button(0, false);
        } else if (mode === 'two' && fingers.size === 1 && !cancelled) {
          const o = [...fingers.values()][0];
          move(...at({clientX: (o.x + f.x) / 2, clientY: (o.y + f.y) / 2}));
          button(1, true);
          button(1, false);
          mode = 'done';
          return;
        }
        if (fingers.size === 0) mode = 'idle';
      },
    };
  })();
  canvas.addEventListener('wheel', (e) => (e.preventDefault(), input((w) => (w.u8(4), w.i16(Math.round(e.deltaX)), w.i16(Math.round(e.deltaY))))), {passive: false});
  canvas.addEventListener('contextmenu', (e) => e.preventDefault());
  const key = (e, down) => {
    e.preventDefault();
    input((w) => (w.u8(5), w.u16(0), w.u8(down ? 1 : 0), w.str(e.key)));
  };
  canvas.addEventListener('keydown', (e) => key(e, true));
  canvas.addEventListener('keyup', (e) => key(e, false));
  canvas.addEventListener('paste', (e) => {
    const text = e.clipboardData?.getData('text');
    if (text) (e.preventDefault(), input((w) => (w.u8(6), w.str(text))));
  });

  // The device's own keyboard (a phone, a tablet): a hidden text field takes the typing. Text goes as
  // text (any language, emoji, autocorrect); Backspace and Enter go as keys.
  const keys = document.createElement('textarea');
  keys.setAttribute('autocapitalize', 'off');
  keys.setAttribute('autocomplete', 'off');
  keys.setAttribute('autocorrect', 'off');
  keys.setAttribute('spellcheck', 'false');
  keys.setAttribute('aria-label', 'Type into the remote screen');
  keys.style.cssText = 'position:fixed;left:0;bottom:0;width:1px;height:1px;opacity:0;border:0;padding:0;font-size:16px';
  const tap = (name) => (input((w) => (w.u8(5), w.u16(0), w.u8(1), w.str(name))), input((w) => (w.u8(5), w.u16(0), w.u8(0), w.str(name))));
  keys.addEventListener('beforeinput', (e) => {
    e.preventDefault();
    if (e.inputType === 'insertText' || e.inputType === 'insertReplacementText' || e.inputType === 'insertFromPaste') {
      const text = e.data ?? e.dataTransfer?.getData('text') ?? '';
      // A newline is the Enter key, not a character.
      text.split(/\r?\n/).forEach((line, i) => (i && tap('Enter'), line && input((w) => (w.u8(6), w.str(line)))));
    } else if (e.inputType === 'insertLineBreak' || e.inputType === 'insertParagraph') tap('Enter');
    else if (e.inputType.startsWith('deleteContentBackward')) tap('Backspace');
    else if (e.inputType.startsWith('deleteContentForward')) tap('Delete');
  });
  keys.addEventListener('keydown', (e) => {
    // Keys with no text (arrows, Escape, Tab) from a hardware keyboard on a tablet.
    if (e.key.length > 1 && !['Unidentified', 'Process', 'Backspace', 'Enter'].includes(e.key)) key(e, true), key(e, false);
  });
  (canvas.parentNode ?? document.body).append(keys);

  return {
    stats,
    /** Show the device's on-screen keyboard (call it from a tap: browsers allow it only then). */
    keyboard: () => keys.focus({preventScroll: true}),
    close: () => (link.close(), decoder?.close?.(), sound?.close(), keys.remove(), pointer.remove()),
  };
}

// ---- <rein-remote src="wss://…"> ----

if (typeof customElements !== 'undefined' && !customElements.get('rein-remote')) {
  customElements.define('rein-remote', class extends HTMLElement {
    connectedCallback() {
      const canvas = document.createElement('canvas');
      canvas.style.cssText = 'max-width:100%;max-height:100%;display:block;outline:none;background:#000';
      this.append(canvas);
      this.session = connect(this.getAttribute('src'), canvas);
      this.keyboard = () => this.session.keyboard();
    }
    disconnectedCallback() {
      this.session?.close();
    }
  });
}
