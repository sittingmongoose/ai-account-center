/** Read-only, session-only deliverables. Request paths are relative to the machine share. */
import fs from 'fs';
import path from 'path';
import { createApiRouter } from './api-router';
import { requireBrowserSession } from './status-routes';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import { openRegularFile } from '../status/share-read';
import { parseByteRange, sendFileBytes } from '../status/status-files';

export const MAX_SHARE_BYTES = 100 * 1024 * 1024;
const TYPES: Record<string, string> = {
  '.html': 'text/html; charset=utf-8',
  '.md': 'text/plain; charset=utf-8',
  '.txt': 'text/plain; charset=utf-8',
  '.json': 'application/json',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.gif': 'image/gif',
  '.webp': 'image/webp',
  '.svg': 'image/svg+xml',
  '.mp4': 'video/mp4',
  '.webm': 'video/webm',
  '.pdf': 'application/pdf',
};
// No scripts, origin identity, forms, popups, connections or outside resources. Inline styling
// and embedded images keep standalone concepts readable. Applies to every type, including errors.
export const SHARE_CSP =
  "sandbox; default-src 'none'; script-src 'none'; connect-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'self'";

function safePath(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length > 0 &&
    value.length <= 2048 &&
    !/[\\:%\x00-\x1f\x7f]/.test(value) &&
    value.split('/').every((part) => part.length > 0 && !part.startsWith('.'))
  );
}

function allowedBase(relative: string): string | null {
  const parts = relative.split('/');
  if (parts[0] === 'share' && parts.length > 1) return 'share';
  if (
    parts[0] === 'agent-reports' &&
    parts[1] === 'v1' &&
    parts[2] === 'writers' &&
    /^[a-zA-Z0-9][a-zA-Z0-9._-]{0,127}$/.test(parts[3] ?? '') &&
    parts[4] === 'media' &&
    parts.length > 5
  )
    return parts.slice(0, 5).join('/');
  return null;
}

export function machineShareRoot(): string {
  return process.platform === 'win32'
    ? '\\\\TRUENAS\\Cursor'
    : process.platform === 'darwin'
      ? '/Volumes/Cursor'
      : '/mnt/Cursor';
}

/**
 * The descriptor check deliberately fails closed without Linux /proc. Unlike a second path stat,
 * it proves the file actually opened is confined even if a writer swaps a parent for a symlink.
 * Other platforms need an equivalent native handle-path primitive before enabling this route.
 */
export function createShareRouter(root: () => string = machineShareRoot) {
  const router = createApiRouter();
  router.use(requireBrowserSession);
  router.use(async (req, res) => {
    res.setHeader('Cache-Control', 'no-store');
    res.setHeader('X-Content-Type-Options', 'nosniff');
    res.setHeader('Content-Security-Policy', SHARE_CSP);
    res.setHeader('Referrer-Policy', 'no-referrer');
    res.setHeader('Cross-Origin-Resource-Policy', 'same-origin');
    const refuse = (status: number, code: string, error: string) =>
      res.status(status).json({ code, error });
    if (!isDashboardWebSocketOriginAllowed(req)) {
      refuse(403, 'wrong_origin', 'Use the dashboard origin.');
      return;
    }
    if (req.method !== 'GET' && req.method !== 'HEAD') {
      res.setHeader('Allow', 'GET, HEAD');
      refuse(405, 'read_only', 'Only GET and HEAD are supported.');
      return;
    }
    const relative = req.query.path;
    const base = safePath(relative) ? allowedBase(relative) : null;
    if (req.path !== '/' || Object.keys(req.query).length !== 1 || !base || !safePath(relative)) {
      refuse(400, 'invalid_path', 'Use a share-relative file path.');
      return;
    }
    const extension = path.extname(relative).toLowerCase();
    const type = TYPES[extension];
    if (!type) {
      refuse(415, 'unsupported_type', 'This file type is not supported.');
      return;
    }
    if (process.platform !== 'linux') {
      refuse(503, 'unsupported_host', 'Secure share file access is unavailable on this host.');
      return;
    }
    let opened: Awaited<ReturnType<typeof openRegularFile>> | undefined;
    try {
      const machine = await fs.promises.realpath(root());
      const folder = path.join(machine, base);
      // The allowlisted root cannot itself be redirected by a writer.
      if ((await fs.promises.realpath(folder)) !== folder) {
        refuse(404, 'not_found', 'This file is not available.');
        return;
      }
      const file = await fs.promises.realpath(path.join(machine, relative));
      const realRelative = path.relative(folder, file).split(path.sep).join('/');
      if (!file.startsWith(folder + path.sep) || !safePath(realRelative)) {
        refuse(404, 'not_found', 'This file is not available.');
        return;
      }
      opened = await openRegularFile(file);
      const actual = await fs.promises.readlink(`/proc/self/fd/${opened.handle.fd}`);
      // Exact equality also refuses deleted/moved files and symlink races after realpath.
      if (actual !== file) {
        refuse(404, 'not_found', 'This file is not available.');
        return;
      }
      const { handle, stat } = opened;
      if (stat.size > MAX_SHARE_BYTES) {
        refuse(413, 'too_large', 'This file exceeds the 100 MB limit.');
        return;
      }
      const range = type.startsWith('video/') ? parseByteRange(req.headers.range, stat.size) : null;
      if (range === 'unsatisfiable') {
        res.setHeader('Content-Range', `bytes */${stat.size}`);
        refuse(416, 'invalid_range', 'The requested range is outside this file.');
        return;
      }
      res.setHeader('Content-Type', type);
      if (extension === '.svg') res.setHeader('Content-Disposition', 'attachment');
      if (type.startsWith('video/')) res.setHeader('Accept-Ranges', 'bytes');
      const start = range ? range.start : 0;
      const end = range ? range.end : stat.size - 1;
      res.status(range ? 206 : 200);
      if (range) res.setHeader('Content-Range', `bytes ${start}-${end}/${stat.size}`);
      res.setHeader('Content-Length', String(Math.max(0, end - start + 1)));
      if (req.method === 'HEAD' || stat.size === 0) res.end();
      else await sendFileBytes(handle, res, start, end);
    } catch {
      if (res.headersSent) res.destroy();
      else refuse(404, 'not_found', 'This file is not available.');
    } finally {
      await opened?.handle.close().catch(() => undefined);
    }
  });
  return router;
}
