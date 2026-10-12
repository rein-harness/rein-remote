// pair.js against the server's own code (crates/rr-proto/src/pair.rs, crates/rr-server/src/pairing.rs):
// the vectors the Rust tests write, reproduced here; and a signature made here, which the Rust tests verify.
import assert from 'node:assert/strict';
import {readFileSync, writeFileSync} from 'node:fs';
import test from 'node:test';
import {hex, hkdf, hmac, keyMac, publicKey, sha256, sign, spakeClient, unhex, verify} from '../pair.js';

const v = JSON.parse(readFileSync(new URL('./pair-vectors.json', import.meta.url), 'utf8'));

test('SHA-256, HMAC and HKDF match their standards', () => {
  assert.equal(hex(sha256('abc')), 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
  assert.equal(hex(sha256('a'.repeat(1000))), '41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3');
  // RFC 4231 test case 2.
  assert.equal(hex(hmac(new TextEncoder().encode('Jefe'), new TextEncoder().encode('what do ya want for nothing?'))), '5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843');
  // RFC 5869 A.3.
  assert.equal(hex(hkdf(new Uint8Array(22).fill(0x0b), new Uint8Array(0), 42)), '8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8');
});

test("SPAKE2: the same points, key and confirmations as the server's code", () => {
  const c = spakeClient(v.code, unhex(v.client_random));
  assert.equal(hex(c.message), v.x);
  const k = c.finish(unhex(v.y));
  assert.equal(hex(k.shared), v.shared);
  assert.equal(hex(k.confirmMine), v.confirm_a);
  assert.equal(hex(k.confirmTheirs), v.confirm_b);
  assert.equal(hex(keyMac(k.shared, 'client', unhex(v.public))), v.key_mac);
  // A wrong code: different confirmations.
  const wrong = spakeClient('314-158', unhex(v.client_random)).finish(unhex(v.y));
  assert.notEqual(hex(wrong.confirmTheirs), v.confirm_b);
  assert.throws(() => c.finish(new Uint8Array(65).fill(4)));
});

test("ECDSA: the server's signatures verify here, and this side's there", () => {
  assert.ok(verify(unhex(v.public), unhex(v.msg), unhex(v.sig)));
  assert.ok(!verify(unhex(v.public), unhex(v.msg + '00'), unhex(v.sig)));
  // A signature made here, with fixed entropy so the file stays the same; the Rust test verifies it.
  const priv = new Uint8Array(32).fill(0x55);
  const msg = new TextEncoder().encode('rein-remote client auth v1 from pair.js');
  const sig = sign(priv, msg, new Uint8Array(32).fill(7));
  const out = {public: hex(publicKey(priv)), msg: hex(msg), sig: hex(sig)};
  assert.ok(verify(publicKey(priv), msg, sig));
  const path = new URL('./js-signature.json', import.meta.url);
  const have = (() => {
    try {
      return readFileSync(path, 'utf8');
    } catch {
      return '';
    }
  })();
  const want = JSON.stringify(out) + '\n';
  if (process.env.RR_WRITE_VECTORS || !have) writeFileSync(path, want);
  else assert.equal(have, want, 'run with RR_WRITE_VECTORS=1 to update js-signature.json');
});
