/**
 * Serving the packaged Slint dashboard (dist/ui).
 *
 * - Precompressed variants: scripts/build-ui.js writes .br and .gz copies and lists
 *   them in ui-build-manifest.json. They are checked once at startup (size and
 *   SHA-256) and served by content negotiation. Nothing here is compressed on
 *   the fly; /api JSON is encoded per request (api-compression.ts).
 * - Cache-Control by path: pkg/<buildId>/** is immutable; everything else revalidates.
 * - Page routes: /, /login, /analytics, /accounts and /accounts/<provider> answer
 *   with index.html; a few aliases redirect; anything else goes back to /.
 */

import crypto from 'crypto';
import fs from 'fs';
import type http from 'http';
import path from 'path';
import { pipeline } from 'stream';
import type { NextFunction, Request, RequestHandler, Response } from 'express';
import { createLogger } from '../services/logging';

export type UiEncoding = 'br' | 'gzip';

interface UiVariant {
  file: string;
  bytes: number;
  etag: string;
}

export interface StaticUi {
  root: string;
  buildId: string | null;
  commit: string | null;
  variants: Map<string, Partial<Record<UiEncoding, UiVariant>>>;
}

const logger = createLogger('web-server');
const MAX_MANIFEST_BYTES = 1024 * 1024;
const MAX_VARIANTS = 4096;
const IMMUTABLE = 'public, max-age=31536000, immutable';
const VERSIONED_PKG = /^pkg\/[a-f0-9]{12}\//;
const SUFFIX: Record<UiEncoding, string> = { br: '.br', gzip: '.gz' };
const CONTENT_TYPES = new Map<string, string>([
  ['.wasm', 'application/wasm'],
  ['.js', 'text/javascript; charset=utf-8'],
  ['.mjs', 'text/javascript; charset=utf-8'],
  ['.css', 'text/css; charset=utf-8'],
  ['.html', 'text/html; charset=utf-8'],
  ['.svg', 'image/svg+xml'],
  ['.json', 'application/json; charset=utf-8'],
  ['.txt', 'text/plain; charset=utf-8'],
  // PWA: the install manifest and the install icons (sw.js is .js above).
  ['.webmanifest', 'application/manifest+json'],
  ['.png', 'image/png'],
]);
/** Build facts for the build scripts only; the UI never fetches it, so it is not served. */
const UNSERVED = new Set(['ui-build-manifest.json']);
/**
 * Root icon aliases probed by iOS Springboard and desktop browsers.
 * Served directly from icons/ without requiring session auth.
 */
const STATIC_ICON_ALIASES: ReadonlyMap<string, string> = new Map<string, string>([
  ['/apple-touch-icon.png', 'icons/apple-touch-icon-180.png'],
  ['/apple-touch-icon-precomposed.png', 'icons/apple-touch-icon-180.png'],
  ['/favicon.ico', 'icons/apple-touch-icon-180.png'],
]);

const PAGES = new Set(['/', '/login', '/analytics', '/accounts']);
const PAGE_ALIASES = new Map([
  ['/settings', '/accounts'],
  ['/home', '/'],
]);

function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function isSafeRelativePath(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length > 0 &&
    value.length <= 512 &&
    !value.startsWith('/') &&
    !value.includes('\\') &&
    !value.includes('\0') &&
    value.split('/').every((segment) => segment !== '' && segment !== '.' && segment !== '..')
  );
}

function isInside(root: string, candidate: string): boolean {
  return candidate.startsWith(`${root}${path.sep}`);
}

function readManifest(root: string): Record<string, unknown> | null {
  try {
    const file = path.join(root, 'ui-build-manifest.json');
    const stat = fs.lstatSync(file);
    if (!stat.isFile() || stat.size > MAX_MANIFEST_BYTES) return null;
    return record(JSON.parse(fs.readFileSync(file, 'utf8')));
  } catch {
    return null;
  }
}

/** One manifest row, checked against the file on disk; null disables the variant. */
function checkVariant(
  root: string,
  entry: Record<string, unknown> | null
): { path: string; encoding: UiEncoding; variant: UiVariant } | null {
  const encoding = entry?.encoding;
  if (!entry || (encoding !== 'br' && encoding !== 'gzip')) return null;
  const original = entry.path;
  if (
    !isSafeRelativePath(original) ||
    entry.file !== `${original}${SUFFIX[encoding]}` ||
    !CONTENT_TYPES.has(path.extname(original).toLowerCase()) ||
    typeof entry.bytes !== 'number' ||
    !Number.isSafeInteger(entry.bytes) ||
    entry.bytes < 1 ||
    typeof entry.sha256 !== 'string' ||
    !/^[a-f0-9]{64}$/.test(entry.sha256)
  )
    return null;
  const file = path.resolve(root, `${original}${SUFFIX[encoding]}`);
  const source = path.resolve(root, original);
  if (!isInside(root, file) || !isInside(root, source)) return null;
  try {
    const stat = fs.lstatSync(file);
    const sourceStat = fs.lstatSync(source);
    if (!stat.isFile() || stat.size !== entry.bytes || !sourceStat.isFile()) return null;
    const digest = crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
    if (digest !== entry.sha256) return null;
    return {
      path: original,
      encoding,
      variant: { file, bytes: entry.bytes, etag: `"${digest.slice(0, 16)}-${encoding}"` },
    };
  } catch {
    return null;
  }
}

/** Read the build manifest and verify every listed variant once, at startup. */
export function loadStaticUi(staticDir: string): StaticUi {
  const root = path.resolve(staticDir);
  const manifest = readManifest(root);
  const variants: StaticUi['variants'] = new Map();
  const rows = Array.isArray(manifest?.precompressed) ? manifest.precompressed : [];
  for (const row of rows.slice(0, MAX_VARIANTS)) {
    const entry = record(row);
    const checked = checkVariant(root, entry);
    if (!checked) {
      // Identity serving never depends on a variant; a bad one is only switched off.
      logger.warn('ui.precompressed.disabled', 'A precompressed dashboard file was disabled', {
        path: isSafeRelativePath(entry?.path) ? entry.path : null,
        encoding: entry?.encoding === 'br' || entry?.encoding === 'gzip' ? entry.encoding : null,
      });
      continue;
    }
    variants.set(checked.path, {
      ...variants.get(checked.path),
      [checked.encoding]: checked.variant,
    });
  }
  const buildId =
    typeof manifest?.buildId === 'string' && /^[a-f0-9]{12}$/.test(manifest.buildId)
      ? manifest.buildId
      : null;
  const commit =
    typeof manifest?.commit === 'string' && /^[a-f0-9]{7,40}$/.test(manifest.commit)
      ? manifest.commit
      : null;
  return { root, buildId, commit, variants };
}

/** Cache-Control by path: content-addressed pkg/<buildId>/** never changes. */
export function uiCacheControl(relativePath: string): string {
  return VERSIONED_PKG.test(relativePath) ? IMMUTABLE : 'no-cache';
}

/**
 * Accept-Encoding with q-values. The highest q wins, br before gzip on a tie;
 * q=0 excludes a coding, and `*` covers codings that are not named.
 */
export function negotiateEncoding(
  header: string | undefined,
  available: Partial<Record<UiEncoding, unknown>>
): UiEncoding | null {
  if (typeof header !== 'string' || !header || header.length > 2048) return null;
  const qualities = new Map<string, number>();
  for (const part of header.split(',')) {
    const [name, ...parameters] = part.split(';');
    const coding = name.trim().toLowerCase();
    if (!coding || qualities.has(coding)) continue;
    let quality = 1;
    for (const parameter of parameters) {
      const [key, value = ''] = parameter.split('=');
      if (key.trim().toLowerCase() !== 'q') continue;
      const text = value.trim();
      quality = /^(?:0(?:\.\d{0,3})?|1(?:\.0{0,3})?)$/.test(text) ? Number(text) : 0;
    }
    qualities.set(coding, quality);
  }
  const quality = (coding: UiEncoding): number =>
    qualities.get(coding) ??
    (coding === 'gzip' ? qualities.get('x-gzip') : undefined) ??
    qualities.get('*') ??
    0;
  let chosen: UiEncoding | null = null;
  for (const coding of ['br', 'gzip'] as const) {
    if (!available[coding] || quality(coding) <= 0) continue;
    if (!chosen || quality(coding) > quality(chosen)) chosen = coding;
  }
  return chosen;
}

/** The decoded path below the static root, or null when it cannot be one. */
function relativeUiPath(root: string, requestPath: string): string | null {
  const alias = STATIC_ICON_ALIASES.get(requestPath);
  if (alias) return alias;
  let decoded: string;
  try {
    decoded = decodeURIComponent(requestPath);
  } catch {
    return null;
  }
  if (decoded.includes('\0') || decoded.includes('\\')) return null;
  if (decoded.split('/').some((segment) => segment === '..')) return null;
  const relative = decoded.replace(/^\/+/, '');
  if (relative && !isInside(root, path.resolve(root, relative))) return null;
  return relative;
}

/**
 * Whether a request path addresses an existing file below the static root,
 * under the same safety rules serving applies. The addresses serving refuses
 * (.br/.gz, the build manifest) answer JSON instead, so they are not files
 * for this purpose.
 */
export function isStaticUiFileRequest(root: string, requestPath: string): boolean {
  const relative = relativeUiPath(root, requestPath);
  if (!relative) return false;
  const lower = relative.toLowerCase();
  if (lower.endsWith('.br') || lower.endsWith('.gz') || UNSERVED.has(path.posix.normalize(lower)))
    return false;
  try {
    return fs.statSync(path.resolve(root, relative)).isFile();
  } catch {
    return false;
  }
}

function setSharedHeaders(ui: StaticUi, res: http.ServerResponse, relative: string): void {
  res.setHeader('Cache-Control', uiCacheControl(relative));
  const type = CONTENT_TYPES.get(path.extname(relative).toLowerCase());
  if (type) res.setHeader('Content-Type', type);
  if (relative === 'index.html') {
    res.setHeader('X-Content-Type-Options', 'nosniff');
    res.setHeader('Referrer-Policy', 'same-origin');
  }
  if (ui.variants.has(relative)) res.setHeader('Vary', 'Accept-Encoding');
}

/** express.static setHeaders for identity answers: the same table as compressed ones. */
export function uiStaticHeaders(ui: StaticUi): (res: http.ServerResponse, file: string) => void {
  return (res, file) => {
    const relative = path.relative(ui.root, file).split(path.sep).join('/');
    setSharedHeaders(ui, res, relative);
  };
}

/**
 * Answer with a precompressed variant when one fits the request; otherwise call
 * `identity`. A variant that cannot be opened at request time also falls back.
 */
function serveUiFile(
  ui: StaticUi,
  req: Request,
  res: Response,
  relative: string,
  identity: () => void
): void {
  const entry = ui.variants.get(relative);
  const encoding = entry ? negotiateEncoding(req.headers['accept-encoding'], entry) : null;
  const variant = encoding ? entry?.[encoding] : undefined;
  if (!encoding || !variant) {
    identity();
    return;
  }
  setSharedHeaders(ui, res, relative);
  res.setHeader('ETag', variant.etag);
  // A Range header is ignored: a compressed variant is always a whole 200.
  if (req.fresh) {
    res.status(304).end();
    return;
  }
  fs.open(variant.file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0), (error, fd) => {
    if (error) {
      identity();
      return;
    }
    fs.fstat(fd, (statError, stat) => {
      if (statError || !stat.isFile() || stat.size !== variant.bytes) {
        fs.close(fd, () => identity());
        return;
      }
      // The client may have gone while the file was opened: release the fd, send nothing.
      if (req.destroyed || res.destroyed || res.writableEnded) {
        fs.close(fd, () => undefined);
        return;
      }
      res.status(200);
      res.setHeader('Content-Encoding', encoding);
      res.setHeader('Content-Length', String(variant.bytes));
      if (req.method === 'HEAD') {
        fs.close(fd, () => undefined);
        res.end();
        return;
      }
      // pipeline destroys both sides (and so closes the fd) on an error or an abort.
      pipeline(fs.createReadStream('', { fd, autoClose: true }), res, () => undefined);
    });
  });
}

function withoutVariantHeaders(res: Response): void {
  for (const header of ['ETag', 'Vary', 'Cache-Control', 'Content-Type']) res.removeHeader(header);
}

/**
 * Mounted before express.static. GET and HEAD only; .br/.gz and the build
 * manifest are never addressed directly; paths with a listed variant are
 * negotiated, the rest fall through.
 */
export function precompressedStatic(ui: StaticUi): RequestHandler {
  return (req: Request, res: Response, next: NextFunction): void => {
    if (req.method !== 'GET' && req.method !== 'HEAD') {
      next();
      return;
    }
    const relative = relativeUiPath(ui.root, req.path);
    if (relative === null) {
      next();
      return;
    }
    const lower = relative.toLowerCase();
    // `./` segments are folded first, so `/./ui-build-manifest.json` is refused too.
    if (
      lower.endsWith('.br') ||
      lower.endsWith('.gz') ||
      UNSERVED.has(path.posix.normalize(lower))
    ) {
      res.status(404).json({ error: 'Not found.' });
      return;
    }
    serveUiFile(ui, req, res, relative, () => {
      withoutVariantHeaders(res);
      if (STATIC_ICON_ALIASES.has(req.path)) {
        setSharedHeaders(ui, res, relative);
        res.sendFile(path.join(ui.root, relative), (error?: Error) => {
          if (error && !res.headersSent) next(error);
        });
        return;
      }
      next();
    });
  };
}

export type PageRouteResult =
  | { kind: 'page' }
  | { kind: 'redirect'; status: 301 | 302; location: string };

/** Exact, case-sensitive page routes. Queries are not read here. */
export function resolvePageRoute(
  pathname: string,
  providerIds: ReadonlySet<string>
): PageRouteResult {
  const isProviderPage = (candidate: string): boolean => {
    const match = /^\/accounts\/([a-z0-9-]{1,32})$/.exec(candidate);
    return match !== null && providerIds.has(match[1]);
  };
  const isKnown = (candidate: string): boolean =>
    PAGES.has(candidate) || PAGE_ALIASES.has(candidate) || isProviderPage(candidate);
  if (PAGES.has(pathname) || isProviderPage(pathname)) return { kind: 'page' };
  const alias = PAGE_ALIASES.get(pathname);
  if (alias) return { kind: 'redirect', status: 301, location: alias };
  if (pathname.length > 1 && pathname.endsWith('/') && isKnown(pathname.slice(0, -1)))
    return { kind: 'redirect', status: 301, location: pathname.slice(0, -1) };
  return { kind: 'redirect', status: 302, location: '/' };
}

/** The app.get('*') fallback: page routes get index.html, the rest are redirected. */
export function pageRouteHandler(ui: StaticUi, providerIds: ReadonlySet<string>): RequestHandler {
  return (req: Request, res: Response, next: NextFunction): void => {
    const route = resolvePageRoute(req.path, providerIds);
    if (route.kind === 'redirect') {
      res.redirect(route.status, route.location);
      return;
    }
    serveUiFile(ui, req, res, 'index.html', () => {
      withoutVariantHeaders(res);
      setSharedHeaders(ui, res, 'index.html');
      res.sendFile(path.join(ui.root, 'index.html'), (error?: Error) => {
        if (error && !res.headersSent) next(error);
      });
    });
  };
}
