import type { Request, Response, Router } from 'express';
import { createApiRouter } from './api-router';
import { hasOnlyKeys, sendError } from './account-route-guards';
import { isDashboardWebSocketOriginAllowed } from '../middleware/auth-middleware';
import { authKind } from '../middleware/request-auth';
import { getStatusService, type StatusService } from '../status/status-service';
import {
  answerDevice,
  deliveryView,
  type AnswerHistoryEntry,
  type AnswerInput,
  type BatchInput,
} from '../status/answers';
import { isCardId } from '../status/report-schema';

/**
 * The Status tab API (docs/status-tab.md):
 * - `GET /api/status`: the whole view;
 * - `GET /api/status/answers?limit=N`: the owner's answer history, newest first;
 * - `POST /api/status/answers`: one answer;
 * - `POST /api/status/answers/batch`: "approve all, except ...".
 * Every route: a browser session only (never a device token, never localhost without sign-in),
 * `Cache-Control: no-store`. POSTs: no query string, the dashboard Origin, application/json and a
 * strict body of at most 16 KB. Errors are `{ error, code }` with fixed sentences.
 */
export const MAX_STATUS_BODY_BYTES = 16 * 1024;
const SHA256 = /^[0-9a-f]{64}$/;
const BATCH_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

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

function isSeq(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function isKeyList(value: unknown, max: number): value is string[] {
  return (
    Array.isArray(value) &&
    value.length <= max &&
    value.every((key) => typeof key === 'string' && key.length > 0 && key.length <= 32)
  );
}

/** The strict shape of one answer, or null. */
export function parseAnswerBody(body: Record<string, unknown>): AnswerInput | null {
  if (!hasOnlyKeys(body, ['cardId', 'cardSeq', 'cardSha256', 'choice'], ['note'])) return null;
  const { cardId, cardSeq, cardSha256, choice, note } = body;
  if (!isCardId(cardId) || !isSeq(cardSeq)) return null;
  if (typeof cardSha256 !== 'string' || !SHA256.test(cardSha256)) return null;
  if (!isKeyList(choice, 12)) return null;
  if (note !== undefined && typeof note !== 'string') return null;
  return { cardId, cardSeq, cardSha256, choice, ...(note !== undefined ? { note } : {}) };
}

/** The strict shape of a batch, or null. */
export function parseBatchBody(body: Record<string, unknown>): BatchInput | null {
  if (!hasOnlyKeys(body, ['batch', 'cards', 'except'])) return null;
  const { batch, cards, except } = body;
  if (typeof batch !== 'string' || !BATCH_ID.test(batch)) return null;
  if (!Array.isArray(cards) || cards.length === 0 || cards.length > 50) return null;
  const parsed: BatchInput['cards'] = [];
  for (const card of cards) {
    if (!card || typeof card !== 'object' || Array.isArray(card)) return null;
    const row = card as Record<string, unknown>;
    if (!hasOnlyKeys(row, ['id', 'seq', 'cardSha256'])) return null;
    if (!isCardId(row.id) || !isSeq(row.seq)) return null;
    if (typeof row.cardSha256 !== 'string' || !SHA256.test(row.cardSha256)) return null;
    parsed.push({ id: row.id, seq: row.seq, cardSha256: row.cardSha256 });
  }
  if (new Set(parsed.map((card) => card.id)).size !== parsed.length) return null;
  if (!Array.isArray(except) || except.length > 50 || !except.every((id) => isCardId(id)))
    return null;
  return { batch, cards: parsed, except: except as string[] };
}

function entryView(entry: AnswerHistoryEntry) {
  return {
    id: entry.id,
    card: {
      id: entry.card.id,
      seq: entry.card.seq,
      title: entry.card.title,
      project: entry.card.project,
      stream: entry.card.stream,
      askedBy: entry.card.askedBy,
      options: entry.card.options,
      recommendation: entry.card.recommendation,
      cardSha256: entry.answer.cardSha256,
    },
    answer: {
      choice: entry.answer.choice,
      note: entry.answer.note,
      via: entry.answer.via,
      batchAnswer: entry.answer.batchAnswer ?? null,
      answeredAt: entry.answer.answeredAt,
      device: entry.answer.device,
      delivery: deliveryView(entry),
    },
  };
}

export function createStatusRouter(service: () => StatusService = getStatusService): Router {
  const router = createApiRouter();
  router.use((req, res, next) => {
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
  });

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
    res.json({ answers: service().answers.history(limit).map(entryView) });
  });

  router.post('/answers', async (req, res) => {
    const body = postBody(req, res);
    if (!body) return;
    const input = parseAnswerBody(body);
    if (!input) {
      invalid(res);
      return;
    }
    const outcome = await service().answers.submit(input, answerDevice(req.headers['user-agent']));
    if (!outcome.ok) {
      sendError(res, outcome.status, outcome.code, outcome.error);
      return;
    }
    service().changed();
    res.json({ answer: entryView(outcome.entry) });
  });

  router.post('/answers/batch', async (req, res) => {
    const body = postBody(req, res);
    if (!body) return;
    const input = parseBatchBody(body);
    if (!input) {
      invalid(res);
      return;
    }
    const result = await service().answers.submitBatch(
      input,
      answerDevice(req.headers['user-agent'])
    );
    service().changed();
    res.json({
      batchAnswer: result.batchAnswer,
      results: result.results.map((row) => ({
        id: row.id,
        outcome: row.outcome,
        ...(row.entry ? { answer: entryView(row.entry) } : {}),
      })),
    });
  });

  return router;
}

export default createStatusRouter();
