#!/usr/bin/env node
/**
 * Copies release binaries into a dist directory with target-suffixed names and
 * writes rsbot-manifest.json plus SHA256SUMS.
 *
 * Usage:
 *   node scripts/release-manifest.mjs --dir target/release \
 *     --out dist --version 0.1.0 --target x86_64-unknown-linux-musl
 */

import { createHash } from 'node:crypto';
import fs from 'node:fs/promises';
import path from 'node:path';

function parseArgs(argv) {
  const values = {};
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index]?.replace(/^--/, '');
    const value = argv[index + 1];
    if (!key || value === undefined) {
      throw new Error(`invalid argument: ${argv[index]}`);
    }
    values[key] = value;
  }
  return values;
}

const args = parseArgs(process.argv.slice(2));
const sourceDir = path.resolve(args.dir ?? 'target/release');
const outDir = path.resolve(args.out ?? 'dist');
const version = args.version;
const target = args.target ?? 'x86_64-unknown-linux-musl';

if (!version || !/^v?\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) {
  throw new Error('--version must be a semantic version, for example 0.1.0');
}
if (!/^[a-z0-9_]+-[a-z0-9_]+-linux-musl$/.test(target)) {
  throw new Error(`invalid target triple: ${target}`);
}

const BINARY = /^rsbot-(?:supervisor|edge|mod-[a-z0-9-]+)$/;
const entries = await fs.readdir(sourceDir);
const binaries = entries.filter((name) => BINARY.test(name)).sort();
for (const required of ['rsbot-supervisor', 'rsbot-edge']) {
  if (!binaries.includes(required)) {
    throw new Error(`missing built binary: ${required}`);
  }
}

await fs.mkdir(outDir, { recursive: true, mode: 0o755 });
const assets = [];
const sums = [];

for (const name of binaries) {
  const bytes = await fs.readFile(path.join(sourceDir, name));
  const file = `${name}-${target}`;
  await fs.writeFile(path.join(outDir, file), bytes, { mode: 0o755 });
  const sha256 = createHash('sha256').update(bytes).digest('hex');
  assets.push({ name, file, sha256, size: bytes.length });
  sums.push(`${sha256}  ${file}`);
}

const manifest = {
  schema: 1,
  version,
  protocol: 1,
  architectures: {
    [target]: { assets },
  },
};
await fs.writeFile(
  path.join(outDir, 'rsbot-manifest.json'),
  `${JSON.stringify(manifest, null, 2)}\n`,
  { mode: 0o644 },
);
await fs.writeFile(path.join(outDir, 'SHA256SUMS'), `${sums.join('\n')}\n`, {
  mode: 0o644,
});

console.log(`wrote ${assets.length} assets for ${target} to ${outDir}`);