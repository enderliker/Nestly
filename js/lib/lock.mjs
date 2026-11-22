import fs from 'node:fs/promises';
import path from 'node:path';

export class LockError extends Error {
  constructor(message) {
    super(message);
    this.name = 'LockError';
  }
}

export function processAlive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) {
    return false;
  }
  try {
    process.kill(pid, 0);
    return true;
  } catch (source) {
    // EPERM means the process exists but belongs to another user.
    return source.code === 'EPERM';
  }
}

/**
 * Creates an exclusive launcher lock.
 *
 * A lock owned by a live process is respected. A stale lock (dead pid or
 * unreadable content) is replaced. Release only removes the file when it
 * still contains this process's pid.
 *
 * @returns {Promise<{path: string, pid: number, release: () => Promise<void>}>}
 */
export async function acquireLock(lockPath) {
  await fs.mkdir(path.dirname(lockPath), { recursive: true, mode: 0o700 });

  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      const handle = await fs.open(lockPath, 'wx', 0o600);
      try {
        await handle.writeFile(`${process.pid}\n`);
        await handle.sync();
      } finally {
        await handle.close();
      }
      return {
        path: lockPath,
        pid: process.pid,
        release: async () => {
          const raw = await fs.readFile(lockPath, 'utf8').catch(() => null);
          if (raw !== null && raw.trim() === String(process.pid)) {
            await fs.unlink(lockPath).catch(() => {});
          }
        },
      };
    } catch (source) {
      if (source.code !== 'EEXIST') {
        throw new LockError(`cannot create ${lockPath}: ${source.message}`);
      }
      const raw = await fs.readFile(lockPath, 'utf8').catch(() => '');
      const owner = Number.parseInt(raw.trim(), 10);
      if (Number.isInteger(owner) && owner > 0 && processAlive(owner)) {
        throw new LockError(
          `another launcher is already running (pid ${owner}, lock ${lockPath})`,
        );
      }
      await fs.unlink(lockPath).catch(() => {});
    }
  }

  throw new LockError(`cannot acquire ${lockPath}`);
}