import assert from 'node:assert/strict';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { SupervisorProcess } from '../lib/process.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const child = path.join(here, 'fixtures', 'child.mjs');

function makeProcess({ mode, code = '0', maxRestarts = 3, stopTimeoutMs = 500, env = {}, spawnImpl }) {
  const lines = [];
  const proc = new SupervisorProcess({
    binary: process.execPath,
    args: [child],
    env: { RUST_LOG: 'info', ...env },
    extraEnv: { MODE: mode, CODE: code },
    maxRestarts,
    backoff: [10, 10, 10],
    stopTimeoutMs,
    spawnImpl,
    write: (line) => {
      lines.push(line);
      return true;
    },
  });
  return { proc, lines };
}

test('restarts a failing supervisor and gives up after the budget', async () => {
  let spawns = 0;
  const { proc, lines } = makeProcess({
    mode: 'exit',
    code: '7',
    spawnImpl: (...args) => {
      spawns += 1;
      return SupervisorProcess.defaultSpawn(...args);
    },
  });
  proc.start();
  const outcome = await proc.completion;
  assert.equal(outcome.code, 7);
  assert.equal(outcome.restarts, 3);
  assert.equal(spawns, 4);
  assert.ok(lines.some((line) => line.includes('exited with code 7')), lines.join('\n'));
});

test('does not restart after a graceful stop', async () => {
  const { proc, lines } = makeProcess({ mode: 'serve' });
  proc.start();
  await proc.waitForLine('serve-ready');
  await proc.stop('SIGTERM');
  const outcome = await proc.completion;
  assert.equal(outcome.restarts, 0);
  assert.equal(outcome.code, 0);
  assert.ok(lines.some((line) => line.includes('serve-ready')));
});

test('escalates to SIGKILL when the child ignores SIGTERM', async () => {
  const { proc } = makeProcess({ mode: 'stubborn', stopTimeoutMs: 150 });
  proc.start();
  await proc.waitForLine('stubborn-ready');
  const started = Date.now();
  await proc.stop('SIGTERM');
  const outcome = await proc.completion;
  assert.ok(Date.now() - started < 2_000, 'stop completed');
  assert.equal(outcome.restarts, 0);
});

test('does not leak unrelated environment variables', async () => {
  const { proc } = makeProcess({
    mode: 'echo',
    env: { SECRET_OTHER: 'must-not-appear', RUST_LOG: 'expected-value' },
  });
  proc.start();
  const line = await proc.waitForLine('env:');
  await proc.stop('SIGTERM');
  assert.ok(line.includes('RUST_LOG=expected-value'), line);
  assert.ok(!line.includes('must-not-appear'), line);
});

test('prefixes every line', async () => {
  const { proc, lines } = makeProcess({ mode: 'serve' });
  proc.start();
  await proc.waitForLine('serve-ready');
  await proc.stop('SIGTERM');
  await proc.completion;
  assert.ok(lines.length > 0);
  for (const line of lines) {
    assert.ok(line.startsWith('[supervisor] '), line);
  }
});