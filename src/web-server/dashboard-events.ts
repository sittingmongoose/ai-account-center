import type { IncomingMessage } from 'http';
import type { Writable } from 'node:stream';
import type { AppUpdateHostProgress, AppUpdateJob } from './services/app-update-contract';
import { WebSocket, type WebSocketServer } from 'ws';
import type { RequestAuthKind } from './middleware/request-auth';
import type { SignInJob } from './services/signin-jobs';

/**
 * Push hints to the dashboard's `/ws` clients. Only sessions that passed the
 * upgrade guard (dashboard origin plus a signed-in session, or localhost when
 * auth is off) are connected. A message is a hint to re-read the API, never
 * the data itself; clients keep polling.
 *
 * A sign-in job can carry a device code, so it is built per client from what
 * the socket's upgrade request proved: who connected (a browser session, a
 * device token or nobody) and whether it came over a secure transport
 * (CONTRACT-registry-lifecycle section 1 rule 5 and section 6.6).
 */
export type DashboardEvent =
  | { type: 'accounts-changed' }
  | { type: 'reset-claims-changed'; id: string }
  | { type: 'signin-job'; job: SignInJob }
  /** A Status tab source changed; the page re-reads /api/status. Browser sessions only. */
  | { type: 'status-changed' }
  | { type: 'analytics-changed' }
  | { type: 'app-update-job'; job: AppUpdateProgress };

type AppUpdateProgress = Pick<
  AppUpdateJob,
  'id' | 'state' | 'cancelRequested' | 'expectedResults'
> & {
  completedResults: number;
  hosts: Array<AppUpdateHostProgress & { host: string }>;
};

type EventSink = (event: DashboardEvent) => void;
const eventSinks = new Set<EventSink>();
const MAX_FEED_FRAME = 64 * 1024;

/** Attach an inherited child stdin. The owner controls the pipe lifetime and child restart. */
export function attachDashboardEventFeed(pipe: Writable): () => void {
  const detach = () => {
    eventSinks.delete(send);
  };
  const send = (event: DashboardEvent) => {
    // Slow or disconnected subscribers can repair missed hints by polling after reconnect.
    if (pipe.destroyed || pipe.writableEnded || pipe.writableNeedDrain) return;
    const line = JSON.stringify(event) + '\n';
    if (Buffer.byteLength(line) <= MAX_FEED_FRAME) pipe.write(line);
  };
  pipe.on('error', detach);
  pipe.once('close', () => {
    detach();
    pipe.off('error', detach);
  });
  eventSinks.add(send);
  return detach;
}

/** A job as a client on a plain transport sees it. */
export function redactSignInJob<
  T extends { verification: unknown; result: null | { email: string | null } },
>(job: T): T {
  return { ...job, verification: null, result: job.result ? { ...job.result, email: null } : null };
}

/** Existing sign-in WebSocket shape, with transport-specific redaction. */
export function signInJobEvent(
  job: SignInJob,
  client: DashboardEventClient
): { type: 'signin-job'; job: SignInJob } | null {
  if (client.authKind !== 'session') return null;
  return { type: 'signin-job', job: client.secure ? job : redactSignInJob(job) };
}

function eventForClient(
  event: DashboardEvent,
  client: DashboardEventClient
): DashboardEvent | null {
  if (event.type === 'accounts-changed') return event;
  if (event.type === 'signin-job') return signInJobEvent(event.job, client);
  return client.authKind === 'session' ? event : null;
}

/** Project one owner job onto the small, output-free WebSocket progress shape. */
export function broadcastAppUpdateProgress(job: AppUpdateJob): number {
  return broadcastDashboardEvent({
    type: 'app-update-job',
    job: {
      id: job.id,
      state: job.state,
      cancelRequested: job.cancelRequested,
      completedResults: job.results.length,
      expectedResults: job.expectedResults,
      hosts: Object.entries(job.hosts ?? {}).map(([host, progress]) => ({
        host,
        state: progress.state,
        currentApp: progress.currentApp,
        phase: progress.phase,
        phaseSince: progress.phaseSince ?? null,
      })),
    },
  });
}

export interface DashboardEventClient {
  /**
   * isSecureTransport() of the socket's upgrade request, when it opened and
   * again for each event, so turning off local network trust (rule 4) stops
   * codes to sockets that relied on it.
   */
  secure: boolean;
  /** authKind() of the socket's upgrade request; null when unknown or signed out. */
  authKind: RequestAuthKind;
  /** The session epoch of a browser session's socket (CONTRACT-auth-devices section 3). */
  sessionEpoch?: number | null;
}

const UNKNOWN_CLIENT: DashboardEventClient = Object.freeze({ secure: false, authKind: null });

type EventSource = DashboardEvent | ((client: DashboardEventClient) => DashboardEvent | null);

interface ClientRecord extends DashboardEventClient {
  /** isSecureTransport() of the upgrade request, asked again now. */
  stillSecure: () => boolean;
  /** The upgrade's socket, so a record whose recorded session expiry passed can be closed. */
  socket: WebSocket;
  /** The session id of a browser connection, so its logout closes this socket. */
  sessionId: string | null;
  /** The session cookie's recorded expiry (ms since epoch); null when the upgrade carried none. */
  sessionExpires: number | null;
}

const servers = new Set<WebSocketServer>();
const clients = new WeakMap<WebSocket, ClientRecord>();

function clientView(record: ClientRecord | undefined): DashboardEventClient {
  if (!record) return UNKNOWN_CLIENT;
  // A recorded server expiry in the past means the cookie's lifetime ended
  // without the socket going away: the connection no longer counts as a
  // session and closes now. A browser-session cookie records no expiry; that
  // socket is never guess-closed here.
  const expired =
    record.authKind === 'session' &&
    record.sessionExpires !== null &&
    record.sessionExpires <= Date.now();
  if (expired) {
    try {
      record.socket.close(4001, 'Signed out');
    } catch {
      // Already closing.
    }
  }
  return {
    secure: record.secure && record.stillSecure(),
    authKind: expired ? null : record.authKind,
    sessionEpoch: expired ? null : record.sessionEpoch,
  };
}

export interface DashboardEventServerOptions {
  /** Classify each connection when it opens; without it every client counts as not secure. */
  isSecure?: (request: IncomingMessage) => boolean;
  /** Who opened each connection; without it every client counts as unknown (null). */
  authKind?: (request: IncomingMessage) => RequestAuthKind;
  /** The session epoch of each browser connection, so "sign out other browsers" can close it. */
  sessionEpoch?: (request: IncomingMessage) => number | null;
  /**
   * The identity of each browser connection (its session id and the cookie
   * expiry the upgrade recorded), so a logout can close its sockets. Asked
   * only for connections already classified as a session.
   */
  sessionIdentity?: (request: IncomingMessage) => {
    sessionId: string | null;
    expires: Date | null;
  } | null;
}

/** Register a server's clients; returns the detach function for shutdown. */
export function attachDashboardEventServer(
  wss: WebSocketServer,
  options: DashboardEventServerOptions = {}
): () => void {
  servers.add(wss);
  const onConnection = (socket: WebSocket, request: IncomingMessage) => {
    const stillSecure = (): boolean => {
      try {
        return options.isSecure?.(request) === true;
      } catch {
        return false;
      }
    };
    const secure = stillSecure();
    let kind: RequestAuthKind = null;
    try {
      const value = options.authKind?.(request) ?? null;
      kind = value === 'session' || value === 'device' ? value : null;
    } catch {
      kind = null;
    }
    let sessionEpoch: number | null = null;
    try {
      const value = options.sessionEpoch?.(request);
      sessionEpoch = typeof value === 'number' ? value : null;
    } catch {
      sessionEpoch = null;
    }
    let sessionId: string | null = null;
    let sessionExpires: number | null = null;
    if (kind === 'session') {
      try {
        const value = options.sessionIdentity?.(request) ?? null;
        if (value && typeof value.sessionId === 'string' && value.sessionId) {
          sessionId = value.sessionId;
          sessionExpires = value.expires instanceof Date ? value.expires.getTime() : null;
        }
      } catch {
        /* Unknown session identity: the socket keeps its classification. */
      }
    }
    clients.set(socket, {
      socket,
      secure,
      authKind: kind,
      sessionEpoch,
      stillSecure,
      sessionId,
      sessionExpires,
    });
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
  if (typeof source !== 'function') {
    for (const sink of eventSinks) {
      try {
        sink(source);
      } catch {
        /* The owner has already published state. A missed hint is recoverable by polling. */
      }
    }
  }
  let sent = 0;
  for (const wss of servers) {
    for (const client of wss.clients) {
      if (client.readyState !== WebSocket.OPEN) continue;
      let event: DashboardEvent | null;
      try {
        const view = clientView(clients.get(client));
        event = typeof source === 'function' ? source(view) : eventForClient(source, view);
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

/**
 * Close the browser-session sockets whose epoch is no longer current (after
 * "sign out other browsers" or a password change). Returns how many closed.
 */
export function closeStaleSessionClients(isCurrent: (epoch: number | null) => boolean): number {
  let closed = 0;
  for (const wss of servers) {
    for (const client of wss.clients) {
      const info = clients.get(client);
      if (!info || info.authKind !== 'session') continue;
      let current = true;
      try {
        current = isCurrent(info.sessionEpoch ?? null);
      } catch {
        current = false;
      }
      if (current) continue;
      try {
        client.close(4001, 'Signed out');
        closed += 1;
      } catch {
        // Already closing.
      }
    }
  }
  return closed;
}

/**
 * Close the browser-session sockets opened with `sessionId` (a logout or a
 * session destruction). Returns how many closed. Sockets whose upgrade
 * recorded no session id are left to the epoch and expiry checks.
 */
export function closeSessionClients(sessionId: string): number {
  let closed = 0;
  for (const wss of servers) {
    for (const client of wss.clients) {
      const info = clients.get(client);
      if (!info || info.authKind !== 'session') continue;
      if (info.sessionId !== sessionId) continue;
      try {
        client.close(4001, 'Signed out');
        closed += 1;
      } catch {
        // Already closing.
      }
    }
  }
  return closed;
}
