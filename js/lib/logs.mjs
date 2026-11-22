import { once } from 'node:events';
import { StringDecoder } from 'node:string_decoder';

export const DEFAULT_MAX_LINE_BYTES = 8 * 1024;
const TRUNCATION_MARK = '…';

/**
 * Incremental UTF-8 line decoder with a hard per-line cap.
 *
 * Chunks may split multi-byte characters or lines. An oversized line is
 * truncated once, marked with an ellipsis, and the remainder of that line is
 * discarded until the next newline.
 */
export class LineDecoder {
  constructor({ maxLineBytes = DEFAULT_MAX_LINE_BYTES } = {}) {
    if (!Number.isInteger(maxLineBytes) || maxLineBytes < 1) {
      throw new TypeError('maxLineBytes must be a positive integer');
    }
    this.maxLineBytes = maxLineBytes;
    this.decoder = new StringDecoder('utf8');
    this.partial = '';
    this.truncated = false;
    this.discarding = false;
  }

  push(chunk) {
    if (!(chunk instanceof Uint8Array)) {
      throw new TypeError('LineDecoder.push expects a Buffer or Uint8Array');
    }
    return this.#consume(this.decoder.write(chunk), false);
  }

  flush() {
    return this.#consume(this.decoder.end(), true);
  }

  #append(segment) {
    if (this.discarding || segment.length === 0) {
      return;
    }
    const room = this.maxLineBytes - this.partial.length;
    if (segment.length <= room) {
      this.partial += segment;
      return;
    }
    if (room > 0) {
      this.partial += segment.slice(0, room);
    }
    this.truncated = true;
    this.discarding = true;
  }

  #emit(lines) {
    const line = this.partial.replace(/\r$/, '');
    lines.push(this.truncated ? `${line}${TRUNCATION_MARK}` : line);
    this.partial = '';
    this.truncated = false;
    this.discarding = false;
  }

  #consume(text, final) {
    const lines = [];
    let start = 0;
    while (true) {
      const newline = text.indexOf('\n', start);
      if (newline === -1) {
        this.#append(text.slice(start));
        if (final) {
          if (this.partial.length > 0 || this.truncated) {
            this.#emit(lines);
          }
        }
        return lines;
      }
      if (!this.discarding) {
        this.#append(text.slice(start, newline));
      }
      this.#emit(lines);
      start = newline + 1;
      if (start >= text.length) {
        return lines;
      }
    }
  }
}

function defaultWrite(line) {
  return process.stdout.write(`${line}\n`);
}

async function defaultDrain() {
  await once(process.stdout, 'drain');
}

/**
 * Reads a stream line by line, prefixes each line, and respects backpressure.
 *
 * `write` returns `false` when the consumer is full; the pump then awaits
 * `drain` before reading more.
 */
export async function pumpStream(stream, options = {}) {
  if (!stream || typeof stream[Symbol.asyncIterator] !== 'function') {
    throw new TypeError('pumpStream expects a readable stream');
  }
  const {
    prefix = '',
    write = defaultWrite,
    drain = defaultDrain,
    signal,
  } = options;
  const decoder = new LineDecoder(options.decoder);
  const emit = async (line) => {
    const formatted = `${prefix}${line}`;
    if (write(formatted) === false && typeof drain === 'function') {
      await drain();
    }
  };

  for await (const chunk of stream) {
    if (signal?.aborted) {
      break;
    }
    for (const line of decoder.push(chunk)) {
      await emit(line);
    }
  }
  for (const line of decoder.flush()) {
    await emit(line);
  }
}