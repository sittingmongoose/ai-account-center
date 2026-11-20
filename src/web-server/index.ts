import type { AccountDashboard, ClaudeDashboardPlatform } from './services/account-dashboard-types';
import { compositionGuard } from './compose-request';
/**
 * AI Account Center - Web Server
 *
 * Express server for the account dashboard and native account controls.
 * Single HTTP server handles REST API, static files, and WebSocket connections.
 * The same Leptos WebAssembly dashboard is served in development and production.
 */

import express from 'express';
import { RustService, type RustServiceMode } from './rust-service';
import http from 'http';
import type https from 'https';
import type { AddressInfo } from 'net';
import type { Duplex, Writable } from 'stream';
import path from 'path';
import { WebSocketServer } from 'ws';
import {
  authMiddleware,
  closeDashboardSessions,
  getDashboardWebSocketRejectionStatus,
  isApiRequestPath,
  isDashboardWebSocketUpgradeAllowed,
  openDashboardSessionMiddleware,
} from './middleware/auth-middleware';
import { requestLoggingMiddleware } from './middleware/request-logging-middleware';
import { createLogger } from '../services/logging';
import { DEFAULT_DASHBOARD_HOST, isLoopbackHost } from '../commands/config-dashboard-host';
import { ConfigError } from '../errors/error-types';
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
import { readPackagedBuild, recordRunningBuild } from './services/deployed-build';
import { getCcsDir } from '../utils/config-manager';
import { attachDashboardEventFeed, attachDashboardEventServer } from './dashboard-events';
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
import { readSnapshotSettings } from './agent-usage/agent-usage-settings';
import {
  closeAacStore,
  onStoreHostFallback,
  openAacStore,
  startAacStoreHostJobsWatch,
  startAacStoreMaintenance,
} from '../store/store-service';
import { USAGE_HUB_MOUNT } from './usage-hub/usage-hub-contract';
import { createUsageHubRouter } from './usage-hub/usage-hub-router';
import { startProcessBroker, stopProcessBroker } from '../utils/managed-process';
import { startEventLoopMonitor } from '../utils/event-loop-monitor';

export interface ServerOptions {
  port: number;
  /** M3 service routing; default off. Auth and endpoint guards stay in TS. */
  rustService?: RustServiceMode;
  /** Optional existing binary; never built or installed at startup. */
  rustServiceBinary?: string;
  /** Per-boot proof for a single loopback-only Rust composition companion. */
  compositionProof?: string;
  /** Start the private cut worker without public-only TS listeners or deployment bookkeeping. */
  sidecar?: boolean;
  /** Publishes an admitted Rust-held account snapshot to the usage hub. */
  onRustDashboard?: (platform: ClaudeDashboardPlatform, dashboard: AccountDashboard) => void;
  /** Optional inherited stdin of the Rust public dashboard event receiver. */
  dashboardEventFeed?: Writable;
  host?: string;
  staticDir?: string;
  dev?: boolean;
  /** Explicit verified Ubuntu driver composition; omission keeps the legacy usage-only source. */
}

export interface ServerInstance {
  server: http.Server;
  wss: WebSocketServer;
  cleanup: () => void;
  publicAddress?: AddressInfo;
  /** Private test/diagnostic counters; never exposed by an HTTP route. */
  rustService: RustService;
  /** The optional in-process HTTPS listener (`dashboard_tls.https_listener`); null when off. */
  httpsServer?: https.Server | null;
  /** Bound public HTTPS address for either listener implementation. */
  httpsAddress: AddressInfo | null;
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
  if (
    (options.sidecar && !options.compositionProof) ||
    (options.compositionProof &&
      (options.host !== '127.0.0.1' ||
        options.port !== 0 ||
        !/^[0-9a-f]{64}$/.test(options.compositionProof)))
  )
    throw new ConfigError('The composition backend requires an ephemeral loopback listener.');
  const app = express();
  // Routes answer only to their canonical spelling; set before the first app.use.
  app.set('case sensitive routing', true);
  // Express 5 defaults to the simple query parser. Keep the Express 4 parser so
  // repeated and bracketed query keys stay the same shape.
  app.set('query parser', 'extended');
  // The stack's name and version in every answer help no legitimate client.
  app.disable('x-powered-by');
  if (options.compositionProof) app.use(compositionGuard(options.compositionProof));
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
        res.status(400).json({ error: 'Invalid JSON in request body', code: 'invalid_json' });
        return;
      }
      // body-parser's own 413 would otherwise reach Express's HTML error page.
      if (err.type === 'entity.too.large') {
        if (sendAuthPathError(req, res, 413, 'body_too_large', 'Request body is too large.'))
          return;
        res.status(413).json({ error: 'Request body is too large.', code: 'body_too_large' });
        return;
      }
      next(err);
    }
  );

  // The packaged UI location: needed by the static-asset skip below and by
  // the static handlers further down.
  const staticDir = options.staticDir || path.join(__dirname, '../ui');
  const staticUi = loadStaticUi(staticDir);
  // A reused UI bundle names the commit it was built at, not the running server's.
  const packagedBuild = readPackagedBuild();
  setDashboardBuildCommit(packagedBuild?.commit.slice(0, 8) ?? staticUi.commit);

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
    requestLoggingMiddleware(req, res, next, !!options.compositionProof);
  });

  // Session middleware (for dashboard auth)
  const sessionMiddleware = options.compositionProof
    ? (_req: express.Request, _res: express.Response, next: express.NextFunction) => next()
    : await openDashboardSessionMiddleware();
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
  const rustService = new RustService(options.rustService);
  const rustServiceReady = rustService.start(options.rustServiceBinary);
  app.use('/api', (_req, res, next) => {
    res.locals.rustService = rustService;
    if (options.onRustDashboard) res.locals.onRustDashboard = options.onRustDashboard;
    next();
  });
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
  if (!options.sidecar) {
    app.use(USAGE_HUB_MOUNT, createUsageHubRouter());
    app.use(precompressedStatic(staticUi));
    app.use(express.static(staticDir, { index: false, setHeaders: uiStaticHeaders(staticUi) }));
    app.get('/{*splat}', pageRouteHandler(staticUi, new Set(DASHBOARD_PROVIDER_IDS)));
  }
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
    if (options.compositionProof) {
      rejectWebSocketUpgrade(socket, 404, 'WebSocket endpoint not found');
      return;
    }
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
  let stopStoreMaintenance: (() => void) | null = null;
  let stopStoreHostJobsWatch: (() => void) | null = null;
  let stopStoreFallbackWatch: (() => void) | null = null;
  // Start the small command parent before analytics/collectors populate the server heap.
  await startProcessBroker().catch(() => {
    logger.warn('server.command_transport_unavailable', 'Private command transport unavailable');
  });
  const stopEventLoopMonitor = startEventLoopMonitor(logger);
  let cleaned = false;

  // Account changes and sign-in jobs reach /ws clients as hints. A job goes only
  // to browser sessions, and its code only to those that connected over a
  // secure transport. The upgrade request carries the session (see above).
  const detachDashboardEvents = attachDashboardEventServer(wss, {
    isSecure: isSecureTransport,
    authKind: (request) => authKind(request as express.Request),
    sessionEpoch: (request) => (request as express.Request).session?.epoch ?? null,
  });

  const detachRustEvents = options.dashboardEventFeed
    ? attachDashboardEventFeed(options.dashboardEventFeed)
    : undefined;

  // Combined cleanup function
  const cleanup = () => {
    if (cleaned) return;
    cleaned = true;
    stopStoreFallbackWatch?.();
    stopStoreFallbackWatch = null;
    stopEventLoopMonitor();
    httpsServer?.close();
    detachDashboardEvents();
    detachRustEvents?.();
    stopAccountLifecycleMaintenance();
    stopStoreMaintenance?.();
    stopStoreMaintenance = null;
    stopStoreHostJobsWatch?.();
    stopStoreHostJobsWatch = null;
    // R3: producers stop above. Drain admitted calls, close the sole writer, then stop
    // the service. The socket handle refuses new work once closing starts.
    void closeAacStore().finally(() => rustService.stop());
    wss.clients.forEach((client) => client.close(1001, 'Server shutting down'));
    void stopProcessBroker();
    // Saves the sign-ins and stops the session store's timers (the shutdown handlers await it).
    void closeDashboardSessions();
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
      // Only a build that is actually serving becomes the one installers must keep.
      if (!options.sidecar && recordRunningBuild(getCcsDir(), packagedBuild) > 0)
        logger.warn('web.deployed_build.unrecorded', 'Running build could not be recorded');
      // The history store (docs/store.md): opened before the first sample, which waits for it;
      // without the native module the dashboard runs as before, without stored history.
      // R3: while the service hosts the store, its jobs run there; this loop starts only
      // for the in-process store, including after the automatic fallback. While hosted,
      // the jobs watch carries the host's failure warnings instead (R3 review L1).
      const startStoreMaintenanceOnce = (): void => {
        stopStoreHostJobsWatch?.();
        stopStoreHostJobsWatch = null;
        stopStoreMaintenance ??= startAacStoreMaintenance({
          snapshotDirectory: async () => (await readSnapshotSettings())?.directory ?? null,
        });
      };
      stopStoreFallbackWatch = onStoreHostFallback(startStoreMaintenanceOnce);
      void openAacStore().then((opened) => {
        if (opened.store && !opened.hosted) startStoreMaintenanceOnce();
        else if (opened.store && opened.hosted)
          stopStoreHostJobsWatch ??= startAacStoreHostJobsWatch();
        else if (
          !opened.store &&
          opened.reason &&
          opened.reason !== 'missing' &&
          opened.reason !== 'disabled'
        )
          logger.warn('store.unavailable', 'The history store is unavailable', {
            reason: opened.reason,
          });
      });
      // Staging folders of sign-ins from before a restart, and trash past 30 days.
      if (
        !rustService.lifecycleInPrivate &&
        (!options.sidecar || process.env.AAC_CUT_PUBLIC_LIFECYCLE !== '1')
      ) {
        startAccountLifecycleMaintenance(rustService.writesOn ? rustServiceReady : undefined);
      }
      // First run with sign-in on and no password: this run's one-time setup code.
      if (!options.compositionProof) void prepareFirstRunSetupCode();
      // The optional HTTPS listener (dashboard_tls.https_listener, off by default).
      const https = options.compositionProof
        ? Promise.resolve(null)
        : startDashboardHttpsListener(app, listenHost, onUpgrade);
      void https
        .catch(() => null)
        .then((started) => {
          httpsServer = started;
          const bound = started?.address();
          const httpsAddress = typeof bound === 'object' ? bound : null;
          resolve({ server, wss, cleanup, httpsServer: started, httpsAddress, rustService });
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
