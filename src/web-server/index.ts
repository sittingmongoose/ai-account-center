/**
 * AI Account Center - Web Server
 *
 * Express server for the account dashboard and native account controls.
 * Single HTTP server handles REST API, static files, and WebSocket connections.
 * The same Slint WebAssembly dashboard is served in development and production.
 */

import express from 'express';
import http from 'http';
import type https from 'https';
import type { AddressInfo } from 'net';
import type { Duplex } from 'stream';
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
  startAntigravityRuntime,
  stopAntigravityRuntime,
  type AntigravityRuntimeFactory,
} from '../antigravity/runtime-service';
import { getInstalledAntigravityRuntimeFactory } from '../antigravity/production-runtime';
import {
  isStaticUiFileRequest,
  loadStaticUi,
  pageRouteHandler,
  precompressedStatic,
  uiStaticHeaders,
} from './static-ui';
import { apiCompression } from './api-compression';
import { DASHBOARD_PROVIDER_IDS } from './services/dashboard-provider-table';
import { setDashboardBuildCommit } from './services/dashboard-server-info';
import { attachDashboardEventServer } from './dashboard-events';
import { isSecureTransport } from './middleware/secure-transport';
import {
  configureDashboardTransport,
  prepareFirstRunSetupCode,
  sendAuthPathError,
  startDashboardHttpsListener,
} from './dashboard-auth-runtime';
import { authKind } from './middleware/request-auth';
import {
  startAccountLifecycleMaintenance,
  stopAccountLifecycleMaintenance,
} from './services/account-lifecycle-runtime';
import { startCgroupForeignCheck } from './services/cgroup-foreign-check';
import { USAGE_HUB_MOUNT } from './usage-hub/usage-hub-contract';
import { createUsageHubRouter } from './usage-hub/usage-hub-router';

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
  /** The optional in-process HTTPS listener (`dashboard_tls.https_listener`); null when off. */
  httpsServer?: https.Server | null;
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
  // The stack's name and version in every answer help no legitimate client.
  app.disable('x-powered-by');
  // Trusted TLS proxy hop (off unless dashboard_tls.trusted_proxy is set).
  configureDashboardTransport(app);
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
      req: express.Request,
      res: express.Response,
      next: express.NextFunction
    ) => {
      if (err instanceof SyntaxError && err.status === 400 && 'body' in err) {
        if (sendAuthPathError(req, res, 400, 'invalid_json', 'Invalid JSON in request body'))
          return;
        res.status(400).json({ error: 'Invalid JSON in request body' });
        return;
      }
      // body-parser's own 413 would otherwise reach Express's HTML error page.
      if (err.type === 'entity.too.large') {
        if (sendAuthPathError(req, res, 413, 'body_too_large', 'Request body is too large.'))
          return;
        res.status(413).json({ error: 'Request body is too large.' });
        return;
      }
      next(err);
    }
  );

  // The packaged UI location: needed by the static-asset skip below and by
  // the static handlers further down.
  const staticDir = options.staticDir || path.join(__dirname, '../ui');
  const staticUi = loadStaticUi(staticDir);
  setDashboardBuildCommit(staticUi.commit);

  // A static-asset GET/HEAD carries no session state and its log line is
  // noise: one stat (express.static stats again anyway) skips both. Page and
  // /api requests keep the audit line and the session, whatever the method.
  app.use((req, res, next) => {
    const staticAsset =
      (req.method === 'GET' || req.method === 'HEAD') &&
      !isApiRequestPath(req.path) &&
      isStaticUiFileRequest(staticUi.root, req.path);
    res.locals.staticAssetRequest = staticAsset;
    if (staticAsset) {
      next();
      return;
    }
    requestLoggingMiddleware(req, res, next);
  });

  // Session middleware (for dashboard auth)
  const sessionMiddleware = createSessionMiddleware();
  app.use((req, res, next) => {
    if (res.locals.staticAssetRequest === true) {
      next();
      return;
    }
    sessionMiddleware(req, res, next);
  });

  // Auth middleware (protects API routes when enabled)
  app.use(authMiddleware);

  // REST API routes (modularized); big JSON answers are encoded on the way out.
  app.use('/api', apiCompression());
  const { apiRoutes } = await import('./routes/index');
  app.use('/api', apiRoutes);
  // Any /api request no router answered, in any method or letter case, is a JSON 404;
  // it never reaches the static files or the page fallback below.
  app.use((req, res, next) => {
    if (!isApiRequestPath(req.path)) {
      next();
      return;
    }
    if (sendAuthPathError(req, res, 404, 'not_found', 'API endpoint was not found.')) return;
    res.status(404).json({ error: 'API endpoint was not found.' });
  });

  // The T3 usage hub: read-only CLIProxyAPI-shaped usage from the dashboard's
  // cache, off until a key is set (ai-account-center dashboard usage-hub).
  app.use(USAGE_HUB_MOUNT, createUsageHubRouter());

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

  const onUpgrade = (request: http.IncomingMessage, socket: Duplex, head: Buffer): void => {
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
  };
  server.on('upgrade', onUpgrade);
  let httpsServer: https.Server | null = null;
  let stopCgroupForeignCheck: (() => void) | null = null;

  const codexAutoSwitch = getCodexAutoSwitchService();
  // Account changes and sign-in jobs reach /ws clients as hints. A job goes only
  // to browser sessions, and its code only to those that connected over a
  // secure transport. The upgrade request carries the session (see above).
  const detachDashboardEvents = attachDashboardEventServer(wss, {
    isSecure: isSecureTransport,
    authKind: (request) => authKind(request as express.Request),
    sessionEpoch: (request) => (request as express.Request).session?.epoch ?? null,
  });

  // Combined cleanup function
  const cleanup = () => {
    httpsServer?.close();
    detachDashboardEvents();
    stopAccountLifecycleMaintenance();
    codexAutoSwitch.stop();
    stopAntigravityRuntime();
    stopAccountAnalyticsSampling();
    stopCgroupForeignCheck?.();
    stopCgroupForeignCheck = null;
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
          startAntigravityRuntime();
        } catch {
          logger.error(
            'antigravity.runtime_unavailable',
            'Antigravity switching could not initialize safely'
          );
        }
      }
      codexAutoSwitch.start();
      startAccountAnalyticsSampling();
      // Warn when launched apps leaked into the dashboard cgroup (read-only).
      stopCgroupForeignCheck = startCgroupForeignCheck();
      // Staging folders of sign-ins from before a restart, and trash past 30 days.
      startAccountLifecycleMaintenance();
      // Usage cache loads on-demand when Analytics page is visited
      // This keeps server startup instant for users who don't need analytics
      // First run with sign-in on and no password: this run's one-time setup code.
      void prepareFirstRunSetupCode();
      // The optional HTTPS listener (dashboard_tls.https_listener, off by default).
      void startDashboardHttpsListener(app, listenHost, onUpgrade)
        .catch(() => null)
        .then((started) => {
          httpsServer = started;
          resolve({ server, wss, cleanup, httpsServer: started });
        });
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
