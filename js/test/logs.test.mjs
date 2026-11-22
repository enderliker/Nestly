import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { LineDecoder, pumpStream } from '../lib/logs.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const logger = path.join(here, 'fixtures', 'logger.mjs');

test('decodes complete lines and keeps partials', () => {
  const decoder = new LineDecoder();
  assert.deepEqual(decoder.push(Buffer.from('one\r\ntwo\nthr')), ['one', 'two']);
  assert.deepEqual(decoder.push(Buffer.from('ee\n')), ['three']);
  assert.deepEqual(decoder.flush(), []);
});

test('preserves multi-byte characters split across chunks', () => {
  const decoder = new LineDecoder();
  const bytes = Buffer.from('héllo\n');
  const first = decoder.push(bytes.subarray(0, 2));
  const second = decoder.push(bytes.subarray(2));
  assert.deepEqual(first, []);
  assert.deepEqual(second, ['héllo']);
});

test('replaces invalid UTF-8 instead of throwing', () => {
  const decoder = new LineDecoder();
  const lines = decoder.push(Buffer.from([0xff, 0x0a]));
  assert.equal(lines.length, 1);
  assert.equal(lines[0], '�');
});

test('caps a line at the configured byte limit', () => {
  const decoder = new LineDecoder({ maxLineBytes: 4 });
  const lines = decoder.push(Buffer.from('abcdefghij\nok\n'));
  assert.equal(lines.length, 2);
  assert.ok(lines[0].startsWith('abcd'), lines[0]);
  assert.ok(lines[0].endsWith('…'), lines[0]);
  assert.equal(lines[1], 'ok');
});

test('flushes a final line without a newline', () => {
  const decoder = new LineDecoder();
  assert.deepEqual(decoder.push(Buffer.from('tail')), []);
  assert.deepEqual(decoder.flush(), ['tail']);
});

test('multiplexes two children writing simultaneously without loss', async () => {
  const count = 10_000;
  const children = ['alpha', 'beta'].map((id) =>
    spawn(process.execPath, [logger], {
      env: { ...process.env, LOGGER_ID: id, LOGGER_COUNT: String(count) },
      stdio: ['ignore', 'pipe', 'pipe'],
    }),
  );
  const seen = new Map();
  const tasks = children.map(async (child, index) => {
    const id = index === 0 ? 'alpha' : 'beta';
    seen.set(id, []);
    const exited = new Promise((resolve) => child.once('exit', resolve));
    await pumpStream(child.stdout, {
      prefix: '[supervisor] ',
      write: (line) => {
        assert.ok(line.startsWith('[supervisor] '), line);
        seen.get(id).push(line);
        return true;
      },
    });
    await exited;
  });
  await Promise.all(tasks);

  for (const id of ['alpha', 'beta']) {
    const lines = seen.get(id);
    assert.equal(lines.length, count, `${id} line count`);
    const numbers = new Set(
      lines.map((line) => Number(line.slice('[supervisor] '.length + id.length + 1))),
    );
    assert.equal(numbers.size, count, `${id} unique line numbers`);
    for (const line of lines) {
      assert.match(line, /^\[supervisor\] (alpha|beta):\d+$/);
    }
  }
});

test('waits for drain before continuing', async () => {
  const child = spawn(process.execPath, [logger], {
    env: { ...process.env, LOGGER_ID: 'slow', LOGGER_COUNT: '200' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let lines = 0;
  let drains = 0;
  const started = Date.now();
  const exited = new Promise((resolve) => child.once('exit', resolve));
  await pumpStream(child.stdout, {
    prefix: '',
    write: () => {
      lines += 1;
      return lines % 10 !== 0;
    },
    drain: async () => {
      drains += 1;
      await new Promise((resolve) => setTimeout(resolve, 2));
    },
  });
  await exited;
  assert.equal(lines, 200);
  assert.equal(drains, 20);
  assert.ok(Date.now() - started >= 30, 'drain was awaited');
});