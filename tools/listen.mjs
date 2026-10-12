#!/usr/bin/env node
// Listen to a rein-remote server without a browser: connects to its WebSocket as a client that plays
// sound, counts what comes (audio packets, their size, video frames) for a while, and prints it with
// the server's own /stats (whose "audio" says how loud what it captured was). For checking capture on
// a machine with no browser at hand: a Linux server, a container, a Windows runner.
//
//   node tools/listen.mjs 'http://HOST:PORT/?token=T' [--seconds 5] [--audio off]
//
// Prints one JSON object. Needs Node 22+ (WebSocket).

const argv = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : dflt;
};
const page = argv.find((a) => /^https?:/.test(a));
if (!page) {
  console.error('usage: node tools/listen.mjs URL [--seconds N] [--audio off]');
  process.exit(2);
}
const seconds = Number(opt('seconds', 5));
const plays = opt('audio', 'on') !== 'off';
const u = new URL(page);
const token = u.searchParams.get('token');
const ws = new WebSocket(`${u.protocol === 'https:' ? 'wss' : 'ws'}://${u.host}/stream${token ? `?token=${token}` : ''}`);
ws.binaryType = 'arraybuffer';

// A Hello (version 2): H.264 in software, sound (if asked), pointer input, 60 Hz, no datagrams.
const hello = () => {
  const b = [];
  const u16 = (v) => b.push(v & 255, (v >> 8) & 255);
  const u32 = (v) => (u16(v & 0xffff), u16(v >>> 16));
  u16(0), u16(0);
  const name = new TextEncoder().encode('listen.mjs');
  u16(name.length), b.push(...name);
  b.push(1, 1, 0); // one codec: H.264, software
  u16(1920), u16(1080), u16(60);
  b.push(plays ? 1 : 0, 1);
  u32(60000), u32(0);
  const out = new Uint8Array(7 + b.length);
  out.set([1, 2, 0]);
  new DataView(out.buffer).setUint32(3, b.length, true);
  out.set(b, 7);
  return out;
};

const got = {audioPackets: 0, audioBytes: 0, videoFrames: 0, firstAudioMs: null, serverSaysAudio: null, packetMs: []};
const t0 = Date.now();
let lastTs = null;
ws.onopen = () => ws.send(hello());
ws.onmessage = (e) => {
  const buf = new Uint8Array(e.data);
  for (let at = 0; at + 7 <= buf.length; ) {
    const v = new DataView(buf.buffer, buf.byteOffset + at);
    const kind = buf[at], len = v.getUint32(3, true);
    if (kind === 1) {
      // The server's Hello: versions, name, codecs, max size and rate, then whether it has sound.
      const codecsAt = at + 7 + 4 + 2 + v.getUint16(11, true);
      got.serverSaysAudio = buf[codecsAt + 1 + buf[codecsAt] * 2 + 6] === 1;
    }
    if (kind === 3) got.videoFrames++;
    if (kind === 4) {
      const ts = Number(v.getBigUint64(7, true));
      got.audioPackets++;
      got.audioBytes += v.getUint32(15, true);
      got.firstAudioMs ??= Date.now() - t0;
      // The packets' spacing on the server's clock (10 ms each).
      if (lastTs !== null && got.packetMs.length < 2000) got.packetMs.push((ts - lastTs) / 1000);
      lastTs = ts;
    }
    at += 7 + len;
  }
};
ws.onerror = (e) => {
  console.error(`listen.mjs: ${e.message ?? 'connection failed'}`);
  process.exit(1);
};
await new Promise((r) => setTimeout(r, seconds * 1000));
ws.close();
const spacing = [...got.packetMs].sort((a, b) => a - b);
let stats = null;
try {
  stats = await (await fetch(`${u.origin}/stats${token ? `?token=${token}` : ''}`)).json();
} catch {}
console.log(
  JSON.stringify(
    {
      seconds,
      serverSaysAudio: got.serverSaysAudio,
      videoFrames: got.videoFrames,
      audioPackets: got.audioPackets,
      audioPerSecond: +(got.audioPackets / seconds).toFixed(1),
      audioKbps: +((got.audioBytes * 8) / seconds / 1000).toFixed(1),
      firstAudioMs: got.firstAudioMs,
      packetSpacingMs: spacing.length ? {p50: spacing[spacing.length >> 1], min: spacing[0], max: spacing[spacing.length - 1]} : null,
      serverAudio: stats?.audio ?? null,
    },
    null,
    2,
  ),
);
process.exit(0);
