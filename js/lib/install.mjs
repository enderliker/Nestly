import { randomBytes } from 'node:crypto';
import fs from 'node:fs/promises';
import path from 'node:path';

import { assertAssetName, SHA256_HEX, verifySha256 } from './verify.mjs';

export class InstallError extends Error {
  constructor(message) {
    super(message);
    this.name = 'InstallError';
  }
}

const VERSION_NAME = /^v?\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
const STALE_TEMP_MS = 60 * 60 * 1_000;

function assertVersion(version) {
  if (typeof version !== 'string' || !VERSION_NAME.test(version)) {
    throw new InstallError(`unsafe version string: ${String(version)}`);
  }
  return version;
}

function safeJoin(base, name) {
  try {
    assertAssetName(name);
  } catch (source) {
    throw new InstallError(source.message);
  }
  const resolvedBase = path.resolve(base);
  const resolved = path.resolve(resolvedBase, name);
  if (!resolved.startsWith(`${resolvedBase}${path.sep}`)) {
    throw new InstallError(`asset escapes its install directory: ${name}`);
  }
  return resolved;
}

async function lstatOrNull(target) {
  try {
    return await fs.lstat(target);
  } catch (source) {
    if (source.code === 'ENOENT') {
      return null;
    }
    throw source;
  }
}

async function syncDirectory(directory) {
  try {
    const handle = await fs.open(directory, 'r');
    try {
      await handle.sync();
    } finally {
      await handle.close();
    }
  } catch {
    // Directory fsync is best effort on filesystems that reject it.
  }
}

async function writeFileSynced(target, bytes, mode) {
  const handle = await fs.open(target, 'wx', mode);
  try {
    await handle.writeFile(bytes);
    await handle.sync();
  } finally {
    await handle.close();
  }
}

async function readMetadata(installDir, version) {
  const versionDir = path.join(installDir, assertVersion(version));
  const raw = await fs.readFile(path.join(versionDir, 'metadata.json'), 'utf8');
  let metadata;
  try {
    metadata = JSON.parse(raw);
  } catch {
    throw new InstallError('installed metadata is not valid JSON');
  }
  if (metadata?.version !== version || !Array.isArray(metadata?.assets)) {
    throw new InstallError('installed metadata does not match the requested version');
  }
  const assets = metadata.assets.map((asset) => {
    safeJoin(versionDir, asset?.name);
    if (typeof asset?.name !== 'string' || typeof asset.sha256 !== 'string') {
      throw new InstallError('installed metadata has a malformed asset');
    }
    if (!SHA256_HEX.test(asset.sha256)) {
      throw new InstallError(`installed metadata has an invalid sha256 for ${asset.file}`);
    }
    return {
      name: asset.name,
      file: typeof asset.file === 'string' ? asset.file : asset.name,
      sha256: asset.sha256.toLowerCase(),
      size: Number(asset.size),
    };
  });
  return { versionDir, metadata: { version, assets } };
}

/** Returns the version pointed to by the `current` file. */
export async function readCurrent(installDir) {
  const raw = await fs.readFile(path.join(installDir, 'current'), 'utf8').catch((source) => {
    throw new InstallError(`cannot read the current version pointer: ${source.message}`);
  });
  const version = raw.trim();
  assertVersion(version);
  const versionDir = path.join(installDir, version);
  const stat = await lstatOrNull(versionDir);
  if (!stat || !stat.isDirectory() || stat.isSymbolicLink()) {
    throw new InstallError(`installed version ${version} is not a real directory`);
  }
  return version;
}

/**
 * Verifies every installed asset against its recorded SHA-256.
 *
 * Any missing, symlinked, oversized, or tampered file makes this return false;
 * it never throws for a corrupt install.
 */
export async function verifyInstalled({ installDir, version }) {
  try {
    const { versionDir, metadata } = await readMetadata(installDir, version);
    if (metadata.assets.length === 0) {
      return false;
    }
    for (const asset of metadata.assets) {
      const target = safeJoin(versionDir, asset.name);
      const stat = await fs.lstat(target);
      if (!stat.isFile() || stat.isSymbolicLink() || stat.size !== asset.size) {
        return false;
      }
      const bytes = await fs.readFile(target);
      if (!verifySha256(bytes, asset.sha256)) {
        return false;
      }
    }
    return true;
  } catch {
    return false;
  }
}

/** Returns the installed supervisor binary after validating its path. */
export async function currentSupervisor({ installDir, version }) {
  const { versionDir, metadata } = await readMetadata(installDir, version);
  const supervisor =
    metadata.assets.find((asset) => asset.name === 'nestly-supervisor') ??
    metadata.assets.find((asset) => asset.name === 'rsbot-supervisor');
  if (!supervisor) {
    throw new InstallError('the installed metadata has no nestly-supervisor asset');
  }
  const binary = safeJoin(versionDir, supervisor.name);
  const stat = await fs.lstat(binary);
  if (!stat.isFile() || stat.isSymbolicLink()) {
    throw new InstallError('the installed supervisor is not a regular file');
  }
  return { binary, file: supervisor.file };
}

async function collectGarbage(installDir, keep) {
  const entries = await fs.readdir(installDir, { withFileTypes: true });
  const keepSet = new Set(keep);
  const versions = [];
  for (const entry of entries) {
    if (!entry.isDirectory() || entry.isSymbolicLink() || !VERSION_NAME.test(entry.name)) {
      continue;
    }
    const stat = await fs.stat(path.join(installDir, entry.name));
    versions.push({ name: entry.name, mtime: stat.mtimeMs });
  }
  const remaining = versions
    .filter((version) => !keepSet.has(version.name))
    .sort((left, right) => right.mtime - left.mtime);
  if (remaining.length > 0) {
    keepSet.add(remaining[0].name);
  }
  for (const version of remaining.slice(1)) {
    await fs.rm(path.join(installDir, version.name), { recursive: true, force: true });
  }

  const cutoff = Date.now() - STALE_TEMP_MS;
  for (const entry of entries) {
    if (!entry.name.startsWith('.tmp-') && !entry.name.startsWith('.broken-')) {
      continue;
    }
    const stat = await fs.lstat(path.join(installDir, entry.name)).catch(() => null);
    if (stat && stat.mtimeMs < cutoff) {
      await fs.rm(path.join(installDir, entry.name), { recursive: true, force: true });
    }
  }
}

/**
 * Installs a verified release atomically.
 *
 * Assets are validated before any download, written into a private temporary
 * directory, fsynced, and renamed into place. The `current` pointer is a
 * regular file replaced by rename, never a symlink.
 */
export async function installVersion({ installDir, version, assets, fetchAsset }) {
  assertVersion(version);
  if (!Array.isArray(assets) || assets.length === 0) {
    throw new InstallError('the manifest has no assets to install');
  }
  const seen = new Set();
  const plan = assets.map((asset) => {
    if (typeof asset?.name !== 'string' || typeof asset.file !== 'string') {
      throw new InstallError('malformed asset entry');
    }
    safeJoin(installDir, asset.name);
    try {
      assertAssetName(asset.file);
    } catch (source) {
      throw new InstallError(source.message);
    }
    if (seen.has(asset.name)) {
      throw new InstallError(`duplicate asset name: ${asset.name}`);
    }
    seen.add(asset.name);
    if (typeof asset.sha256 !== 'string' || !SHA256_HEX.test(asset.sha256)) {
      throw new InstallError(`invalid sha256 for ${asset.name}`);
    }
    if (!Number.isInteger(asset.size) || asset.size < 0) {
      throw new InstallError(`invalid size for ${asset.name}`);
    }
    return { ...asset, sha256: asset.sha256.toLowerCase() };
  });

  await fs.mkdir(installDir, { recursive: true, mode: 0o700 });
  const installStat = await fs.lstat(installDir);
  if (!installStat.isDirectory() || installStat.isSymbolicLink()) {
    throw new InstallError('the install directory must be a real directory');
  }

  const suffix = randomBytes(4).toString('hex');
  const temporary = path.join(installDir, `.tmp-${version}-${process.pid}-${suffix}`);
  await fs.mkdir(temporary, { recursive: true, mode: 0o700 });

  try {
    for (const asset of plan) {
      const bytes = await fetchAsset(asset.file);
      if (!Buffer.isBuffer(bytes)) {
        throw new InstallError(`asset ${asset.file} did not arrive as a Buffer`);
      }
      if (bytes.length !== asset.size) {
        throw new InstallError(
          `size mismatch for ${asset.file}: expected ${asset.size}, got ${bytes.length}`,
        );
      }
      if (!verifySha256(bytes, asset.sha256)) {
        throw new InstallError(`checksum mismatch for ${asset.file}`);
      }
      // The logical name is the local file name; the target-suffixed
      // manifest name is provenance only. This lets configuration refer to
      // bare service names inside a versioned release directory.
      await writeFileSynced(safeJoin(temporary, asset.name), bytes, 0o755);
    }

    const metadata = {
      version,
      assets: plan.map(({ name, file, sha256, size }) => ({ name, file, sha256, size })),
    };
    await writeFileSynced(
      path.join(temporary, 'metadata.json'),
      Buffer.from(`${JSON.stringify(metadata)}\n`),
      0o600,
    );
    await syncDirectory(temporary);

    const final = path.join(installDir, version);
    if (await lstatOrNull(final)) {
      if (await verifyInstalled({ installDir, version })) {
        await fs.rm(temporary, { recursive: true, force: true });
        return { version, dir: final };
      }
      const broken = path.join(installDir, `.broken-${version}-${process.pid}-${suffix}`);
      await fs.rename(final, broken);
      await fs.rename(temporary, final);
      await fs.rm(broken, { recursive: true, force: true });
    } else {
      await fs.rename(temporary, final);
    }
    await syncDirectory(installDir);

    const currentTemporary = path.join(installDir, `.current-${process.pid}-${suffix}.tmp`);
    await writeFileSynced(currentTemporary, Buffer.from(`${version}\n`), 0o600);
    await fs.rename(currentTemporary, path.join(installDir, 'current'));
    await syncDirectory(installDir);

    await collectGarbage(installDir, [version]);
    return { version, dir: final };
  } catch (source) {
    await fs.rm(temporary, { recursive: true, force: true }).catch(() => {});
    throw source;
  }
}