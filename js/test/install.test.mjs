import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import {
  InstallError,
  installVersion,
  readCurrent,
  verifyInstalled,
} from '../lib/install.mjs';
import { sha256Hex } from '../lib/verify.mjs';

async function temporaryDirectory() {
  return fs.mkdtemp(path.join(os.tmpdir(), 'rsbot-install-'));
}

function asset(name, file, bytes) {
  return {
    name,
    file,
    sha256: sha256Hex(Buffer.from(bytes)),
    size: Buffer.byteLength(bytes),
  };
}

async function install(dir, version, assets) {
  const contents = new Map(assets.map((entry) => [entry.file, Buffer.from(`${entry.file}-body`)]));
  for (const entry of assets) {
    entry.sha256 = sha256Hex(contents.get(entry.file));
    entry.size = contents.get(entry.file).length;
  }
  return installVersion({
    installDir: dir,
    version,
    assets,
    fetchAsset: async (file) => {
      const bytes = contents.get(file);
      if (!bytes) throw new Error(`unexpected asset ${file}`);
      return bytes;
    },
  });
}

const ASSETS = [
  asset('rsbot-supervisor', 'rsbot-supervisor-x86_64-unknown-linux-musl', ''),
  asset('rsbot-edge', 'rsbot-edge-x86_64-unknown-linux-musl', ''),
];

test('installs atomically and records a current pointer', async () => {
  const dir = await temporaryDirectory();
  const installed = await install(dir, '0.1.0', ASSETS);
  assert.equal(installed.version, '0.1.0');
  assert.equal(await readCurrent(dir), '0.1.0');

  const versionDir = path.join(dir, '0.1.0');
  const entries = (await fs.readdir(versionDir)).sort();
  assert.deepEqual(entries, [
    'metadata.json',
    'rsbot-edge',
    'rsbot-supervisor',
  ]);
  const metadata = JSON.parse(await fs.readFile(path.join(versionDir, 'metadata.json'), 'utf8'));
  assert.equal(metadata.version, '0.1.0');
  assert.equal(metadata.assets.length, 2);
  assert.equal(metadata.assets[0].name, 'rsbot-supervisor');
  assert.match(metadata.assets[0].file, /^rsbot-supervisor-x86_64-unknown-linux-musl$/);

  assert.equal(await verifyInstalled({ installDir: dir, version: '0.1.0' }), true);
  const stat = await fs.lstat(path.join(dir, 'current'));
  assert.ok(stat.isFile(), 'current must be a regular file, not a symlink');
  assert.equal(stat.mode & 0o777, 0o600);
  const fileStat = await fs.stat(path.join(versionDir, 'rsbot-supervisor'));
  assert.equal(fileStat.mode & 0o777, 0o755);
  const dirStat = await fs.stat(versionDir);
  assert.equal(dirStat.mode & 0o777, 0o700);
  assert.equal(
    (await fs.readdir(dir)).filter((name) => name.startsWith('.tmp-')).length,
    0,
    'temporary directories are cleaned up',
  );
});

test('detects corruption and refuses to verify it', async () => {
  const dir = await temporaryDirectory();
  await install(dir, '0.1.0', ASSETS);
  const binary = path.join(dir, '0.1.0', 'rsbot-supervisor');
  await fs.appendFile(binary, 'tampered');
  assert.equal(await verifyInstalled({ installDir: dir, version: '0.1.0' }), false);
});

test('rejects path traversal before downloading', async () => {
  const dir = await temporaryDirectory();
  let downloaded = false;
  await assert.rejects(
    () =>
      installVersion({
        installDir: dir,
        version: '0.1.0',
        assets: [
          {
            name: 'rsbot-supervisor',
            file: '../escape',
            sha256: 'a'.repeat(64),
            size: 1,
          },
        ],
        fetchAsset: async () => {
          downloaded = true;
          return Buffer.from('x');
        },
      }),
    InstallError,
  );
  assert.equal(downloaded, false);
});

test('rejects an asset whose bytes do not match the manifest hash', async () => {
  const dir = await temporaryDirectory();
  await assert.rejects(
    () =>
      installVersion({
        installDir: dir,
        version: '0.1.0',
        assets: [
          {
            name: 'rsbot-supervisor',
            file: 'rsbot-supervisor-x86_64-unknown-linux-musl',
            sha256: '0'.repeat(64),
            size: 4,
          },
        ],
        fetchAsset: async () => Buffer.from('evil'),
      }),
    /checksum/,
  );
  assert.deepEqual(await fs.readdir(dir), [], 'no partial version is left behind');
});

test('replaces a corrupt install and keeps the previous version', async () => {
  const dir = await temporaryDirectory();
  await install(dir, '0.1.0', ASSETS);
  await install(dir, '0.1.1', ASSETS);
  assert.equal(await readCurrent(dir), '0.1.1');
  const binary = path.join(dir, '0.1.1', 'rsbot-supervisor');
  await fs.writeFile(binary, 'corrupt');
  assert.equal(await verifyInstalled({ installDir: dir, version: '0.1.1' }), false);

  await install(dir, '0.1.1', ASSETS);
  assert.equal(await verifyInstalled({ installDir: dir, version: '0.1.1' }), true);

  await install(dir, '0.1.2', ASSETS);
  const versions = (await fs.readdir(dir)).filter((name) => /^\d+\.\d+\.\d+$/.test(name)).sort();
  assert.deepEqual(versions, ['0.1.1', '0.1.2'], 'only current and previous are retained');
});

test('refuses a version directory that is a symlink', async () => {
  const dir = await temporaryDirectory();
  const elsewhere = await temporaryDirectory();
  await fs.writeFile(path.join(elsewhere, 'payload'), 'x');
  await fs.symlink(elsewhere, path.join(dir, '0.1.0'));
  assert.equal(await verifyInstalled({ installDir: dir, version: '0.1.0' }), false);
  await assert.rejects(() => readCurrent(dir), InstallError);
});

test('rejects unsafe version strings', async () => {
  const dir = await temporaryDirectory();
  for (const version of ['../x', 'main', '1.2', '1.2.3/../../etc']) {
    await assert.rejects(
      () =>
        installVersion({
          installDir: dir,
          version,
          assets: ASSETS,
          fetchAsset: async () => Buffer.from('x'),
        }),
      InstallError,
      version,
    );
  }
});