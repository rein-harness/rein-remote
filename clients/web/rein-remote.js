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

// ---- the session ----

/**
 * Connect to a server and show its screen on `canvas`. Returns {close, stats}: stats has the frames
 * shown, keyframes, the decoder (hardware or software) and the last round trip.
 */
export function connect(url, canvas, opts = {}) {
  const ctx = canvas.getContext('2d');
  const ws = new WebSocket(url);
  ws.binaryType = 'arraybuffer';
  const stats = {frames: 0, keyframes: 0, bytes: 0, decoder: '', rttMs: 0, state: 'connecting', error: ''};
  let config, decoder, waitKey = true, lost = 0, codec;
  const sent = new Map(); // frame number → received time (µs, client clock)
  const send = (bytes) => ws.readyState === 1 && ws.send(bytes);

  const helloFrame = async () => {
    const w = new W();
    w.u16(0); w.u16(0); // protocol versions spoken
    w.str(opts.name ?? (navigator.userAgentData?.platform || navigator.platform || 'browser'));
    // What this browser decodes (hardware first is the server's preference to honor).
    const caps = [];
    for (const [id, c] of [[1, 'avc1.42e01f'], [4, 'vp09.00.10.08'], [3, 'av01.0.04M.08']]) {
      try {
        const hw = await VideoDecoder.isConfigSupported({codec: c, hardwareAcceleration: 'prefer-hardware'});
        const sw = hw.supported ? hw : await VideoDecoder.isConfigSupported({codec: c});
        if (sw.supported) caps.push([id, hw.supported ? 1 : 0]);
      } catch {}
    }
    w.u8(caps.length);
    for (const [id, hw] of caps) (w.u8(id), w.u8(hw));
    w.u16(screen.width * devicePixelRatio); w.u16(screen.height * devicePixelRatio); w.u16(120);
    w.u8(0); // audio: not yet
    w.u8(1 | 2 | 4); // pointer, keyboard, touch
    return w.frame(K.HELLO);
  };

  const makeDecoder = () => {
    try {
      if (decoder && decoder.state !== 'closed') decoder.close();
    } catch {}
    decoder = new VideoDecoder({
      output: (frame) => {
        if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
          canvas.width = frame.displayWidth;
          canvas.height = frame.displayHeight;
        }
        ctx.drawImage(frame, 0, 0);
        const n = Number(frame.timestamp);
        frame.close();
        stats.frames++;
        const received = sent.get(n);
        sent.delete(n);
        // Feedback: what arrived and when it was on screen (drives the server's rate control).
        const w = new W();
        w.u32(n); w.u64(received ?? nowUs()); w.u64(nowUs()); w.u16(lost);
        lost = 0;
        send(w.frame(K.FEEDBACK));
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
      decoder.decode(new EncodedVideoChunk({type: key ? 'key' : 'delta', timestamp: n, data}));
    } catch (err) {
      stats.error = String(err.message ?? err);
      waitKey = true;
      send(new W().frame(K.KEYFRAME_REQUEST));
    }
  };

  ws.onopen = async () => {
    stats.state = 'handshake';
    send(await helloFrame());
  };
  ws.onclose = () => (stats.state = 'closed');
  ws.onmessage = (ev) => {
    for (const f of frames(new Uint8Array(ev.data))) {
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
          r.u64(); // capture time, server clock
          const data = r.bytes();
          stats.bytes += data.length;
          if (waitKey && !key) {
            lost++;
            continue;
          }
          if (!codec) {
            // Hardware decoding where the device has it, else whatever this browser decodes with.
            codec = {codec: config?.codec === 'h264' ? avcCodec(data) : 'vp09.00.10.08', optimizeForLatency: true};
            pending.push({key, n, data});
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
  canvas.addEventListener('pointerdown', (e) => {
    canvas.focus();
    e.preventDefault();
    canvas.setPointerCapture?.(e.pointerId);
    move(...at(e));
    button(btn(e.button), true);
  });
  canvas.addEventListener('pointerup', (e) => (move(...at(e)), button(btn(e.button), false)));
  canvas.addEventListener('pointermove', (e) => {
    if (performance.now() - lastMove < 8) return;
    lastMove = performance.now();
    move(...at(e));
  });
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

  return {stats, close: () => (ws.close(), decoder?.close?.())};
}

// ---- <rein-remote src="wss://…"> ----

if (typeof customElements !== 'undefined' && !customElements.get('rein-remote')) {
  customElements.define('rein-remote', class extends HTMLElement {
    connectedCallback() {
      const canvas = document.createElement('canvas');
      canvas.style.cssText = 'max-width:100%;max-height:100%;display:block;outline:none;background:#000';
      this.append(canvas);
      this.session = connect(this.getAttribute('src'), canvas);
    }
    disconnectedCallback() {
      this.session?.close();
    }
  });
}
