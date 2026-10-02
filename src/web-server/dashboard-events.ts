import type { IncomingMessage } from 'http';
import { WebSocket, type WebSocketServer } from 'ws';
import type { SignInJob } from './services/signin-jobs';

/**
 * Push hints to the dashboard's `/ws` clients. Only sessions that passed the
 * upgrade guard (dashboard origin plus a signed-in session, or localhost when
 * auth is off) are connected. A message is a hint to re-read the API, never
 * the data itself; clients keep polling.
 *
 * A sign-in job can carry a device code, so it is built per client: a socket
 * whose upgrade did not arrive over a secure transport receives the job
 * without its verification block (CONTRACT-registry-lifecycle section 1 rule 5).
 */
export type DashboardEvent = { type: 'accounts-changed' } | { type: 'signin-job'; job: SignInJob };

export interface DashboardEventClient {
  /** isSecureTransport() of the socket's upgrade request. */
  secure: boolean;
}

type EventSource = DashboardEvent | ((client: DashboardEventClient) => DashboardEvent | null);

const servers = new Set<WebSocketServer>();
const clients = new WeakMap<WebSocket, DashboardEventClient>();

export interface DashboardEventServerOptions {
  /** Classify each connection when it opens; without it every client counts as not secure. */
  isSecure?: (request: IncomingMessage) => boolean;
}

/** Register a server's clients; returns the detach function for shutdown. */
export function attachDashboardEventServer(
  wss: WebSocketServer,
  options: DashboardEventServerOptions = {}
): () => void {
  servers.add(wss);
  const onConnection = (socket: WebSocket, request: IncomingMessage) => {
    let secure = false;
    try {
      secure = options.isSecure?.(request) === true;
    } catch {
      secure = false;
    }
    clients.set(socket, { secure });
  };
  wss.on('connection', onConnection);
  return () => {
    servers.delete(wss);
    wss.off('connection', onConnection);
  };
}

/**
 * Send to every open client; returns how many sockets were sent the event. A
 * function source builds the event per client (null skips that client).
 */
export function broadcastDashboardEvent(source: EventSource): number {
  let sent = 0;
  for (const wss of servers) {
    for (const client of wss.clients) {
      if (client.readyState !== WebSocket.OPEN) continue;
      let event: DashboardEvent | null;
      try {
        event =
          typeof source === 'function' ? source(clients.get(client) ?? { secure: false }) : source;
      } catch {
        event = null;
      }
      if (!event) continue;
      try {
        client.send(JSON.stringify(event));
        sent += 1;
      } catch {
        // A socket that closed in between is skipped.
      }
    }
  }
  return sent;
}
