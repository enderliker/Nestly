import {
  createHash,
  createPublicKey,
  timingSafeEqual,
  verify as cryptoVerify,
} from 'node:crypto';

export class VerificationError extends Error {
  constructor(message) {
    super(message);
    this.name = 'VerificationError';
  }
}

const SHA256_HEX = /^[0-9a-fA-F]{64}$/;
const ASSET_NAME = /^rsbot-[a-z0-9][a-z0-9._-]*$/;
const ED25519_SIGNATURE_BYTES = 64;

/** Hex SHA-256 of a buffer. */
export function sha256Hex(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

/**
 * Compares a buffer against an expected hex digest in constant time.
 *
 * @throws {VerificationError} when the digest is not 64 hex characters.
 */
export function verifySha256(bytes, expectedHex) {
  if (typeof expectedHex !== 'string' || !SHA256_HEX.test(expectedHex)) {
    throw new VerificationError('sha256 digest must be 64 hexadecimal characters');
  }
  const expected = Buffer.from(expectedHex.toLowerCase(), 'hex');
  const actual = createHash('sha256').update(bytes).digest();
  return timingSafeEqual(actual, expected);
}

/**
 * Rejects names that are not flat `rsbot-*` release assets.
 *
 * @throws {VerificationError} for traversal, separators, NUL bytes, or an
 * unknown prefix.
 */
export function assertAssetName(name) {
  if (typeof name !== 'string' || name.length === 0 || name.length > 128) {
    throw new VerificationError(`invalid release asset name: ${String(name)}`);
  }
  if (
    name.includes('/') ||
    name.includes('\\') ||
    name.includes('\u0000') ||
    name.includes('..') ||
    name.startsWith('.')
  ) {
    throw new VerificationError(`invalid release asset name: ${name}`);
  }
  if (!ASSET_NAME.test(name) || name === 'rsbot-') {
    throw new VerificationError(`invalid release asset name: ${name}`);
  }
  return name;
}

function decodePublicKey(value) {
  if (typeof value !== 'string' || value.trim().length === 0) {
    throw new VerificationError('a pinned Ed25519 public key is required');
  }
  try {
    if (value.includes('BEGIN PUBLIC KEY')) {
      return createPublicKey(value);
    }
    const der = Buffer.from(value.trim(), 'base64');
    if (der.length === 0) {
      throw new VerificationError('the pinned public key is empty');
    }
    return createPublicKey({ key: der, format: 'der', type: 'spki' });
  } catch (source) {
    if (source instanceof VerificationError) {
      throw source;
    }
    throw new VerificationError(`invalid Ed25519 public key: ${source.message}`);
  }
}

/**
 * Verifies a detached Ed25519 signature over the exact manifest bytes.
 *
 * @returns {boolean} whether the signature is valid.
 * @throws {VerificationError} for a malformed key or signature.
 */
export function verifyManifestSignature(manifestBytes, signatureBytes, publicKeyValue) {
  if (!Buffer.isBuffer(manifestBytes)) {
    throw new VerificationError('manifest bytes must be a Buffer');
  }
  if (
    !Buffer.isBuffer(signatureBytes) ||
    signatureBytes.length !== ED25519_SIGNATURE_BYTES
  ) {
    throw new VerificationError('Ed25519 signature must be exactly 64 bytes');
  }
  const key = decodePublicKey(publicKeyValue);
  try {
    return cryptoVerify(null, manifestBytes, key, signatureBytes);
  } catch (source) {
    throw new VerificationError(`cannot verify the manifest signature: ${source.message}`);
  }
}

export { SHA256_HEX };