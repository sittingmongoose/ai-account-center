/**
 * Shutdown Handler
 *
 * Handles SIGINT/SIGTERM signals to close server and exit immediately.
 * No graceful waiting - config dashboard doesn't need it.
 */

import { Server as HTTPServer } from 'http';
import { WebSocketServer } from 'ws';
import { ok } from '../utils/ui';
import { closeDashboardSessions } from './middleware/auth-middleware';
import { closeAacStore } from '../store/store-service';

/**
 * Setup shutdown handlers for SIGINT and SIGTERM
 */
export function setupGracefulShutdown(
  server: HTTPServer,
  _wss: WebSocketServer,
  cleanup?: () => void
): void {
  const shutdown = () => {
    console.log('\n' + ok('Shutting down...'));

    // Run cleanup (closes file watchers + WebSocket clients)
    if (cleanup) {
      cleanup();
    }

    // Close the server, save the sign-ins (at most 2 s) and close the history store (it seals
    // its logs; at most 3 s), then exit. Every acknowledged event is already on disk.
    server.close();
    const storeClosed = Promise.race([
      closeAacStore(),
      new Promise<void>((resolve) => setTimeout(resolve, 3000).unref?.()),
    ]);
    void Promise.allSettled([closeDashboardSessions(), storeClosed]).finally(() => process.exit(0));
  };

  process.on('SIGINT', shutdown);
  process.on('SIGTERM', shutdown);
}
