#!/usr/bin/env node
/**
 * Signs rsbot-manifest.json with an Ed25519 key.
 *
 * RSBOT_SIGNING_KEY must hold a base64-encoded PKCS#8 DER private key. When it
 * is unset the script prints a notice and exits successfully, so unsigned
 * development releases still build.
 *
 * Usage: node scripts/sign-manifest.mjs dist/rsbot-manifest.json
 */

import { createPrivateKey, sign } from 'node:crypto';
import fs from 'node:fs/promises';

const manifestPath = process.argv[2];
if (!manifestPath) {
  throw new Error('usage: sign-manifest.mjs <manifest path>');
}

const encoded = process.env.RSBOT_SIGNING_KEY;
if (!encoded) {
  console.log('RSBOT_SIGNING_KEY is unset; the manifest was not signed');
  process.exit(0);
}

const key = createPrivateKey({
  key: Buffer.from(encoded, 'base64'),
  format: 'der',
  type: 'pkcs8',
});
const bytes = await fs.readFile(manifestPath);
const signature = sign(null, bytes, key);
await fs.writeFile(`${manifestPath}.sig`, signature, { mode: 0o644 });
console.log(`signed ${manifestPath}`);