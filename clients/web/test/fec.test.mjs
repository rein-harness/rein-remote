// The web client's datagram reassembly against frames split by the server's code (rr-proto's fec):
// node --test clients/web/test
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import {FecReassembler, fecReconstruct} from '../rein-remote.js';

const hex = (s) => Uint8Array.from(s.match(/../g) ?? [], (x) => parseInt(x, 16));
const cases = JSON.parse(readFileSync(new URL('./fec-vectors.json', import.meta.url), 'utf8'));

test('frames come back whole from datagrams, lost ones rebuilt from parity, in any order', () => {
  for (const c of cases) {
    const r = new FecReassembler(8);
    const out = c.datagrams.map((d) => r.push(hex(d))).filter(Boolean);
    assert.equal(out.length, 1);
    assert.deepEqual(out[0], hex(c.frame));
  }
});

test('any k of k + m shards rebuild the data', () => {
  // Shards from the 2500-byte case: k data and m parity of its one block.
  const c = cases[1];
  const ds = c.datagrams.map(hex);
  const k = ds[0][9];
  const shards = [];
  for (const d of ds) shards[d[8]] = d.subarray(16);
  const data = fecReconstruct(shards, k);
  const whole = new Uint8Array(data.reduce((n, x) => n + x.length, 0));
  let at = 0;
  for (const x of data) whole.set(x, at), (at += x.length);
  assert.deepEqual(whole.subarray(0, hex(c.frame).length), hex(c.frame));
});

test('loss comes from the sequence numbers', () => {
  const r = new FecReassembler(8);
  const ds = cases[3].datagrams.map(hex);
  for (const d of ds) r.push(d);
  // Every 40th was dropped: the span of sequence numbers less those that came.
  const seqs = ds.map((d) => d[14] | (d[15] << 8));
  const unwrap = seqs.map((s) => (s < 32768 ? s + 65536 : s));
  const expected = Math.max(...unwrap) - Math.min(...unwrap) + 1;
  assert.ok(expected > ds.length);
  assert.equal(r.lossPermille(), Math.floor(((expected - ds.length) * 1000) / expected));
  assert.equal(r.lossPermille(), 0);
});

test('a datagram lost between two counts still counts', () => {
  // Counted after each datagram (a client's feedback after each frame): every gap shows in the count after it.
  const r = new FecReassembler(8);
  const unwrap = (d) => {
    const s = d[14] | (d[15] << 8);
    return s < 32768 ? s + 65536 : s;
  };
  const ds = cases[3].datagrams.map(hex).sort((a, b) => unwrap(a) - unwrap(b));
  let gaps = 0, counted = 0;
  ds.forEach((d, i) => {
    if (i > 0 && unwrap(d) - unwrap(ds[i - 1]) > 1) gaps++;
    r.push(d);
    if (r.lossPermille() > 0) counted++;
  });
  assert.ok(gaps > 0);
  assert.equal(counted, gaps);
});
