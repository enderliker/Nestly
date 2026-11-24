import path from 'node:path';

export class ConfigError extends Error {
  constructor(message) {
    super(message);
    this.name = 'ConfigError';
  }
}

export const DEFAULT_REPO = 'enderliker/nestly';
export const DEFAULT_INSTALL_DIR = '/home/container/nestlybins';
export const DEFAULT_STATE_DIR = '/home/container/nestly-state';

/** Repositories this launcher is allowed to download from. */
export const ALLOWED_REPOS = new Set([DEFAULT_REPO, 'enderliker/rsbot']);

const RELEASE_TAG = /^v?\d+\.\d+\.\d+(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$/;
const MODULE_TOKEN = /^DISCORD_TOKEN_[A-Z0-9_]+$/;
const PASSTHROUGH = new Set([
  'NESTLY_OPS_TOKEN',
  'NESTLY_ALERT_CHANNEL',
  'NESTLY_CONFIG',
  'RSBOT_OPS_TOKEN',
  'RSBOT_ALERT_CHANNEL',
  'RSBOT_CONFIG',
  'RUST_LOG',
  'TZ',
]);

const DEFAULTS = {
  httpTimeoutMs: 30_000,
  maxDownloadBytes: 64 * 1024 * 1024,
  restartBudget: 3,
};

function requireAbsolute(value, name) {
  if (typeof value !== 'string' || value.length === 0 || value.includes('\u0000')) {
    throw new ConfigError(`${name} must be a non-empty absolute path`);
  }
  if (!path.isAbsolute(value)) {
    throw new ConfigError(`${name} must be an absolute path, got ${value}`);
  }
  return path.normalize(value);
}

function integer(value, name, { min, max, fallback }) {
  if (value === undefined || value === '') {
    return fallback;
  }
  if (!/^\d+$/.test(String(value))) {
    throw new ConfigError(`${name} must be an integer, got ${value}`);
  }
  const parsed = Number(value);
  if (parsed < min || parsed > max) {
    throw new ConfigError(`${name} must be between ${min} and ${max}, got ${parsed}`);
  }
  return parsed;
}

function releaseTag(value, name) {
  const tag = value.trim();
  if (!RELEASE_TAG.test(tag)) {
    throw new ConfigError(`${name} is not a valid release tag: ${value}`);
  }
  return tag;
}

/**
 * Validates the launcher environment.
 *
 * Paths must be absolute, the repository must be allowlisted, numeric
 * settings are bounded, and the child environment is an explicit allowlist so
 * unrelated credentials never reach the supervisor.
 */
export function loadConfig(env = process.env, options = {}) {
  const { requireToken = true } = options;

  const repo = (env.NESTLY_REPO ?? env.RSBOT_REPO ?? DEFAULT_REPO).trim();
  if (!ALLOWED_REPOS.has(repo)) {
    throw new ConfigError(
      `repository ${repo} is not in the launcher allowlist (${[...ALLOWED_REPOS].join(', ')})`,
    );
  }

  const releaseRaw = (env.NESTLY_RELEASE ?? env.RSBOT_RELEASE ?? 'latest').trim();
  const release =
    releaseRaw === 'latest'
      ? 'latest'
      : releaseTag(releaseRaw, env.NESTLY_RELEASE ? 'NESTLY_RELEASE' : 'RSBOT_RELEASE');

  const rawPin = env.NESTLY_PIN_VERSION ?? env.RSBOT_PIN_VERSION;
  const pinVersion = rawPin
    ? releaseTag(rawPin, env.NESTLY_PIN_VERSION ? 'NESTLY_PIN_VERSION' : 'RSBOT_PIN_VERSION')
    : null;

  const installDir = requireAbsolute(
    env.NESTLY_INSTALL_DIR ?? env.RSBOT_INSTALL_DIR ?? DEFAULT_INSTALL_DIR,
    env.NESTLY_INSTALL_DIR ? 'NESTLY_INSTALL_DIR' : (env.RSBOT_INSTALL_DIR ? 'RSBOT_INSTALL_DIR' : 'NESTLY_INSTALL_DIR'),
  );
  const stateDir = requireAbsolute(
    env.NESTLY_STATE_DIR ?? env.RSBOT_STATE_DIR ?? DEFAULT_STATE_DIR,
    env.NESTLY_STATE_DIR ? 'NESTLY_STATE_DIR' : (env.RSBOT_STATE_DIR ? 'RSBOT_STATE_DIR' : 'NESTLY_STATE_DIR'),
  );
  const logDir = requireAbsolute(
    env.NESTLY_LOG_DIR ?? env.RSBOT_LOG_DIR ?? path.join(stateDir, 'logs'),
    env.NESTLY_LOG_DIR ? 'NESTLY_LOG_DIR' : (env.RSBOT_LOG_DIR ? 'RSBOT_LOG_DIR' : 'NESTLY_LOG_DIR'),
  );

  const token = env.DISCORD_TOKEN;
  if (requireToken && (typeof token !== 'string' || token.trim().length === 0)) {
    throw new ConfigError('DISCORD_TOKEN is required');
  }

  const childEnv = {};
  if (typeof token === 'string' && token.length > 0) {
    childEnv.DISCORD_TOKEN = token;
  }
  for (const [key, value] of Object.entries(env)) {
    if (MODULE_TOKEN.test(key) && typeof value === 'string' && value.length > 0) {
      childEnv[key] = value;
    }
  }
  for (const key of PASSTHROUGH) {
    if (env[key] !== undefined) {
      childEnv[key] = env[key];
    }
  }

  const configPathRaw = env.NESTLY_CONFIG ?? env.RSBOT_CONFIG;
  const configPath = configPathRaw
    ? requireAbsolute(configPathRaw, env.NESTLY_CONFIG ? 'NESTLY_CONFIG' : 'RSBOT_CONFIG')
    : null;

  return {
    repo,
    release,
    pinVersion,
    installDir,
    stateDir,
    logDir,
    githubToken: env.NESTLY_GITHUB_TOKEN ?? env.RSBOT_GITHUB_TOKEN ?? null,
    pubkey: env.NESTLY_PUBKEY ?? env.RSBOT_PUBKEY ?? null,
    configPath,
    httpTimeoutMs: integer(
      env.NESTLY_HTTP_TIMEOUT_MS ?? env.RSBOT_HTTP_TIMEOUT_MS,
      env.NESTLY_HTTP_TIMEOUT_MS ? 'NESTLY_HTTP_TIMEOUT_MS' : 'RSBOT_HTTP_TIMEOUT_MS',
      {
        min: 1_000,
        max: 300_000,
        fallback: DEFAULTS.httpTimeoutMs,
      },
    ),
    maxDownloadBytes: integer(
      env.NESTLY_MAX_DOWNLOAD_BYTES ?? env.RSBOT_MAX_DOWNLOAD_BYTES,
      env.NESTLY_MAX_DOWNLOAD_BYTES ? 'NESTLY_MAX_DOWNLOAD_BYTES' : 'RSBOT_MAX_DOWNLOAD_BYTES',
      {
        min: 1,
        max: 512 * 1024 * 1024,
        fallback: DEFAULTS.maxDownloadBytes,
      },
    ),
    restartBudget: integer(
      env.NESTLY_RESTART_BUDGET ?? env.RSBOT_RESTART_BUDGET,
      env.NESTLY_RESTART_BUDGET ? 'NESTLY_RESTART_BUDGET' : 'RSBOT_RESTART_BUDGET',
      {
        min: 0,
        max: 10,
        fallback: DEFAULTS.restartBudget,
      },
    ),
    skipUpdate:
      (env.NESTLY_SKIP_UPDATE ?? env.RSBOT_SKIP_UPDATE) === '1' ||
      (env.NESTLY_SKIP_UPDATE ?? env.RSBOT_SKIP_UPDATE) === 'true',
    childEnv,
  };
}

/** Maps the current Node architecture to a release target triple. */
export function releaseTarget(arch = process.arch) {
  switch (arch) {
    case 'x64':
      return 'x86_64-unknown-linux-musl';
    case 'arm64':
      return 'aarch64-unknown-linux-musl';
    default:
      throw new ConfigError(`unsupported architecture: ${arch}`);
  }
}