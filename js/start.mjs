#!/usr/bin/env node
/**
 * nestly launcher.
 *
 * Resolves a signed GitHub release, verifies every byte with SHA-256, installs
 * it atomically under /home/container/nestlybins, starts the supervisor, and
 * streams its logs to the container console. Zero npm dependencies.
 */

import fs from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

import { loadConfig, releaseTarget } from './lib/config.mjs';
import {
  GitHubError,
  downloadAsset,
  resolveRelease,
  validateManifest,
} from './lib/github.mjs';
import {
  currentSupervisor,
  installVersion,
  readCurrent,
  verifyInstalled,
} from './lib/install.mjs';
import { acquireLock } from './lib/lock.mjs';
import { SupervisorProcess } from './lib/process.mjs';
import { verifyManifestSignature } from './lib/verify.mjs';

const DOWNLOAD_CONCURRENCY = 4;

function downloadOptions(config) {
  return {
    maxBytes: config.maxDownloadBytes,
    timeoutMs: config.httpTimeoutMs,
    token: config.githubToken,
  };
}

async function requireAsset(release, ...names) {
  const asset = release.assets.find((candidate) => names.includes(candidate.name));
  if (!asset) {
    throw new GitHubError(`release ${release.tag} has no ${names.join(' or ')} asset`);
  }
  return asset;
}

async function downloadAll(downloads, config) {
  const results = new Map();
  let next = 0;
  const workers = Array.from(
    { length: Math.min(DOWNLOAD_CONCURRENCY, downloads.length) },
    async () => {
      while (true) {
        const index = next;
        next += 1;
        if (index >= downloads.length) {
          return;
        }
        const { asset, url } = downloads[index];
        const bytes = await downloadAsset(url, downloadOptions(config));
        results.set(asset.file, bytes);
      }
    },
  );
  await Promise.all(workers);
  return results;
}

async function verifiedInstalled(config) {
  const version = await readCurrent(config.installDir).catch(() => null);
  if (!version) {
    return null;
  }
  return (await verifyInstalled({ installDir: config.installDir, version }))
    ? version
    : null;
}

async function prepareVersion(config, log) {
  if (config.skipUpdate) {
    const version = await verifiedInstalled(config);
    if (!version) {
      throw new Error(
        'NESTLY_SKIP_UPDATE is set but no verified installed version exists',
      );
    }
    log(`launcher: using the installed version ${version} (updates disabled)`);
    return version;
  }

  const target = releaseTarget();
  try {
    const release = await resolveRelease(config);
    const manifestAsset = await requireAsset(
      release,
      'nestly-manifest.json',
      'rsbot-manifest.json',
    );
    const manifestBytes = await downloadAsset(
      manifestAsset.url,
      downloadOptions(config),
    );

    if (config.pubkey) {
      const signatureAsset = await requireAsset(release, `${manifestAsset.name}.sig`);
      const signature = await downloadAsset(
        signatureAsset.url,
        downloadOptions(config),
      );
      if (!verifyManifestSignature(manifestBytes, signature, config.pubkey)) {
        throw new Error('release manifest signature verification failed');
      }
      log('launcher: release manifest signature verified');
    } else {
      log(
        'launcher: WARN NESTLY_PUBKEY is unset; the manifest is not signature-verified (SHA-256 is still enforced)',
      );
    }

    let manifest;
    try {
      manifest = JSON.parse(manifestBytes.toString('utf8'));
    } catch {
      throw new Error('the release manifest is not valid JSON');
    }
    const selected = validateManifest(manifest, {
      target,
      maxBytes: config.maxDownloadBytes,
    });

    const installed = await verifiedInstalled(config);
    if (installed === selected.version) {
      log(`launcher: version ${selected.version} is installed and verified`);
      return selected.version;
    }

    const downloads = selected.assets.map((asset) => {
      const releaseAsset = release.assets.find(
        (candidate) => candidate.name === asset.file,
      );
      if (!releaseAsset) {
        throw new GitHubError(
          `release ${release.tag} has no ${asset.file} asset`,
        );
      }
      return { asset, url: releaseAsset.url };
    });
    const contents = await downloadAll(downloads, config);

    await installVersion({
      installDir: config.installDir,
      version: selected.version,
      assets: selected.assets,
      fetchAsset: async (file) => {
        const bytes = contents.get(file);
        if (!bytes) {
          throw new Error(`missing downloaded asset ${file}`);
        }
        return bytes;
      },
    });
    log(`launcher: installed version ${selected.version} from ${release.tag}`);
    return selected.version;
  } catch (source) {
    const fallback = await verifiedInstalled(config);
    if (fallback) {
      log(
        `launcher: WARN update failed (${source.message}); starting the verified version ${fallback}`,
      );
      return fallback;
    }
    throw source;
  }
}

/**
 * Runs the launcher.
 *
 * @param {Record<string, string|undefined>} env
 * @param {{handleSignals?: boolean, log?: (line: string) => void, error?: (line: string) => void}} options
 * @returns {Promise<{code: number}>}
 */
export async function main(env = process.env, options = {}) {
  const log = options.log ?? ((line) => process.stdout.write(`${line}\n`));
  const error = options.error ?? ((line) => process.stderr.write(`${line}\n`));
  let lock = null;
  let supervisor = null;
  let stopping = false;
  const handlers = [];

  try {
    const config = loadConfig(env);
    await fs.mkdir(config.stateDir, { recursive: true, mode: 0o700 });
    await fs.mkdir(config.logDir, { recursive: true, mode: 0o700 });
    lock = await acquireLock(path.join(config.stateDir, 'launcher.lock'));

    const version = await prepareVersion(config, log);
    const { binary } = await currentSupervisor({
      installDir: config.installDir,
      version,
    });
    if (!(await verifyInstalled({ installDir: config.installDir, version }))) {
      throw new Error('the installed supervisor failed SHA-256 verification before spawn');
    }

    supervisor = new SupervisorProcess({
      binary,
      args: config.configPath ? ['--config', config.configPath] : [],
      env: config.childEnv,
      // The supervisor resolves relative service binaries inside the
      // installed release, so configuration never pins a version path.
      extraEnv: {
        NESTLY_BIN_DIR: path.dirname(binary),
        RSBOT_BIN_DIR: path.dirname(binary),
      },
      cwd: config.stateDir,
      maxRestarts: config.restartBudget,
      write: log,
    });

    if (options.handleSignals !== false) {
      for (const signal of ['SIGTERM', 'SIGINT']) {
        const handler = () => {
          stopping = true;
          if (supervisor) {
            void supervisor.stop(signal);
          }
        };
        process.on(signal, handler);
        handlers.push([signal, handler]);
      }
    }

    supervisor.start();
    const outcome = await supervisor.completion;
    return { code: stopping ? 0 : outcome.code };
  } catch (source) {
    error(`launcher: ${source.message}`);
    return { code: 1 };
  } finally {
    for (const [signal, handler] of handlers) {
      process.off(signal, handler);
    }
    await lock?.release();
  }
}

const invokedDirectly =
  process.argv[1] !== undefined &&
  import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href;

if (invokedDirectly) {
  const { code } = await main();
  process.exitCode = code;
}