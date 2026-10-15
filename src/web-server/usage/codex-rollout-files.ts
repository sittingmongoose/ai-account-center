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

/** Backpressure bounds both compressed input and decoded chunks. Never materialize a rollout. */
export async function* codexZstdChunks(file: string): AsyncGenerator<Buffer> {
  // The minimum production runtime is Node 22.22; the repository still uses Node 20 typings.
  const create = (zlib as unknown as { createZstdDecompress: () => Transform })
    .createZstdDecompress;
  if (!create) throw new CCSError('Codex compressed logs need zstd support in Node');
  const input = fs.createReadStream(file, { highWaterMark: 64 * 1024 });
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
}
