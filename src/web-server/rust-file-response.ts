/** Validate and render a bounded private file reply; public route guards run before this. */
import type { Request, Response } from 'express';
import { RustService } from './rust-service';

interface FileReply {
  status: number;
  headers: Record<string, string>;
  json: { code: string; error: string } | null;
  bytes: string | null;
}
const HEADERS = new Set([
  'content-type',
  'content-length',
  'content-range',
  'accept-ranges',
  'cache-control',
  'content-disposition',
  'x-content-type-options',
  'content-security-policy',
]);
const TYPES = new Set([
  'image/png',
  'image/jpeg',
  'image/webp',
  'image/gif',
  'image/svg+xml',
  'video/mp4',
  'video/webm',
  'application/pdf',
  'application/json',
  'text/html; charset=utf-8',
  'text/plain; charset=utf-8',
]);
function validReply(value: unknown, head: boolean, media: boolean): value is FileReply {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const reply = value as FileReply;
  if (
    ![200, 206, 400, 404, 413, 415, 416].includes(reply.status) ||
    !reply.headers ||
    typeof reply.headers !== 'object' ||
    Array.isArray(reply.headers)
  )
    return false;
  for (const [key, value] of Object.entries(reply.headers)) {
    if (
      !HEADERS.has(key) ||
      typeof value !== 'string' ||
      value.length > 256 ||
      /[\r\n\x00]/.test(value)
    )
      return false;
    if (key === 'content-type' && !TYPES.has(value)) return false;
    if (key === 'content-security-policy' && (!media || value !== "default-src 'none'; sandbox"))
      return false;
    if (key === 'x-content-type-options' && value !== 'nosniff') return false;
    if (key === 'cache-control' && (!media || value !== 'private, max-age=300')) return false;
    if (key === 'content-disposition' && value !== 'attachment') return false;
    if (key === 'accept-ranges' && value !== 'bytes') return false;
  }
  if (
    reply.headers['content-range'] &&
    !/^bytes (?:[0-9]+-[0-9]+|\*)\/[0-9]+$/.test(reply.headers['content-range'])
  )
    return false;
  if (reply.status !== 200 && reply.status !== 206) {
    const errors: Record<string, [number, string]> = media
      ? {
          not_found: [404, 'This file is not available.'],
          too_large: [413, 'This file is larger than its limit (images 2 MB, videos 50 MB).'],
          not_media: [415, 'This file is not a PNG, JPEG or WebP image or an MP4 or WebM video.'],
          range_not_satisfiable: [416, 'The requested range is outside this file.'],
        }
      : {
          invalid_path: [400, 'Use a share-relative file path.'],
          not_found: [404, 'This file is not available.'],
          too_large: [413, 'This file exceeds the 100 MB limit.'],
          unsupported_type: [415, 'This file type is not supported.'],
          invalid_range: [416, 'The requested range is outside this file.'],
        };
    const expected = reply.json && errors[reply.json.code];
    return (
      reply.bytes === null &&
      !!expected &&
      reply.status === expected[0] &&
      reply.json?.error === expected[1]
    );
  }
  if (
    reply.json !== null ||
    typeof reply.bytes !== 'string' ||
    reply.bytes.length > 1_400_000 ||
    !TYPES.has(reply.headers['content-type'])
  )
    return false;
  if (
    media &&
    (!['image/png', 'image/jpeg', 'image/webp', 'video/mp4', 'video/webm'].includes(
      reply.headers['content-type']
    ) ||
      reply.headers['x-content-type-options'] !== 'nosniff' ||
      reply.headers['content-security-policy'] !== "default-src 'none'; sandbox" ||
      reply.headers['accept-ranges'] !== 'bytes' ||
      reply.headers['cache-control'] !== 'private, max-age=300')
  )
    return false;
  if (
    !media &&
    reply.headers['content-type'] === 'image/svg+xml' &&
    reply.headers['content-disposition'] !== 'attachment'
  )
    return false;
  if (reply.status === 206 && !reply.headers['content-range']) return false;
  const bytes = Buffer.from(reply.bytes, 'base64');
  if (bytes.toString('base64') !== reply.bytes) return false;
  const length = reply.headers['content-length'];
  return (
    /^\d{1,9}$/.test(length ?? '') &&
    Number(length) <= 100 * 1024 * 1024 &&
    (head ? bytes.length === 0 : Number(length) === bytes.length)
  );
}

/** A successful on-mode read skips the legacy filesystem path entirely. Shadow keeps its TS stream. */
export async function sendRustFile(
  req: Request,
  res: Response,
  route: string,
  input: unknown
): Promise<boolean> {
  const service: unknown = res.locals.rustService;
  if (!(service instanceof RustService) || service.mode !== 'on') return false;
  const reply = (await service.compose(
    req,
    route,
    input,
    () => null,
    (body) => validReply(body, req.method === 'HEAD', route.startsWith('/api/status/files/'))
  )) as FileReply | null;
  if (!reply) return false;
  for (const [key, value] of Object.entries(reply.headers)) res.setHeader(key, value);
  res.status(reply.status);
  if (reply.json) res.json(reply.json);
  else if (req.method === 'HEAD') res.end();
  else res.end(Buffer.from(reply.bytes ?? '', 'base64'));
  return true;
}
