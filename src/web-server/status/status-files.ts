/**
 * Images a card's options show (`GET /api/status/files/<writer>/<path>`): only files inside that
 * writer's own folder of the reports share. A path with `..`, an absolute path or a link that
 * resolves outside the folder is not served. The type comes from the file's first bytes (PNG, JPEG
 * or WebP), never its name, and files over 2 MB are refused.
 */
import fs from 'fs';
import path from 'path';
import { isWriterFilePath, isWriterName } from './report-schema';

export const MAX_CARD_IMAGE_BYTES = 2 * 1024 * 1024;

export type CardImage =
  | { ok: true; contentType: 'image/png' | 'image/jpeg' | 'image/webp'; bytes: Buffer }
  | { ok: false; reason: 'not_found' | 'too_large' | 'not_an_image' };

/** The image type of these bytes, by content. */
export function sniffImage(bytes: Buffer): 'image/png' | 'image/jpeg' | 'image/webp' | null {
  if (
    bytes.length >= 8 &&
    bytes.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))
  )
    return 'image/png';
  if (bytes.length >= 3 && bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff)
    return 'image/jpeg';
  if (
    bytes.length >= 12 &&
    bytes.subarray(0, 4).toString('latin1') === 'RIFF' &&
    bytes.subarray(8, 12).toString('latin1') === 'WEBP'
  )
    return 'image/webp';
  return null;
}

export async function readCardImage(
  root: string,
  writer: string,
  file: string
): Promise<CardImage> {
  if (!isWriterName(writer) || !isWriterFilePath(file)) return { ok: false, reason: 'not_found' };
  try {
    const base = await fs.promises.realpath(path.join(root, 'v1', 'writers', writer));
    const target = await fs.promises.realpath(path.join(base, ...file.split('/')));
    if (!target.startsWith(base + path.sep)) return { ok: false, reason: 'not_found' };
    const handle = await fs.promises.open(target, 'r');
    try {
      const stat = await handle.stat();
      if (!stat.isFile()) return { ok: false, reason: 'not_found' };
      if (stat.size > MAX_CARD_IMAGE_BYTES) return { ok: false, reason: 'too_large' };
      const buffer = Buffer.alloc(MAX_CARD_IMAGE_BYTES + 1);
      const { bytesRead } = await handle.read(buffer, 0, buffer.length, 0);
      if (bytesRead > MAX_CARD_IMAGE_BYTES) return { ok: false, reason: 'too_large' };
      const bytes = buffer.subarray(0, bytesRead);
      const contentType = sniffImage(bytes);
      return contentType
        ? { ok: true, contentType, bytes: Buffer.from(bytes) }
        : { ok: false, reason: 'not_an_image' };
    } finally {
      await handle.close();
    }
  } catch {
    return { ok: false, reason: 'not_found' };
  }
}
