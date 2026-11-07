import type { NextFunction, Request, Response, Router } from 'express';
import { createApiRouter } from './api-router';
import { hasOnlyKeys, sendError } from './account-route-guards';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import { authKind } from '../middleware/request-auth';
import { getStatusService, type StatusService } from '../status/status-service';
import {
  answerDevice,
  deliveryView,
  isClientSendId,
  validEntryIds,
  type AnswerHistoryEntry,
  type EntryResult,
} from '../status/answers';
import { openCardFile, parseByteRange, sendFileBytes } from '../status/status-files';

/**
 * The Status tab API (docs/status-tab.md):
 * - `GET /api/status`: the whole view;
 * - `GET /api/status/answers?limit=N`: the owner's answers and notes, newest first;
 * - `POST /api/status/answers`: an answer set from the deck, one result per entry;
 * - `GET /api/status/files/<writer>/<path>`: card media (images, videos with byte ranges).
 * Every route: a browser session only (never a device token, never localhost without sign-in).
 * The POST: no query string, the dashboard Origin, application/json and a strict body of at most
 * 64 KB. Errors are `{ error, code }` with fixed sentences.
 */
export const MAX_STATUS_BODY_BYTES = 64 * 1024;
export const MAX_ANSWER_ENTRIES = 50;

function invalid(res: Response): void {
  sendError(res, 400, 'invalid_body', 'The request body is not valid for this action.');
}

function bodyTooLarge(req: Request): boolean {
  const declared = Number(req.headers['content-length']);
  if (Number.isFinite(declared) && declared > MAX_STATUS_BODY_BYTES) return true;
  try {
    return Buffer.byteLength(JSON.stringify(req.body ?? null), 'utf8') > MAX_STATUS_BODY_BYTES;
  } catch {
    return true;
  }
}

/** Origin, JSON, size and a plain object; null after answering. */
function postBody(req: Request, res: Response): Record<string, unknown> | null {
  if (req.originalUrl.includes('?')) {
    sendError(res, 400, 'unexpected_query', 'This request does not take a query string.');
    return null;
  }
  if (
    typeof req.headers.origin !== 'string' ||
    !req.headers.origin ||
    !isDashboardWebSocketOriginAllowed(req)
  ) {
    sendError(res, 403, 'origin_required', 'This change requires the dashboard origin.');
    return null;
  }
  if (!req.is('application/json')) {
    sendError(res, 415, 'json_required', 'This change requires application/json.');
    return null;
  }
  const body: unknown = req.body;
  if (bodyTooLarge(req)) {
    sendError(res, 413, 'body_too_large', 'The request body is too large.');
    return null;
  }
  if (!body || typeof body !== 'object' || Array.isArray(body)) {
    invalid(res);
    return null;
  }
  return body as Record<string, unknown>;
}

function entryView(entry: AnswerHistoryEntry) {
  return {
    id: entry.id,
    card: {
      id: entry.card.id,
      label: entry.card.label ?? null,
      seq: entry.card.seq,
      title: entry.card.title,
      project: entry.card.project,
      stream: entry.card.stream,
      askedBy: entry.card.askedBy,
      options: (entry.card.options ?? []).map((option) => ({
        key: option.key,
        label: option.label,
      })),
      recommendation: entry.card.recommendation,
      cardSha256: entry.answer.cardSha256,
    },
    answer: {
      kind: entry.answer.kind,
      choice: entry.answer.choice,
      note: entry.answer.note,
      via: entry.answer.via,
      answeredAt: entry.answer.answeredAt,
      device: entry.answer.device,
      delivery: deliveryView(entry),
    },
  };
}

const FILE_REFUSALS = {
  not_found: [404, 'This file is not available.'],
  too_large: [413, 'This file is larger than its limit (images 2 MB, videos 50 MB).'],
  not_media: [415, 'This file is not a PNG, JPEG or WebP image or an MP4 or WebM video.'],
} as const;

/** `no-store`, and a browser session only (the Status and Scoreboard routes share this). */
export function requireBrowserSession(req: Request, res: Response, next: NextFunction): void {
  res.setHeader('Cache-Control', 'no-store');
  const kind = authKind(req);
  if (kind === 'device') {
    sendError(res, 403, 'device_scope', 'This page needs a signed-in dashboard session.');
    return;
  }
  if (kind !== 'session') {
    sendError(res, 401, 'auth_required', 'Authentication required');
    return;
  }
  next();
}

export function createStatusRouter(service: () => StatusService = getStatusService): Router {
  const router = createApiRouter();
  router.use(requireBrowserSession);

  router.get('/', (req, res) => {
    if (req.originalUrl.includes('?')) {
      sendError(res, 400, 'unexpected_query', 'This request does not take a query string.');
      return;
    }
    res.json(service().view());
  });

  router.get('/answers', (req, res) => {
    const keys = Object.keys(req.query);
    const raw = req.query.limit;
    if (keys.some((key) => key !== 'limit') || (raw !== undefined && typeof raw !== 'string')) {
      sendError(res, 400, 'unexpected_query', 'Only limit is accepted here.');
      return;
    }
    let limit = 50;
    if (typeof raw === 'string') {
      if (!/^\d{1,4}$/.test(raw) || Number(raw) < 1 || Number(raw) > 500) {
        sendError(res, 400, 'invalid_limit', 'limit must be a whole number from 1 to 500.');
        return;
      }
      limit = Number(raw);
    }
    try {
      res.json({ answers: service().answers.history(limit).map(entryView) });
    } catch {
      sendError(res, 503, 'answers_unavailable', "The answer history can't be read right now.");
    }
  });

  router.post('/answers', async (req, res) => {
    const body = postBody(req, res);
    if (!body) return;
    const entries = body.answers;
    if (
      !hasOnlyKeys(body, ['answers'], ['sendId']) ||
      !Array.isArray(entries) ||
      entries.length === 0 ||
      entries.length > MAX_ANSWER_ENTRIES ||
      (body.sendId !== undefined && !isClientSendId(body.sendId)) ||
      !validEntryIds(entries)
    ) {
      invalid(res);
      return;
    }
    let results: EntryResult[];
    try {
      results = await service().answers.submitSet(
        entries,
        answerDevice(req.headers['user-agent']),
        typeof body.sendId === 'string' ? body.sendId : null
      );
    } catch {
      // submitSet answers per entry; this is what is left if it cannot. Some may be recorded.
      sendError(
        res,
        500,
        'answers_failed',
        'The answers could not be finished. Some may have been recorded; reload before sending again.'
      );
      return;
    }
    service().changed();
    res.json({ results });
  });

  router.get(/^\/files\/([^/]+)\/(.+)$/, async (req, res) => {
    if (req.originalUrl.includes('?')) {
      sendError(res, 400, 'unexpected_query', 'This request does not take a query string.');
      return;
    }
    const params = req.params as unknown as Record<string, string>;
    const media = await openCardFile(service().reader.root, params[0], params[1]);
    if (!media.ok) {
      const [status, error] = FILE_REFUSALS[media.reason];
      sendError(res, status, media.reason, error);
      return;
    }
    try {
      res.setHeader('X-Content-Type-Options', 'nosniff');
      res.setHeader('Content-Security-Policy', "default-src 'none'; sandbox");
      res.setHeader('Accept-Ranges', 'bytes');
      const range = parseByteRange(req.headers.range, media.size);
      if (range === 'unsatisfiable') {
        // A JSON refusal: the media type and the media's caching start below.
        res.setHeader('Content-Range', `bytes */${media.size}`);
        sendError(res, 416, 'range_not_satisfiable', 'The requested range is outside this file.');
        return;
      }
      res.setHeader('Cache-Control', 'private, max-age=300');
      res.setHeader('Content-Type', media.contentType);
      const start = range ? range.start : 0;
      const end = range ? range.end : media.size - 1;
      res.status(range ? 206 : 200);
      if (range) res.setHeader('Content-Range', `bytes ${start}-${end}/${media.size}`);
      res.setHeader('Content-Length', String(Math.max(0, end - start + 1)));
      if (req.method === 'HEAD' || media.size === 0) {
        res.end();
        return;
      }
      await sendFileBytes(media.handle, res, start, end);
    } finally {
      await media.handle.close().catch(() => undefined);
    }
  });

  // A path Express cannot decode (`%zz`) is a bad path: `404 not_found`, as the contract says.
  router.use((error: unknown, req: Request, res: Response, next: NextFunction) => {
    if (error instanceof URIError && req.path.startsWith('/files/') && !res.headersSent) {
      sendError(res, 404, 'not_found', FILE_REFUSALS.not_found[1]);
      return;
    }
    next(error);
  });

  return router;
}

export default createStatusRouter();
