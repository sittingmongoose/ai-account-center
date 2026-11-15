/** Bounded HTTP over a private Unix socket. Never copy caller headers into this request. */
import http from 'http';
import { NetworkError } from '../errors/error-types';

export interface VerifiedRustPrincipal {
  kind: 'session' | 'device' | 'bar' | 'hub';
  id: string;
  role: 'public' | 'admin' | 'device' | 'hub' | 'service';
}

/** L1: the front door itself on the service's internal routes (feeds and events) only. */
export const FRONT_DOOR_PRINCIPAL: VerifiedRustPrincipal = {
  kind: 'session',
  id: 'front-door',
  role: 'service',
};

export interface RustConnection {
  socketPath: string;
  secret: string;
  bootId: string;
  /** L1: one keep-alive agent per boot; absent, each call opens its own connection. */
  agent?: http.Agent;
}

export interface RustReply {
  status: number;
  body: unknown;
  /** L1: the service's named reason on a private 503 (`x-aac-decline`). */
  decline: string | null;
  /** L1: the service's build identity, carried on health replies (`x-aac-service-build`). */
  build: string | null;
}

/** Total deadline per request, the retry of a reused connection included. */
export const RUST_DEADLINE_MS = 500;

/** One keep-alive agent per boot (L1): the hop costs a request, not a connection. */
export function rustAgent(): http.Agent {
  return new http.Agent({ keepAlive: true, maxSockets: 16, maxFreeSockets: 16 });
}

/** How a request failed: the bridge's breaker counts these; declines never come here. */
export type RustFailureKind = 'deadline' | 'connect' | 'reset' | 'cap' | 'reply' | 'boot';

export class RustTransportError extends NetworkError {
  constructor(
    readonly kind: RustFailureKind,
    message: string
  ) {
    super(message);
  }
}

function failureKind(error: unknown): RustFailureKind {
  if (error instanceof RustTransportError) return error.kind;
  const code = (error as NodeJS.ErrnoException | undefined)?.code;
  return code === 'ECONNREFUSED' || code === 'ENOENT' || code === 'EACCES' ? 'connect' : 'reset';
}

export function rustFailureKind(error: unknown): RustFailureKind {
  return failureKind(error);
}

function once(
  connection: RustConnection,
  route: string,
  principal: VerifiedRustPrincipal,
  active: Set<http.ClientRequest>,
  payload: string | undefined,
  method: 'GET' | 'PUT',
  cap: number,
  deadline: number
): Promise<RustReply> {
  return new Promise((resolve, reject) => {
    const request = http.request({
      socketPath: connection.socketPath,
      path: route,
      method,
      agent: connection.agent ?? false,
      headers: {
        'x-aac-boot-secret': connection.secret,
        'x-aac-principal': JSON.stringify(principal),
        ...(payload === undefined
          ? {}
          : { 'content-type': 'application/json', 'content-length': Buffer.byteLength(payload) }),
      },
    });
    active.add(request);
    const timer = setTimeout(
      () => request.destroy(new RustTransportError('deadline', 'Rust service deadline')),
      Math.max(1, deadline - Date.now())
    );
    request.once('close', () => {
      clearTimeout(timer);
      active.delete(request);
    });
    request.once('error', (error) => {
      // A kept-alive connection the service had already closed: the caller may retry once.
      (error as { reused?: boolean }).reused = request.reusedSocket;
      reject(error);
    });
    request.once('response', (response) => {
      const chunks: Buffer[] = [];
      let size = 0;
      response.on('data', (chunk: Buffer) => {
        size += chunk.length;
        if (size > cap) request.destroy(new RustTransportError('cap', 'Rust service response cap'));
        else chunks.push(chunk);
      });
      response.once('error', reject);
      response.once('end', () => {
        if (response.headers['x-aac-service-boot'] !== connection.bootId) {
          reject(new RustTransportError('boot', 'Rust service boot mismatch'));
          return;
        }
        const decline = response.headers['x-aac-decline'];
        const build = response.headers['x-aac-service-build'];
        try {
          resolve({
            status: response.statusCode ?? 503,
            body: JSON.parse(Buffer.concat(chunks).toString('utf8')),
            decline: typeof decline === 'string' ? decline : null,
            build: typeof build === 'string' ? build : null,
          });
        } catch {
          reject(new RustTransportError('reply', 'Rust service invalid response'));
        }
      });
    });
    request.end(payload);
  });
}

export function requestRust(
  connection: RustConnection,
  route: string,
  principal: VerifiedRustPrincipal,
  active: Set<http.ClientRequest>,
  input?: unknown,
  method: 'GET' | 'PUT' = 'GET'
): Promise<RustReply> {
  const payload = input === undefined ? undefined : JSON.stringify(input);
  if (payload !== undefined && Buffer.byteLength(payload) > 4 * 1024 * 1024)
    return Promise.reject(new RustTransportError('cap', 'Rust service request cap'));
  const cap = input === undefined ? 64 * 1024 : 2 * 1024 * 1024;
  const deadline = Date.now() + RUST_DEADLINE_MS;
  const attempt = () => once(connection, route, principal, active, payload, method, cap, deadline);
  return attempt().catch((error: unknown) => {
    // Node does not retry a request whose kept-alive socket the server closed meanwhile.
    // The reads are idempotent: one retry on a fresh connection, inside the same deadline.
    const reused = (error as { reused?: boolean } | undefined)?.reused === true;
    const code = (error as NodeJS.ErrnoException | undefined)?.code;
    if (reused && method === 'GET' && (code === 'ECONNRESET' || code === 'EPIPE')) return attempt();
    throw error;
  });
}

/** L1 feeds: push a source's latest generation to the service (the "fed" level). */
export function pushRustFeed(
  connection: RustConnection,
  source: string,
  feed: { generation: string; data: unknown; facts: unknown },
  active: Set<http.ClientRequest>
): Promise<RustReply> {
  return requestRust(
    connection,
    `/internal/feed/${encodeURIComponent(source)}`,
    FRONT_DOOR_PRINCIPAL,
    active,
    feed,
    'PUT'
  );
}

/**
 * L1 events: the service's NDJSON stream (`facts-needed` today; `status-changed`,
 * `scoreboard-changed` and `store-health` as their groups move). Its own connection, no
 * deadline; each line is parsed and handed over; lines over 64 KiB end the stream.
 */
export function openRustEvents(
  connection: RustConnection,
  onEvent: (event: Record<string, unknown>) => void
): { ready: Promise<void>; close: () => void } {
  let request: http.ClientRequest | null = null;
  const ready = new Promise<void>((resolve, reject) => {
    request = http.request({
      socketPath: connection.socketPath,
      path: '/internal/events',
      method: 'GET',
      agent: false,
      headers: {
        'x-aac-boot-secret': connection.secret,
        'x-aac-principal': JSON.stringify(FRONT_DOOR_PRINCIPAL),
      },
    });
    request.once('error', reject);
    request.once('response', (response) => {
      if (
        response.statusCode !== 200 ||
        response.headers['x-aac-service-boot'] !== connection.bootId
      ) {
        response.resume();
        request?.destroy();
        reject(new RustTransportError('reply', 'Rust event stream refused'));
        return;
      }
      resolve();
      let buffer = '';
      response.setEncoding('utf8');
      response.on('data', (chunk: string) => {
        buffer += chunk;
        let end: number;
        while ((end = buffer.indexOf('\n')) >= 0) {
          const line = buffer.slice(0, end);
          buffer = buffer.slice(end + 1);
          try {
            const event = JSON.parse(line) as unknown;
            if (event && typeof event === 'object' && !Array.isArray(event))
              onEvent(event as Record<string, unknown>);
          } catch {
            /* A line that is not an event is skipped. */
          }
        }
        if (buffer.length > 64 * 1024) request?.destroy();
      });
    });
    request.end();
  });
  return { ready, close: () => request?.destroy() };
}
