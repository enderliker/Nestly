import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { main } from '../start.mjs';
import { sha256Hex } from '../lib/verify.mjs';

const TOKEN = 'discord-token-0123456789abcdef';

async function temporaryDirectory(prefix = 'nestly-start-') {
  return fs.mkdtemp(path.join(os.tmpdir(), prefix));
}

async function installedLauncher({ body = '#!/bin/sh\necho supervisor-boot ok\nexit 0\n', tamper = false } = {}) {
  const installDir = await temporaryDirectory();
  const version = '0.1.0';
  const versionDir = path.join(installDir, version);
  await fs.mkdir(versionDir, { recursive: true, mode: 0o700 });
  const file = 'nestly-supervisor';
  const binary = path.join(versionDir, file);
  await fs.writeFile(binary, body, { mode: 0o755 });
  const bytes = Buffer.from(body);
  const metadata = {
    version,
    assets: [
      {
        name: 'nestly-supervisor',
        file,
        sha256: tamper ? '0'.repeat(64) : sha256Hex(bytes),
        size: bytes.length,
      },
    ],
  };
  await fs.writeFile(path.join(versionDir, 'metadata.json'), JSON.stringify(metadata));
  await fs.writeFile(path.join(installDir, 'current'), `${version}\n`, { mode: 0o600 });
  return { installDir, versionDir, binary };
}

async function runMain(installDir, overrides = {}) {
  const stateDir = await temporaryDirectory();
  const lines = [];
  const errors = [];
  const result = await main(
    {
      DISCORD_TOKEN: TOKEN,
      NESTLY_INSTALL_DIR: installDir,
      NESTLY_STATE_DIR: stateDir,
      NESTLY_SKIP_UPDATE: '1',
      ...overrides,
    },
    {
      handleSignals: false,
      log: (line) => lines.push(line),
      error: (line) => errors.push(line),
    },
  );
  return { result, lines, errors, stateDir };
}

test('verifies the installed binary and streams supervisor logs', async () => {
  const { installDir } = await installedLauncher();
  const { result, lines } = await runMain(installDir, { NESTLY_RESTART_BUDGET: '0' });
  assert.equal(result.code, 0, lines.join('\n'));
  assert.ok(
    lines.some((line) => line === '[supervisor] supervisor-boot ok'),
    lines.join('\n'),
  );
});

test('refuses to start a binary that fails verification', async () => {
  const { installDir } = await installedLauncher({ tamper: true });
  const { result, lines, errors } = await runMain(installDir);
  assert.notEqual(result.code, 0);
  assert.ok(!lines.some((line) => line.includes('supervisor-boot ok')), 'the binary never ran');
  assert.ok(errors.join('\n').match(/checksum|verif/i), errors.join('\n'));
});

test('refuses to start without a current version', async () => {
  const installDir = await temporaryDirectory();
  const { result, errors } = await runMain(installDir);
  assert.notEqual(result.code, 0);
  assert.ok(errors.join('\n').length > 0);
});

test('propagates a supervisor exit code when the restart budget is exhausted', async () => {
  const { installDir } = await installedLauncher({
    body: '#!/bin/sh\nexit 9\n',
  });
  const { result } = await runMain(installDir, { NESTLY_RESTART_BUDGET: '1' });
  assert.equal(result.code, 9);
});

test('rejects the launcher when another instance holds the lock', async () => {
  const { installDir } = await installedLauncher();
  const stateDir = await temporaryDirectory();
  const lockPath = path.join(stateDir, 'launcher.lock');
  await fs.writeFile(lockPath, `${process.pid}\n`);
  const errors = [];
  const result = await main(
    {
      DISCORD_TOKEN: TOKEN,
      NESTLY_INSTALL_DIR: installDir,
      NESTLY_STATE_DIR: stateDir,
      NESTLY_SKIP_UPDATE: '1',
    },
    { handleSignals: false, log: () => {}, error: (line) => errors.push(line) },
  );
  assert.notEqual(result.code, 0);
  assert.match(errors.join('\n'), /already running|lock/i);
});