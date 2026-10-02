import { randomUUID } from 'crypto';
import type { Request, Response, NextFunction } from 'express';
import { createLogger, withRequestContext } from '../../services/logging';

const logger = createLogger('web-server:http');

/**
 * A device token sent in a URL is refused (400 `token_in_query`), but the URL
 * is still logged; its token never is, whatever the redaction setting
 * (CONTRACT-auth-devices section 6).
 */
export function scrubLoggedUrl(url: string): string {
  return url.replace(/aacd_[A-Za-z0-9_%-]{8,}/g, 'aacd_[redacted]');
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
