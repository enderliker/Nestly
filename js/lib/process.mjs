import { spawn as nodeSpawn } from 'node:child_process';
import { EventEmitter, once } from 'node:events';
import { setTimeout as sleep } from 'node:timers/promises';

import { pumpStream } from './logs.mjs';

const SAFE_ENV = new Set([
  'DISCORD_TOKEN',
  'RSBOT_OPS_TOKEN',
  'RSBOT_ALERT_CHANNEL',
  'RSBOT_CONFIG',
  'RUST_LOG',
  'TZ',
]);
const MODULE_TOKEN = /^DISCORD_TOKEN_[A-Z0-9_]+$/;

/**
 * Filters an environment down to the launcher's allowlist.
 *
 * Secrets unrelated to Discord never reach the supervisor, even if they were
 * present in the container environment.
 */
export function sanitizeChildEnv(env = {}) {
  const filtered = {};
  for (const [key, value] of Object.entries(env)) {
    if (SAFE_ENV.has(key) || MODULE_TOKEN.test(key)) {
      filtered[key] = value;
    }
  }
  return filtered;
}

/**
 * Supervises one child process with a bounded restart budget.
 *
 * Events: `spawn`, `line`, `exit`, `error`.
 * `completion` resolves once the process stops for good, either because
 * `stop()` was requested or the restart budget was exhausted.
 */
export class SupervisorProcess extends EventEmitter {
  static defaultSpawn = nodeSpawn;

  constructor(options = {}) {
    super();
    const {
      binary,
      args = [],
      env = {},
      extraEnv = {},
      cwd = undefined,
      maxRestarts = 3,
      backoff = [100, 250, 500],
      stopTimeoutMs = 5_000,
      prefix = '[supervisor] ',
      write,
      drain,
      spawnImpl = SupervisorProcess.defaultSpawn,
    } = options;
    if (typeof binary !== 'string' || binary.length === 0) {
      throw new TypeError('a supervisor binary path is required');
    }
    if (!Number.isInteger(maxRestarts) || maxRestarts < 0) {
      throw new TypeError('maxRestarts must be a non-negative integer');
    }
    this.binary = binary;
    this.args = args;
    this.cwd = cwd;
    this.env = { ...sanitizeChildEnv(env), ...extraEnv };
    this.maxRestarts = maxRestarts;
    this.backoff = backoff.length > 0 ? backoff : [100];
    this.stopTimeoutMs = stopTimeoutMs;
    this.prefix = prefix;
    this.spawnImpl = spawnImpl;
    this.write = write ?? ((line) => process.stdout.write(`${line}\n`));
    this.drain =
      drain ??
      (async () => {
        await once(process.stdout, 'drain');
      });

    this.child = null;
    this.pumps = [];
    this.recent = [];
    this.waiters = [];
    this.restarts = 0;
    this.stopping = false;
    this.started = false;
    this.settled = false;
    this.completion = new Promise((resolve) => {
      this.resolveCompletion = resolve;
    });
  }

  start() {
    if (this.started) {
      return;
    }
    this.started = true;
    this.#launch();
  }

  #emitLine(line) {
    this.recent.push(line);
    if (this.recent.length > 512) {
      this.recent.shift();
    }
    for (const waiter of [...this.waiters]) {
      if (line.includes(waiter.fragment)) {
        waiter.resolve(line);
        this.waiters.splice(this.waiters.indexOf(waiter), 1);
      }
    }
    this.emit('line', line);
  }

  #log(message) {
    const line = `${this.prefix}${message}`;
    this.#emitLine(line);
    this.write(line);
  }

  waitForLine(fragment, timeoutMs = 5_000) {
    const existing = this.recent.find((line) => line.includes(fragment));
    if (existing !== undefined) {
      return Promise.resolve(existing);
    }
    return new Promise((resolve, reject) => {
      const waiter = { fragment, resolve, reject, timer: null };
      waiter.timer = setTimeout(() => {
        const index = this.waiters.indexOf(waiter);
        if (index !== -1) {
          this.waiters.splice(index, 1);
        }
        reject(new Error(`timed out waiting for a line containing ${fragment}`));
      }, timeoutMs);
      const originalResolve = waiter.resolve;
      waiter.resolve = (line) => {
        clearTimeout(waiter.timer);
        originalResolve(line);
      };
      this.waiters.push(waiter);
    });
  }

  #launch() {
    let child;
    try {
      child = this.spawnImpl(this.binary, this.args, {
        env: this.env,
        cwd: this.cwd,
        stdio: ['ignore', 'pipe', 'pipe'],
        detached: false,
        shell: false,
      });
    } catch (source) {
      this.emit('error', source);
      this.#finish({ code: 1, restarts: this.restarts });
      return;
    }
    this.child = child;
    this.emit('spawn', child.pid);

    const pump = async (stream) => {
      if (!stream) {
        return;
      }
      await pumpStream(stream, {
        prefix: this.prefix,
        write: (line) => {
          this.#emitLine(line);
          return this.write(line);
        },
        drain: this.drain,
      });
    };
    this.pumps = [pump(child.stdout), pump(child.stderr)];

    child.once('error', (source) => {
      this.emit('error', source);
    });
    child.once('exit', (code, signal) => {
      void this.#onExit(code, signal);
    });
  }

  async #onExit(code, signal) {
    this.child = null;
    await Promise.allSettled(this.pumps);

    if (this.stopping) {
      this.#log(`supervisor stopped (${signal ?? `code ${code ?? 0}`})`);
      this.#finish({ code: code ?? 0, restarts: this.restarts });
      return;
    }

    const description = signal ? `signal ${signal}` : `code ${code ?? 0}`;
    this.#log(`supervisor exited with ${description}`);

    if (this.restarts >= this.maxRestarts) {
      this.#log(`restart budget exhausted after ${this.restarts} restarts; giving up`);
      this.#finish({ code: code ?? 1, restarts: this.restarts });
      return;
    }

    this.restarts += 1;
    const delay = this.backoff[Math.min(this.restarts - 1, this.backoff.length - 1)];
    this.#log(`restarting supervisor (attempt ${this.restarts}/${this.maxRestarts}) in ${delay} ms`);
    await sleep(delay);
    if (this.stopping) {
      this.#finish({ code: code ?? 0, restarts: this.restarts });
      return;
    }
    this.#launch();
  }

  #finish(outcome) {
    if (this.settled) {
      return;
    }
    this.settled = true;
    this.resolveCompletion(outcome);
  }

  /**
   * Sends a signal, escalates to SIGKILL after the grace period, and waits for
   * the process to exit. A stopped process is never restarted.
   */
  async stop(signal = 'SIGTERM') {
    if (this.settled) {
      return this.completion;
    }
    this.stopping = true;
    const child = this.child;
    if (!child) {
      this.#finish({ code: 0, restarts: this.restarts });
      return this.completion;
    }

    let timer;
    const escalation = new Promise((resolve) => {
      timer = setTimeout(() => {
        this.#log(`supervisor did not stop within ${this.stopTimeoutMs} ms; sending SIGKILL`);
        try {
          child.kill('SIGKILL');
        } catch {
          // Already gone.
        }
        resolve();
      }, this.stopTimeoutMs);
    });
    try {
      child.kill(signal);
    } catch {
      // Already gone; the exit handler still runs.
    }
    await Promise.race([this.completion, escalation]);
    clearTimeout(timer);
    await this.completion;
    return this.completion;
  }
}