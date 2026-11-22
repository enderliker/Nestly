import assert from 'node:assert/strict';
import test from 'node:test';

import {
  ConfigError,
  DEFAULT_INSTALL_DIR,
  DEFAULT_STATE_DIR,
  loadConfig,
} from '../lib/config.mjs';

const TOKEN = 'discord-token-0123456789abcdef';

function env(overrides = {}) {
  return { DISCORD_TOKEN: TOKEN, ...overrides };
}

test('applies the container defaults', () => {
  const config = loadConfig(env());
  assert.equal(config.repo, 'enderliker/rsbot');
  assert.equal(config.release, 'latest');
  assert.equal(config.installDir, DEFAULT_INSTALL_DIR);
  assert.equal(config.stateDir, DEFAULT_STATE_DIR);
  assert.equal(config.logDir, `${DEFAULT_STATE_DIR}/logs`);
  assert.equal(config.httpTimeoutMs, 30_000);
  assert.equal(config.maxDownloadBytes, 64 * 1024 * 1024);
  assert.equal(config.skipUpdate, false);
  assert.equal(config.pinVersion, null);
});

test('requires DISCORD_TOKEN unless explicitly relaxed', () => {
  assert.throws(() => loadConfig({}), ConfigError);
  assert.throws(() => loadConfig({ DISCORD_TOKEN: '   ' }), ConfigError);
  const config = loadConfig({}, { requireToken: false });
  assert.equal(config.childEnv.DISCORD_TOKEN, undefined);
});

test('rejects repositories outside the allowlist', () => {
  assert.throws(
    () => loadConfig(env({ RSBOT_REPO: 'attacker/rsbot' })),
    /allowlist/,
  );
  assert.throws(
    () => loadConfig(env({ RSBOT_REPO: '../../etc' })),
    /allowlist/,
  );
});

test('validates release tags and the pinned version', () => {
  assert.equal(loadConfig(env({ RSBOT_RELEASE: 'v1.2.3' })).release, 'v1.2.3');
  assert.equal(loadConfig(env({ RSBOT_RELEASE: '1.2.3' })).release, '1.2.3');
  for (const bad of ['latest; rm -rf /', '../main', 'v1', 'v1.2.3/../x', '']) {
    assert.throws(() => loadConfig(env({ RSBOT_RELEASE: bad })), ConfigError, bad);
  }
  assert.equal(
    loadConfig(env({ RSBOT_PIN_VERSION: 'v0.1.0' })).pinVersion,
    'v0.1.0',
  );
  assert.throws(() => loadConfig(env({ RSBOT_PIN_VERSION: 'main' })), ConfigError);
});

test('requires absolute install and state directories', () => {
  assert.throws(
    () => loadConfig(env({ RSBOT_INSTALL_DIR: 'rsbotbins' })),
    /absolute/,
  );
  assert.throws(
    () => loadConfig(env({ RSBOT_STATE_DIR: './state' })),
    /absolute/,
  );
  assert.throws(
    () => loadConfig(env({ RSBOT_INSTALL_DIR: '/tmp/rsbot\u0000' })),
    ConfigError,
  );
});

test('bounds numeric settings', () => {
  assert.equal(
    loadConfig(env({ RSBOT_HTTP_TIMEOUT_MS: '1500' })).httpTimeoutMs,
    1500,
  );
  assert.throws(
    () => loadConfig(env({ RSBOT_HTTP_TIMEOUT_MS: '5' })),
    ConfigError,
  );
  assert.throws(
    () => loadConfig(env({ RSBOT_MAX_DOWNLOAD_BYTES: '0' })),
    ConfigError,
  );
  assert.throws(
    () => loadConfig(env({ RSBOT_MAX_DOWNLOAD_BYTES: '999999999999' })),
    ConfigError,
  );
});

test('builds a minimal child environment allowlist', () => {
  const config = loadConfig(
    env({
      DISCORD_TOKEN: TOKEN,
      DISCORD_TOKEN_MUSIC: 'module-token-0123456789',
      RSBOT_OPS_TOKEN: 'ops-token-0123456789',
      RSBOT_ALERT_CHANNEL: '123456789012345678',
      RSBOT_CONFIG: '/home/container/rsbot.toml',
      RUST_LOG: 'info',
      TZ: 'UTC',
      PATH: '/usr/bin',
      SECRET_OTHER: 'must-not-leak',
      AWS_SECRET_ACCESS_KEY: 'must-not-leak',
    }),
  );
  assert.deepEqual(Object.keys(config.childEnv).sort(), [
    'DISCORD_TOKEN',
    'DISCORD_TOKEN_MUSIC',
    'RSBOT_ALERT_CHANNEL',
    'RSBOT_CONFIG',
    'RSBOT_OPS_TOKEN',
    'RUST_LOG',
    'TZ',
  ]);
  assert.equal(config.childEnv.SECRET_OTHER, undefined);
  assert.equal(config.childEnv.AWS_SECRET_ACCESS_KEY, undefined);
  assert.equal(config.childEnv.PATH, undefined);
});

test('does not expose the GitHub token to the supervisor', () => {
  const config = loadConfig(
    env({ RSBOT_GITHUB_TOKEN: 'ghp_012345678901234567890123456789012345' }),
  );
  assert.equal(config.githubToken, 'ghp_012345678901234567890123456789012345');
  assert.equal(config.childEnv.RSBOT_GITHUB_TOKEN, undefined);
});