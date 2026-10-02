import { WebSocket, type WebSocketServer } from 'ws';

/**
 * Push hints to the dashboard's `/ws` clients. Only sessions that passed the
 * upgrade guard (dashboard origin plus a signed-in session, or localhost when
 * auth is off) are connected. A message is a hint to re-read the API, never
 * the data itself.
 */
export type DashboardEvent = { type: 'accounts-changed' };

const servers = new Set<WebSocketServer>();

/** Register a server's clients; returns the detach function for shutdown. */
export function attachDashboardEventServer(wss: WebSocketServer): () => void {
  servers.add(wss);
  return () => {
    servers.delete(wss);
  };
}

/** Send to every open client; returns how many sockets were sent the event. */
export function broadcastDashboardEvent(event: DashboardEvent): number {
  const message = JSON.stringify(event);
  let sent = 0;
  for (const wss of servers) {
    for (const client of wss.clients) {
      if (client.readyState !== WebSocket.OPEN) continue;
      try {
        client.send(message);
        sent += 1;
      } catch {
        // A socket that closed in between is skipped.
      }
    }
  }
  return sent;
}
