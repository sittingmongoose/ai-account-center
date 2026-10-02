/**
 * AI Account Center - Web Server
 *
 * Express server for the account dashboard and native account controls.
 * Single HTTP server handles REST API, static files, and WebSocket connections.
 * The same Slint WebAssembly dashboard is served in development and production.
 */

import express from 'express';
import http from 'http';
import type { AddressInfo } from 'net';
import path from 'path';
import { WebSocketServer } from 'ws';
import {
  authMiddleware,
  createSessionMiddleware,
  getDashboardWebSocketRejectionStatus,
  isApiRequestPath,
  isDashboardWebSocketUpgradeAllowed,
} from './middleware/auth-middleware';
import { requestLoggingMiddleware } from './middleware/request-logging-middleware';
import { shutdownUsageAggregator } from './usage/aggregator';
import { createLogger } from '../services/logging';
import { DEFAULT_DASHBOARD_HOST, isLoopbackHost } from '../commands/config-dashboard-host';
import { getCodexAutoSwitchService } from './services/codex-auto-switch-service';
import {
  startAccountAnalyticsSampling,
  stopAccountAnalyticsSampling,
} from './services/account-analytics-service';
import { ConfigError } from '../errors/error-types';
import {
  configureAntigravityRuntimeFactory,
  getAntigravityRuntime,
  type AntigravityRuntimeFactory,
} from '../antigravity/runtime-service';
import type { AntigravityRuntime } from '../antigravity/runtime-composition';
import { getInstalledAntigravityRuntimeFactory } from '../antigravity/production-runtime';
import { loadStaticUi, pageRouteHandler, precompressedStatic, uiStaticHeaders } from './static-ui';
import { DASHBOARD_PROVIDER_IDS } from './services/dashboard-provider-table';
import { setDashboardBuildCommit } from './services/dashboard-server-info';
import { attachDashboardEventServer } from './dashboard-events';
import { isSecureTransport } from './middleware/secure-transport';
import {
  startAccountLifecycleMaintenance,
  stopAccountLifecycleMaintenance,
} from './services/account-lifecycle-runtime';

export interface ServerOptions {
  port: number;
  host?: string;
  staticDir?: string;
  dev?: boolean;
  /** Explicit verified Ubuntu driver composition; omission keeps the legacy usage-only source. */
  antigravityRuntimeFactory?: AntigravityRuntimeFactory;
}

export interface ServerInstance {
  server: http.Server;
  wss: WebSocketServer;
  cleanup: () => void;
}

function getListenHost(options: ServerOptions): string {
  return options.host || DEFAULT_DASHBOARD_HOST;
}

const logger = createLogger('web-server');

/**
 * A short error class for the log: a body-parser type, a system code or the
 * error's class name, never its message.
 */
export function requestErrorKind(error: unknown): string {
  const candidate = error as { type?: unknown; code?: unknown; name?: unknown } | null;
  for (const value of [candidate?.type, candidate?.code, candidate?.name]) {
    if (typeof value === 'string' && /^[A-Za-z][A-Za-z0-9_.-]{0,63}$/.test(value)) return value;
  }
  return 'unknown';
}

/** The final handler's trace: status and error class only, no message, stack or path. */
function logRequestFailure(status: number, error: unknown, headersSent: boolean): void {
  try {
    logger[status >= 500 ? 'error' : 'warn']('web.request.failed', 'Dashboard request failed', {
      status,
      kind: requestErrorKind(error),
      headersSent,
    });
  } catch {
    /* Logging never changes the answer. */
  }
}

/**
 * Start Express server with WebSocket support
 */
export async function startServer(options: ServerOptions): Promise<ServerInstance> {
  const app = express();
  // Routes answer only to their canonical spelling; set before the first app.use.
  app.set('case sensitive routing', true);
  const server = http.createServer(app);
  const wss = new WebSocketServer({
    noServer: true,
    maxPayload: 1024 * 1024, // 1MB hard limit to prevent DoS
    perMessageDeflate: false, // Prevent zip bomb attacks
  });

  // JSON body parsing with error handler for malformed JSON
  app.use(express.json());
  app.use(
    (
      err: Error & { status?: number; body?: string; type?: string },
      _req: express.Request,
      res: express.Response,
      next: express.NextFunction
    ) => {
      if (err instanceof SyntaxError && err.status === 400 && 'body' in err) {
        res.status(400).json({ error: 'Invalid JSON in request body' });
        return;
      }
      // body-parser's own 413 would otherwise reach Express's HTML error page.
      if (err.type === 'entity.too.large') {
        res.status(413).json({ error: 'Request body is too large.' });
        return;
      }
      next(err);
    }
  );
  app.use(requestLoggingMiddleware);

  // Session middleware (for dashboard auth)
  const sessionMiddleware = createSessionMiddleware();
  app.use(sessionMiddleware);

  // Auth middleware (protects API routes when enabled)
  app.use(authMiddleware);

  // REST API routes (modularized)
  const { apiRoutes } = await import('./routes/index');
  app.use('/api', apiRoutes);
  // Any /api request no router answered, in any method or letter case, is a JSON 404;
  // it never reaches the static files or the page fallback below.
  app.use((req, res, next) => {
    if (!isApiRequestPath(req.path)) {
      next();
      return;
    }
    res.status(404).json({ error: 'API endpoint was not found.' });
  });

  const staticDir = options.staticDir || path.join(__dirname, '../ui');
  const staticUi = loadStaticUi(staticDir);
  setDashboardBuildCommit(staticUi.commit);
  app.use(precompressedStatic(staticUi));
  app.use(
    express.static(staticDir, {
      // '/' is a page route below, with the page headers.
      index: false,
      setHeaders: uiStaticHeaders(staticUi),
    })
  );
  // Slint owns the account dashboard and login state; no React portal is served.
  app.get('*', pageRouteHandler(staticUi, new Set(DASHBOARD_PROVIDER_IDS)));
  // Last: whatever failed above answers in JSON, never with a stack trace.
  app.use(
    (
      err: Error & { status?: number; statusCode?: number },
      _req: express.Request,
      res: express.Response,
      next: express.NextFunction
    ) => {
      const raw = err?.status ?? err?.statusCode;
      const status =
        typeof raw === 'number' && Number.isInteger(raw) && raw >= 400 && raw < 500 ? raw : 500;
      logRequestFailure(status, err, res.headersSent);
      if (res.headersSent) {
        next(err);
        return;
      }
      if (status < 500) {
        res.status(status).json({ error: 'The request could not be processed.' });
        return;
      }
      res.status(500).json({ error: 'The request could not be completed safely.' });
    }
  );

  server.on('upgrade', (request, socket, head) => {
    const pathname = getUpgradePathname(request.url);
    if (!pathname) {
      rejectWebSocketUpgrade(socket, 400, 'Invalid WebSocket upgrade request');
      return;
    }

    if (pathname !== '/ws') {
      rejectWebSocketUpgrade(socket, 404, 'WebSocket endpoint not found');
      return;
    }

    const response = new http.ServerResponse(request);
    sessionMiddleware(
      request as express.Request,
      response as express.Response,
      (error?: unknown) => {
        if (error) {
          rejectWebSocketUpgrade(socket, 500, 'WebSocket session validation failed');
          return;
        }

        if (!isDashboardWebSocketUpgradeAllowed(request)) {
          rejectWebSocketUpgrade(
            socket,
            getDashboardWebSocketRejectionStatus(request),
            'WebSocket access denied'
          );
          return;
        }

        wss.handleUpgrade(request, socket, head, (ws) => {
          wss.emit('connection', ws, request);
        });
      }
    );
  });

  const codexAutoSwitch = getCodexAutoSwitchService();
  let antigravityRuntime: AntigravityRuntime | null = null;
  // Account changes and sign-in jobs reach /ws clients as hints; a job's code
  // goes only to sockets that connected over a secure transport.
  const detachDashboardEvents = attachDashboardEventServer(wss, { isSecure: isSecureTransport });

  // Combined cleanup function
  const cleanup = () => {
    detachDashboardEvents();
    stopAccountLifecycleMaintenance();
    codexAutoSwitch.stop();
    antigravityRuntime?.stop();
    stopAccountAnalyticsSampling();
    wss.clients.forEach((client) => client.close(1001, 'Server shutting down'));
    shutdownUsageAggregator();
  };
  server.once('close', cleanup);

  // Start listening
  return new Promise<ServerInstance>((resolve, reject) => {
    const listenHost = getListenHost(options);
    const onError = (error: NodeJS.ErrnoException) => {
      logger.error('server.listen_failed', 'Dashboard server failed to start', {
        code: error.code || 'unknown',
        message: error.message,
        host: listenHost,
        port: options.port,
      });
      cleanup();
      reject(new Error(formatListenError(error, options)));
    };

    server.once('error', onError);

    const onListening = () => {
      server.off('error', onError);
      try {
        assertSafeDashboardBind(options, server.address());
      } catch (error) {
        cleanup();
        server.close(() => {
          reject(error instanceof Error ? error : new Error(String(error)));
        });
        return;
      }

      logger.info('server.listening', 'Dashboard server listening', {
        host: listenHost,
        port: options.port,
        dev: Boolean(options.dev),
      });
      const antigravityFactory =
        options.antigravityRuntimeFactory ?? getInstalledAntigravityRuntimeFactory();
      if (antigravityFactory) {
        try {
          configureAntigravityRuntimeFactory(antigravityFactory);
          antigravityRuntime = getAntigravityRuntime();
          antigravityRuntime?.start();
        } catch {
          logger.error(
            'antigravity.runtime_unavailable',
            'Antigravity switching could not initialize safely'
          );
        }
      }
      codexAutoSwitch.start();
      startAccountAnalyticsSampling();
      // Staging folders of sign-ins from before a restart, and trash past 30 days.
      startAccountLifecycleMaintenance();
      // Usage cache loads on-demand when Analytics page is visited
      // This keeps server startup instant for users who don't need analytics
      resolve({ server, wss, cleanup });
    };

    try {
      server.listen(options.port, listenHost, onListening);
    } catch (error) {
      server.off('error', onError);
      cleanup();
      reject(new Error(formatListenError(error as NodeJS.ErrnoException, options)));
    }
  });
}

function getUpgradePathname(requestUrl: string | undefined): string | null {
  try {
    return new URL(requestUrl ?? '/', 'http://localhost').pathname;
  } catch {
    return null;
  }
}

function rejectWebSocketUpgrade(
  socket: NodeJS.WritableStream & { destroy: () => void },
  statusCode: 400 | 401 | 403 | 404 | 500,
  message: string
): void {
  socket.write(
    `HTTP/1.1 ${statusCode} ${message}\r\n` +
      'Connection: close\r\n' +
      'Content-Type: text/plain; charset=utf-8\r\n' +
      `Content-Length: ${Buffer.byteLength(message)}\r\n` +
      '\r\n' +
      message
  );
  socket.destroy();
}

function assertSafeDashboardBind(
  options: ServerOptions,
  address: string | AddressInfo | null
): void {
  const listenHost = getListenHost(options);

  if (!isLoopbackHost(listenHost) || typeof address === 'string' || !address) {
    return;
  }

  if (isLoopbackHost(address.address)) {
    return;
  }

  throw new ConfigError(
    `Dashboard host ${listenHost} resolved to non-loopback address ${address.address}; pass --host explicitly to allow network exposure.`
  );
}

function formatListenError(error: NodeJS.ErrnoException, options: ServerOptions): string {
  const listenHost = getListenHost(options);

  if (error.code === 'EADDRINUSE') {
    return `Unable to bind ${listenHost}:${options.port}; the address may be unavailable or the port may already be in use`;
  }

  if (error.code === 'EADDRNOTAVAIL') {
    return `Cannot bind to ${listenHost}:${options.port} on this machine`;
  }

  if (error.code === 'EACCES') {
    return `Permission denied while binding to port ${options.port}`;
  }

  return `Cannot bind to ${listenHost}:${options.port}: ${error.message}`;
}
