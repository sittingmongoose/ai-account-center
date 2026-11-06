/**
 * Media a card shows (`GET /api/status/files/<writer>/<path>`): only files inside that writer's own
 * folder of the reports share. A path with `..`, an absolute path or a link that resolves outside
 * the folder is not served. The type comes from the file's first bytes, never its name: images PNG,
 * JPEG or WebP up to 2 MB; videos MP4 (an `ftyp` box) or WebM (an EBML header naming `webm`) up to
 * 50 MB. Every file is served with byte ranges (iOS Safari plays no video without them).
 */
import fs from 'fs';
import path from 'path';
import type { Writable } from 'stream';
import { isWriterFilePath, isWriterName } from './report-schema';
import { openRegularFile } from './share-read';

export const MAX_CARD_IMAGE_BYTES = 2 * 1024 * 1024;
export const MAX_CARD_VIDEO_BYTES = 50 * 1024 * 1024;
const SNIFF_BYTES = 64;
const SEND_CHUNK_BYTES = 64 * 1024;

export type CardFileType = 'image/png' | 'image/jpeg' | 'image/webp' | 'video/mp4' | 'video/webm';

export type CardFile =
  | { ok: true; contentType: CardFileType; handle: fs.promises.FileHandle; size: number }
  | { ok: false; reason: 'not_found' | 'too_large' | 'not_media' };

/** ISO base media brands that are still images, not video. */
const IMAGE_BRANDS = new Set(['avif', 'avis', 'heic', 'heix', 'heim', 'heis', 'mif1', 'msf1']);

/** The media type of a file's first bytes, by content. */
export function sniffMedia(bytes: Buffer): CardFileType | null {
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
  if (bytes.length >= 12 && bytes.subarray(4, 8).toString('latin1') === 'ftyp') {
    const brand = bytes.subarray(8, 12).toString('latin1');
    return IMAGE_BRANDS.has(brand) || brand === 'qt  ' ? null : 'video/mp4';
  }
  if (
    bytes.length >= 4 &&
    bytes.subarray(0, 4).equals(Buffer.from([0x1a, 0x45, 0xdf, 0xa3])) &&
    bytes.subarray(4).includes(Buffer.from('webm', 'latin1'))
  )
    return 'video/webm';
  return null;
}

/**
 * Finds, confines and types one writer file. On success the caller owns the open handle and closes
 * it: the bytes served come from the same file that was sniffed, even if the path is swapped later.
 */
export async function openCardFile(root: string, writer: string, file: string): Promise<CardFile> {
  if (!isWriterName(writer) || !isWriterFilePath(file)) return { ok: false, reason: 'not_found' };
  try {
    const writersDir = await fs.promises.realpath(path.join(root, 'v1', 'writers'));
    const base = await fs.promises.realpath(path.join(writersDir, writer));
    // A writer folder that is itself a link would make its target the folder files are confined to.
    if (base !== path.join(writersDir, writer)) return { ok: false, reason: 'not_found' };
    const target = await fs.promises.realpath(path.join(base, ...file.split('/')));
    if (!target.startsWith(base + path.sep)) return { ok: false, reason: 'not_found' };
    const { handle, stat } = await openRegularFile(target);
    let keep = false;
    try {
      const head = Buffer.alloc(SNIFF_BYTES);
      const { bytesRead } = await handle.read(head, 0, SNIFF_BYTES, 0);
      const contentType = sniffMedia(head.subarray(0, bytesRead));
      if (!contentType) return { ok: false, reason: 'not_media' };
      const max = contentType.startsWith('video/') ? MAX_CARD_VIDEO_BYTES : MAX_CARD_IMAGE_BYTES;
      if (stat.size > max) return { ok: false, reason: 'too_large' };
      keep = true;
      return { ok: true, contentType, handle, size: stat.size };
    } finally {
      if (!keep) await handle.close();
    }
  } catch {
    return { ok: false, reason: 'not_found' };
  }
}

/**
 * Writes bytes `start..end` of an open file to the response and ends it. A file that shrank since
 * its size was read cannot fill the `Content-Length` already promised, so the response is destroyed:
 * the client sees a failed transfer, never a short file presented as whole.
 */
export async function sendFileBytes(
  handle: fs.promises.FileHandle,
  res: Writable,
  start: number,
  end: number
): Promise<void> {
  let closed = false;
  const onClose = () => {
    closed = true;
  };
  res.once('close', onClose);
  try {
    let position = start;
    while (position <= end && !closed) {
      // A fresh buffer each time: a queued write keeps a reference to the one it was given.
      const chunk = Buffer.allocUnsafe(Math.min(SEND_CHUNK_BYTES, end - position + 1));
      const { bytesRead } = await handle.read(chunk, 0, chunk.length, position);
      if (bytesRead === 0) {
        res.destroy();
        return;
      }
      position += bytesRead;
      if (!res.write(chunk.subarray(0, bytesRead)) && !closed)
        await new Promise<void>((resolve) => {
          const done = () => {
            res.off('drain', done);
            res.off('close', done);
            resolve();
          };
          res.once('drain', done);
          res.once('close', done);
        });
    }
    if (!closed) res.end();
  } catch {
    res.destroy();
  } finally {
    res.off('close', onClose);
  }
}

export type ByteRange = { start: number; end: number } | 'unsatisfiable' | null;

/**
 * One `Range: bytes=` range against a file of `size` bytes: null for no (or an ignorable) header,
 * `unsatisfiable` for a range past the end. Several ranges are answered with the whole file.
 */
export function parseByteRange(header: unknown, size: number): ByteRange {
  if (typeof header !== 'string') return null;
  const match = /^bytes=(\d*)-(\d*)$/.exec(header.trim());
  if (!match || (match[1] === '' && match[2] === '')) return null;
  if (match[1] === '') {
    const suffix = Number(match[2]);
    if (!Number.isSafeInteger(suffix) || suffix === 0) return 'unsatisfiable';
    return size === 0 ? 'unsatisfiable' : { start: Math.max(0, size - suffix), end: size - 1 };
  }
  const start = Number(match[1]);
  const end = match[2] === '' ? size - 1 : Math.min(Number(match[2]), size - 1);
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end)) return null;
  if (start >= size || start > end) return 'unsatisfiable';
  return { start, end };
}
