/**
 * Shutdown Handler
 *
 * Handles SIGINT/SIGTERM, awaiting private workers and persisted state within
 * the dashboard's bounded shutdown deadline before exiting.
 */

import { Server as HTTPServer } from 'http';
import { WebSocketServer } from 'ws';
import { ok } from '../utils/ui';
import { closeDashboardSessions } from './middleware/auth-middleware';
import { closeAacStore } from '../store/store-service';
import { stopProcessBroker } from '../utils/managed-process';

/**
 * Setup shutdown handlers for SIGINT and SIGTERM
 */
export function setupGracefulShutdown(
  server: HTTPServer,
  _wss: WebSocketServer,
  cleanup?: () => void
): void {
  let shuttingDown = false;
  const shutdown = () => {
    if (shuttingDown) return;
    shuttingDown = true;
    console.log('\n' + ok('Shutting down...'));

    // Run cleanup (closes file watchers + WebSocket clients)
    if (cleanup) {
      cleanup();
    }

    // Cleanup may start broker teardown without awaiting it. Include the same idempotent
    // promise here so process.exit cannot cancel a retired broker's SIGKILL deadline.
    // Store and broker closure share the existing 3 s bound; sign-ins wait at most 2 s.
    server.close();
    const storeAndBrokerClosed = Promise.race([
      Promise.allSettled([closeAacStore(), stopProcessBroker()]),
      new Promise<void>((resolve) => setTimeout(resolve, 3000).unref?.()),
    ]);
    void Promise.allSettled([closeDashboardSessions(), storeAndBrokerClosed]).finally(() =>
      process.exit(0)
    );
  };

  process.on('SIGINT', shutdown);
  process.on('SIGTERM', shutdown);
}
