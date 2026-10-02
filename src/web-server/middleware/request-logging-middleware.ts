import { randomUUID } from 'crypto';
import type { Request, Response, NextFunction } from 'express';
import { createLogger, withRequestContext } from '../../services/logging';

const logger = createLogger('web-server:http');

const TOKEN_IN_URL = /aacd_[A-Za-z0-9_%-]{8,}/g;
const REDACTED_TOKEN = 'aacd_[redacted]';

/** Percent-decodes ASCII escapes only; never throws on a malformed sequence. */
function decodeAsciiEscapes(value: string): string {
  return value.replace(/%([0-7][0-9A-Fa-f])/g, (_match, hex: string) =>
    String.fromCharCode(Number.parseInt(hex, 16))
  );
}

/**
 * A device token sent in a URL is refused (400 `token_in_query`), but the URL
 * is still logged; its token never is, whatever the redaction setting
 * (CONTRACT-auth-devices section 6). An encoded token (`%61acd_...`, even
 * encoded twice) is found in the decoded URL, and then the decoded URL is
 * logged with the token replaced.
 */
export function scrubLoggedUrl(url: string): string {
  const scrubbed = url.replace(TOKEN_IN_URL, REDACTED_TOKEN);
  let decoded = scrubbed;
  for (let pass = 0; pass < 3; pass += 1) {
    const next = decodeAsciiEscapes(decoded);
    if (next === decoded) break;
    decoded = next;
  }
  if (decoded === scrubbed) return scrubbed;
  const redacted = decoded.replace(TOKEN_IN_URL, REDACTED_TOKEN);
  return redacted === decoded ? scrubbed : redacted;
}

export function requestLoggingMiddleware(req: Request, res: Response, next: NextFunction): void {
  const requestId = randomUUID();
  const startTime = Date.now();
  res.locals.ccsRequestId = requestId;
  res.setHeader('x-ccs-request-id', requestId);
  const shouldSkipLogging = req.originalUrl.startsWith('/api/logs');

  res.on('finish', () => {
    if (shouldSkipLogging) {
      return;
    }
    withRequestContext({ requestId }, () => {
      logger.info('request.completed', 'Dashboard request completed', {
        requestId,
        method: req.method,
        path: scrubLoggedUrl(req.originalUrl),
        statusCode: res.statusCode,
        durationMs: Date.now() - startTime,
        remoteAddress: req.socket.remoteAddress || null,
        userAgent: req.headers['user-agent'] || null,
      });
    });
  });

  // Wrap the downstream handler chain so structured logs emitted by route
  // handlers carry the requestId (the logger auto-attaches it from the active
  // request context). Mirrors src/proxy/server/proxy-server.ts.
  withRequestContext({ requestId }, () => next());
}
