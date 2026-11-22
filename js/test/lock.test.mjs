import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { LockError, acquireLock } from '../lib/lock.mjs';

async function temporaryDirectory() {
  return fs.mkdtemp(path.join(os.tmpdir(), 'rsbot-lock-'));
}

test('acquires, blocks a second launcher, and releases', async () => {
  const dir = await temporaryDirectory();
  const lockPath = path.join(dir, 'launcher.lock');
  const lock = await acquireLock(lockPath);
  assert.equal((await fs.readFile(lockPath, 'utf8')).trim(), String(process.pid));
  await assert.rejects(() => acquireLock(lockPath), LockError);
  await lock.release();
  await assert.rejects(() => fs.stat(lockPath), /ENOENT/);
  const again = await acquireLock(lockPath);
  await again.release();
});

test('replaces a lock left by a dead process', async () => {
  const dir = await temporaryDirectory();
  const lockPath = path.join(dir, 'launcher.lock');
  await fs.writeFile(lockPath, '999999999\n');
  const lock = await acquireLock(lockPath);
  assert.equal((await fs.readFile(lockPath, 'utf8')).trim(), String(process.pid));
  await lock.release();
});

test('does not delete a lock another process replaced', async () => {
  const dir = await temporaryDirectory();
  const lockPath = path.join(dir, 'launcher.lock');
  const lock = await acquireLock(lockPath);
  await fs.writeFile(lockPath, '42424242\n');
  await lock.release();
  assert.equal((await fs.readFile(lockPath, 'utf8')).trim(), '42424242');
});