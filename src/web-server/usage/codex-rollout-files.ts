import * as fs from 'fs';
import * as path from 'path';
import * as zlib from 'zlib';
import type { Transform } from 'stream';
import { CCSError } from '../../errors/error-types';

const SESSION_UUID =
  /([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\.jsonl(?:\.zst)?$/i;

export function codexRolloutKey(file: string): string {
  return SESSION_UUID.exec(path.basename(file))?.[1]?.toLowerCase() ?? file.replace(/\.zst$/, '');
}

/** A growing plain rollout wins over its finished compressed twin, independent of mtime. */
export function preferPlainCodexRollouts<T extends { file: string }>(files: T[]): T[] {
  const plain = new Set(
    files.filter((item) => !item.file.endsWith('.zst')).map((item) => codexRolloutKey(item.file))
  );
  const compressed = new Set<string>();
  return files.filter((item) => {
    if (!item.file.endsWith('.zst')) return true;
    const key = codexRolloutKey(item.file);
    if (plain.has(key) || compressed.has(key)) return false;
    compressed.add(key);
    return true;
  });
}

export interface CodexZstdBoundary {
  compressed: number;
  decoded: number;
}

interface CodexZstdOptions {
  start?: CodexZstdBoundary;
  deadline?: number;
  onDeadline?: () => void;
  onFrame?: (boundary: CodexZstdBoundary) => void;
}

/**
 * Walk framing, not magic-byte matches in payloads. Zstd frames are independent;
 * blocks within a frame are not. Only the decoder confirms a consumed boundary.
 * https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md
 */
function frameEnd(fd: number, start: number, size: number, deadline: number): number | undefined {
  const header = Buffer.alloc(18);
  const read = (position: number, length: number): Buffer => {
    if (position + length > size || fs.readSync(fd, header, 0, length, position) !== length)
      throw new CCSError('Truncated Codex zstd frame');
    return header;
  };
  const magic = read(start, 4).readUInt32LE(0);
  if ((magic & 0xfffffff0) >>> 0 === 0x184d2a50) {
    const end = start + 8 + read(start + 4, 4).readUInt32LE(0);
    if (end > size) throw new CCSError('Truncated Codex zstd metadata');
    return end;
  }
  if (magic !== 0xfd2fb528) throw new CCSError('Invalid Codex zstd frame');
  const descriptor = read(start + 4, 1)[0];
  if (descriptor & 8) throw new CCSError('Invalid Codex zstd frame header');
  const single = (descriptor & 32) !== 0;
  const contentSizeBytes = [single ? 1 : 0, 2, 4, 8][descriptor >>> 6];
  const dictionaryBytes = [0, 1, 2, 4][descriptor & 3];
  let position = start + 5 + (single ? 0 : 1) + dictionaryBytes + contentSizeBytes;
  for (;;) {
    if (Date.now() >= deadline) return undefined;
    const block = read(position, 3).readUIntLE(0, 3);
    const type = (block >>> 1) & 3;
    if (type === 3) throw new CCSError('Invalid Codex zstd block');
    position += 3 + (type === 1 ? 1 : block >>> 3);
    if (block & 1) break;
  }
  position += descriptor & 4 ? 4 : 0;
  if (position > size) throw new CCSError('Truncated Codex zstd block');
  return position;
}

/** Backpressure bounds both compressed input and decoded chunks. Never materialize a rollout. */
export async function* codexZstdChunks(
  file: string,
  options: CodexZstdOptions = {}
): AsyncGenerator<Buffer> {
  // The minimum production runtime is Node 22.22; the repository still uses Node 20 typings.
  const create = (zlib as unknown as { createZstdDecompress: () => Transform })
    .createZstdDecompress;
  if (!create) throw new CCSError('Codex compressed logs need zstd support in Node');
  const decode = async function* (start: number, end?: number): AsyncGenerator<Buffer> {
    const input = fs.createReadStream(file, { start, end, highWaterMark: 64 * 1024 });
    const decoder = create();
    const onError = (error: Error): void => {
      decoder.destroy(error);
    };
    input.on('error', onError);
    input.pipe(decoder);
    try {
      for await (const chunk of decoder) yield chunk as Buffer;
    } finally {
      input.destroy();
      decoder.destroy();
    }
  };
  // Bounded metadata sniffers and non-checkpoint readers need no framing walk.
  if (!options.onFrame && !options.start) {
    yield* decode(0);
    return;
  }
  const fd = fs.openSync(file, 'r');
  try {
    const size = fs.fstatSync(fd).size;
    if (!size) throw new CCSError('Truncated Codex zstd frame');
    let compressed = options.start?.compressed ?? 0;
    let decoded = options.start?.decoded ?? 0;
    while (compressed < size) {
      const end = frameEnd(fd, compressed, size, options.deadline ?? Infinity);
      if (end === undefined || Date.now() >= (options.deadline ?? Infinity)) {
        options.onDeadline?.();
        return;
      }
      for await (const chunk of decode(compressed, end - 1)) {
        decoded += chunk.length;
        yield chunk;
      }
      compressed = end;
      options.onFrame?.({ compressed, decoded });
    }
  } finally {
    fs.closeSync(fd);
  }
}
