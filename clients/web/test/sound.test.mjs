// Placing sound in time with the picture (rein-remote.js): node --test clients/web/test
import assert from 'node:assert/strict';
import test from 'node:test';
import {placePacket, sharedOffset} from '../rein-remote.js';

test('the shared offset is the picture’s, or later when the sound needs longer', () => {
  // Picture ready ~20 ms after capture, sound heard ~15 ms after: the picture's (sound waits).
  assert.equal(sharedOffset([19000, 20000, 21000], [14000, 15000, 16000]), 30000);
  assert.equal(sharedOffset([40000, 41000, 42000], [14000, 15000, 16000]), 41000);
  // Sound needs 50 ms: its median and the jitter buffer (the picture waits for it then).
  assert.equal(sharedOffset([19000, 20000, 21000], [49000, 50000, 51000]), 65000);
  // A burst of late packets (the page stalled) doesn't move it: medians.
  assert.equal(sharedOffset([20000], [50000, 50000, 50000, 50000, 50000, 300000, 300000]), 65000);
  // Nothing yet from one side: the other's.
  assert.equal(sharedOffset([], [50000]), 65000);
  assert.equal(sharedOffset([20000], []), 20000);
  assert.equal(sharedOffset([], []), undefined);
});

test('packets play back to back, stretched gently, realigned past 20 ms', () => {
  const d = 0.01;
  // The first goes where it should.
  assert.deepEqual(placePacket(0, 1.0, d, 0.5), {at: 1.0, rate: 1, playhead: 1.01});
  // On time: right after the last one, at normal speed.
  let p = placePacket(1.01, 1.01, d, 0.5);
  assert.equal(p.at, 1.01);
  assert.equal(p.rate, 1);
  // 5 ms behind the picture: still back to back, a little faster to catch up (never over 1%).
  p = placePacket(1.015, 1.01, d, 0.5);
  assert.equal(p.at, 1.015);
  assert.ok(p.rate > 1 && p.rate <= 1.01);
  assert.ok(p.playhead < 1.025);
  // 5 ms ahead: a little slower.
  p = placePacket(1.005, 1.01, d, 0.5);
  assert.ok(p.rate < 1 && p.rate >= 0.99);
  // 30 ms off either way: moved to where it should be.
  assert.deepEqual(placePacket(1.04, 1.01, d, 0.5), {at: 1.01, rate: 1, playhead: 1.02});
  assert.deepEqual(placePacket(0.98, 1.01, d, 0.5), {at: 1.01, rate: 1, playhead: 1.02});
  // Behind by more than 20 ms and too late to be on time: dropped, the playhead kept.
  assert.deepEqual(placePacket(1.04, 1.01, d, 1.02), {drop: true, playhead: 1.04});
  // Silence moves at once (nobody hears it), and is dropped when it would be late.
  assert.deepEqual(placePacket(1.015, 1.01, d, 0.5, true), {at: 1.01, rate: 1, playhead: 1.02});
  assert.deepEqual(placePacket(1.015, 1.01, d, 1.012, true).drop, true);
  // Within 20 ms but past the earliest it can start: as soon as it can.
  p = placePacket(1.01, 1.008, d, 1.012);
  assert.equal(p.at, 1.012);
});
