import assert from 'node:assert/strict';
import {
  createHash,
  generateKeyPairSync,
  sign,
} from 'node:crypto';
import test from 'node:test';

import {
  VerificationError,
  assertAssetName,
  sha256Hex,
  verifyManifestSignature,
  verifySha256,
} from '../lib/verify.mjs';

test('hashes bytes with SHA-256', () => {
  assert.equal(
    sha256Hex(Buffer.alloc(0)),
    'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
  );
  assert.equal(
    sha256Hex(Buffer.from('abc')),
    'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
  );
});

test('compares hashes and rejects malformed digests', () => {
  const digest = sha256Hex(Buffer.from('payload'));
  assert.equal(verifySha256(Buffer.from('payload'), digest), true);
  assert.equal(verifySha256(Buffer.from('payload'), digest.toUpperCase()), true);
  assert.equal(verifySha256(Buffer.from('tampered'), digest), false);
  for (const bad of ['', 'abc', 'z'.repeat(64), `${digest}00`]) {
    assert.throws(() => verifySha256(Buffer.from('payload'), bad), VerificationError);
  }
});

test('accepts only release asset names', () => {
  for (const name of [
    'nestly-supervisor-x86_64-unknown-linux-musl',
    'nestly-edge-aarch64-unknown-linux-musl',
    'nestly-mod-music-x86_64-unknown-linux-musl',
    'nestly-manifest.json',
    'nestly-manifest.json.sig',
    'rsbot-supervisor-x86_64-unknown-linux-musl',
    'rsbot-edge-aarch64-unknown-linux-musl',
    'rsbot-mod-music-x86_64-unknown-linux-musl',
    'rsbot-manifest.json',
    'rsbot-manifest.json.sig',
  ]) {
    assert.equal(assertAssetName(name), name);
  }
  for (const name of [
    '../nestly-supervisor',
    'nestly-supervisor/../escape',
    '../rsbot-supervisor',
    'rsbot-supervisor/../escape',
    '/etc/passwd',
    'nestly-',
    'rsbot-',
    'other-binary',
    'nestly-supervisor\u0000',
    'nestly-supervisor\\..\\x',
  ]) {
    assert.throws(() => assertAssetName(name), VerificationError, name);
  }
});

test('verifies an Ed25519 signature over the exact manifest bytes', () => {
  const { publicKey, privateKey } = generateKeyPairSync('ed25519');
  const manifest = Buffer.from('{"schema":1}');
  const signature = sign(null, manifest, privateKey);
  const pem = publicKey.export({ type: 'spki', format: 'pem' });
  assert.equal(verifyManifestSignature(manifest, signature, pem), true);
  assert.equal(
    verifyManifestSignature(Buffer.from('{"schema":2}'), signature, pem),
    false,
  );

  const der = publicKey.export({ type: 'spki', format: 'der' });
  assert.equal(
    verifyManifestSignature(manifest, signature, der.toString('base64')),
    true,
  );
});

test('rejects malformed public keys and signatures', () => {
  assert.throws(
    () => verifyManifestSignature(Buffer.from('x'), Buffer.alloc(64), 'not-a-key'),
    VerificationError,
  );
  const { publicKey } = generateKeyPairSync('ed25519');
  const pem = publicKey.export({ type: 'spki', format: 'pem' });
  assert.throws(
    () => verifyManifestSignature(Buffer.from('x'), Buffer.alloc(3), pem),
    VerificationError,
  );
});

test('uses the same hash function as the downloader', () => {
  const payload = Buffer.from('streamed asset');
  assert.equal(sha256Hex(payload), createHash('sha256').update(payload).digest('hex'));
});