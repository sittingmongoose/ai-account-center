/**
 * On-the-fly compression for /api JSON responses.
 *
 * The dashboard files ship precompressed (static-ui.ts); API JSON is built per
 * request, and the analytics payloads re-send the same highly repetitive data
 * on every poll, so they are encoded here instead. A response body is buffered
 * up to a cap and only encoded when it is JSON above a small threshold, the
 * status allows a body, and the client accepts the coding (the q-value rules
 * of static-ui.negotiateEncoding). Anything else passes through untouched.
 */
import zlib from 'zlib';
import type { NextFunction, Request, RequestHandler, Response } from 'express';
import { negotiateEncoding, type UiEncoding } from './static-ui';

/** JSON above this size is encoded; below it the coding costs more than it saves. */
const MIN_COMPRESS_BYTES = 1024;
/** Hard cap on the buffered body; larger answers stream out unencoded. */
const MAX_BUFFERED_BYTES = 8 * 1024 * 1024;
/** Brotli quality for per-request encoding: near-default size at a fraction of the CPU. */
const BROTLI_QUALITY = 4;
const CODINGS: Partial<Record<UiEncoding, unknown>> = { br: true, gzip: true };
const JSON_TYPE = /^application\/json\b/i;

interface PendingWrite {
  data: Buffer;
  callback?: () => void;
}

function toBuffer(chunk: unknown, encoding: unknown): Buffer | null {
  if (chunk === undefined || chunk === null) return null;
  if (Buffer.isBuffer(chunk)) return chunk;
  if (typeof chunk === 'string')
    return Buffer.from(chunk, typeof encoding === 'string' ? (encoding as BufferEncoding) : 'utf8');
  if (chunk instanceof Uint8Array) return Buffer.from(chunk);
  return Buffer.from(String(chunk));
}

/**
 * Mounted on /api ahead of the router. The write/end pair buffers candidate
 * bodies; everything is decided once, at end(), while the headers are still
 * ours to change.
 */
export function apiCompression(): RequestHandler {
  return (req: Request, res: Response, next: NextFunction): void => {
    const originalWrite = res.write.bind(res) as unknown as (
      data: Buffer,
      cb?: () => void
    ) => boolean;
    const originalEnd = res.end.bind(res) as unknown as (data?: Buffer | null) => void;
    const pending: PendingWrite[] = [];
    let buffered = 0;
    let passthrough = false;

    /** Give up on encoding: buffered chunks and everything after go out as they are. */
    const release = (): void => {
      passthrough = true;
      for (const write of pending) originalWrite(write.data, write.callback);
      pending.length = 0;
      buffered = 0;
    };

    /** Only while nothing has left: sent headers, a coding or a non-JSON type rule it out. */
    const candidate = (): boolean =>
      !passthrough &&
      !res.headersSent &&
      res.getHeader('Content-Encoding') === undefined &&
      JSON_TYPE.test(String(res.getHeader('Content-Type') ?? ''));

    res.write = ((chunk: unknown, encoding?: unknown, callback?: unknown): boolean => {
      if (typeof encoding === 'function') {
        callback = encoding;
        encoding = undefined;
      }
      const data = toBuffer(chunk, encoding);
      if (data === null) return true;
      const cb = typeof callback === 'function' ? (callback as () => void) : undefined;
      if (!candidate() || buffered + data.length > MAX_BUFFERED_BYTES) {
        release();
        return originalWrite(data, cb);
      }
      pending.push({ data, callback: cb });
      buffered += data.length;
      return true;
    }) as unknown as Response['write'];

    res.end = ((chunk?: unknown, encoding?: unknown, callback?: unknown): Response => {
      if (typeof chunk === 'function') {
        callback = chunk;
        chunk = undefined;
      }
      if (typeof encoding === 'function') {
        callback = encoding;
        encoding = undefined;
      }
      if (typeof callback === 'function') res.once('finish', callback as () => void);
      const data = toBuffer(chunk, encoding);

      if (data !== null) {
        if (!candidate() || buffered + data.length > MAX_BUFFERED_BYTES) {
          release();
          originalEnd(data);
          return res;
        }
        pending.push({ data });
        buffered += data.length;
      } else if (!candidate()) {
        release();
        originalEnd();
        return res;
      }

      // Above the threshold the representation depends on Accept-Encoding,
      // encoded or not; below it every client gets the same bytes.
      const status = res.statusCode;
      if (buffered <= MIN_COMPRESS_BYTES || status < 200 || status === 204 || status === 304) {
        release();
        originalEnd();
        return res;
      }
      res.vary('Accept-Encoding');
      const coding = negotiateEncoding(req.headers['accept-encoding'], CODINGS);
      if (!coding) {
        release();
        originalEnd();
        return res;
      }

      const body = Buffer.concat(
        pending.map((write) => write.data),
        buffered
      );
      const callbacks = pending
        .map((write) => write.callback)
        .filter((cb): cb is () => void => cb !== undefined);
      pending.length = 0;
      // The encoded bytes go out chunked; the identity length would be a lie.
      res.removeHeader('Content-Length');
      res.setHeader('Content-Encoding', coding);
      const compressor =
        coding === 'br'
          ? zlib.createBrotliCompress({
              params: { [zlib.constants.BROTLI_PARAM_QUALITY]: BROTLI_QUALITY },
            })
          : zlib.createGzip();
      compressor.on('data', (encoded: Buffer) => originalWrite(encoded));
      compressor.once('error', () => {
        // Encoding an in-memory buffer failing is practically unreachable;
        // until the headers are out the identity body can still answer.
        if (res.headersSent) {
          originalEnd();
          return;
        }
        res.removeHeader('Content-Encoding');
        for (const cb of callbacks) cb();
        originalWrite(body);
        originalEnd();
      });
      compressor.once('end', () => {
        for (const cb of callbacks) cb();
        originalEnd();
      });
      compressor.end(body);
      return res;
    }) as unknown as Response['end'];

    next();
  };
}
