/**
 * R3 lane B: the store method hop over a pooled Unix socket, speaking the one fixed HTTP/1.1
 * shape `aac-http` answers `POST /internal/store/<method>` with (a `content-length` body, the
 * boot id header, no chunked/1xx/trailers). Node's `http.request` carries the same contract but
 * costs a ClientRequest, an agent hop and a parser event chain per call; under the fixture's
 * fresh-process measurement that JS dominates the socket's price over the in-process NAPI hop.
 * This client writes one request buffer and reads one fixed-shape reply, so a call costs one
 * write, one read and a promise. It is strict and fail-safe: any reply that is not the exact
 * shape (missing `content-length`, a bad status line, an early close, an oversize body) destroys
 * the socket and rejects with a transport error, which the store handle turns into `service_down`
 * and the front door into the TypeScript fallback. A write is never retried. Idle sockets are
 * unref'd, so a parked pool never keeps the dashboard alive; a socket in flight is ref'd.
 */
import net from 'node:net';
import { EventEmitter } from 'node:events';

/** A pending call's cancellation handle: the liveness watch and a deadline destroy it. */
export interface StoreCallHandle {
  once(event: 'close', listener: () => void): void;
  destroy(error?: Error): void;
}

/** Sockets kept per boot socket path, and the calls waiting for a free one. */
interface Pool {
  idle: net.Socket[];
  busy: number;
  waiters: Array<(socket: net.Socket) => void>;
}
const pools = new Map<string, Pool>();
/** At most this many sockets to one service, as the keep-alive agent bounded them. */
const MAX_SOCKETS = 16;

function poolOf(socketPath: string): Pool {
  let pool = pools.get(socketPath);
  if (!pool) {
    pool = { idle: [], busy: 0, waiters: [] };
    pools.set(socketPath, pool);
  }
  return pool;
}

function takeIdle(pool: Pool): net.Socket | null {
  while (pool.idle.length > 0) {
    const socket = pool.idle.pop() as net.Socket;
    if (!socket.destroyed && socket.writable) {
      socket.removeAllListeners('close');
      return socket;
    }
    socket.destroy();
  }
  return null;
}

function connectFresh(pool: Pool, socketPath: string): Promise<net.Socket> {
  pool.busy++;
  return new Promise((resolve, reject) => {
    const socket = net.connect(socketPath);
    socket.setNoDelay(true);
    const fail = (error: Error) => {
      pool.busy--;
      socket.destroy();
      reject(error);
    };
    socket.once('error', fail);
    socket.once('connect', () => {
      socket.removeListener('error', fail);
      resolve(socket);
    });
  });
}

/** A live idle socket, a fresh connection, or a waiter queued for the next release. */
function acquire(pool: Pool, socketPath: string): Promise<net.Socket> {
  const idle = takeIdle(pool);
  if (idle) {
    idle.ref();
    pool.busy++;
    return Promise.resolve(idle);
  }
  if (pool.busy < MAX_SOCKETS) return connectFresh(pool, socketPath);
  return new Promise((resolve) => {
    pool.waiters.push(resolve);
  });
}

/** Hand idle sockets and fresh connections to queued waiters, then prune an empty pool. */
function drain(pool: Pool, socketPath: string): void {
  while (pool.waiters.length > 0) {
    const idle = takeIdle(pool);
    if (idle) {
      const waiter = pool.waiters.shift() as (socket: net.Socket) => void;
      idle.ref();
      pool.busy++;
      waiter(idle);
      continue;
    }
    if (pool.busy < MAX_SOCKETS) {
      const waiter = pool.waiters.shift() as (socket: net.Socket) => void;
      pool.busy++;
      const fresh = net.connect(socketPath);
      fresh.setNoDelay(true);
      const fail = () => {
        pool.busy--;
        fresh.destroy();
        pool.waiters.unshift(waiter);
        drain(pool, socketPath);
      };
      fresh.once('error', fail);
      fresh.once('connect', () => {
        fresh.removeListener('error', fail);
        fresh.ref();
        waiter(fresh);
      });
      continue;
    }
    break;
  }
  if (pool.idle.length === 0 && pool.busy === 0 && pool.waiters.length === 0)
    pools.delete(socketPath);
}

function release(pool: Pool, socketPath: string, socket: net.Socket): void {
  pool.busy--;
  if (!socket.destroyed && socket.writable) {
    socket.unref();
    pool.idle.push(socket);
    // A service that closes an idle keep-alive socket drops it from the pool at once, so the
    // next call connects fresh instead of writing into a half-closed socket (as the agent did).
    socket.once('close', () => {
      const at = pool.idle.indexOf(socket);
      if (at >= 0) pool.idle.splice(at, 1);
    });
  } else {
    socket.destroy();
  }
  drain(pool, socketPath);
}

export interface StoreReply {
  status: number;
  boot: string | null;
  text: string;
}

/**
 * One store request over the pool. Resolves with the reply's status, boot header and body text;
 * rejects with the error passed to `handle.destroy` (a deadline or the liveness watch) or a
 * transport error when the socket fails or the reply is not the fixed shape. The handle's
 * `close` event fires once the call settles, so the liveness watch can drop it.
 */
export function storeSocketCall(
  socketPath: string,
  secret: string,
  principal: string,
  method: string,
  args: string,
  replyCap: number
): { promise: Promise<StoreReply>; handle: StoreCallHandle } {
  const emitter = new EventEmitter();
  let settled = false;
  let socket: net.Socket | null = null;
  let pool: Pool | null = null;
  let finish: (error: Error | null, reply?: StoreReply) => void = () => {};

  const handle: StoreCallHandle = {
    once: (event, listener) => {
      emitter.once(event, listener);
    },
    destroy: (error?: Error) => {
      finish(error ?? new Error('Rust store call destroyed'));
    },
  };

  const promise = new Promise<StoreReply>((resolve, reject) => {
    finish = (error, reply) => {
      if (settled) return;
      settled = true;
      const held = socket;
      const heldPool = pool;
      socket = null;
      pool = null;
      if (held && heldPool) {
        if (error) {
          heldPool.busy--;
          held.destroy();
          drain(heldPool, socketPath);
        } else {
          release(heldPool, socketPath, held);
        }
      }
      if (error) reject(error);
      else resolve(reply as StoreReply);
      emitter.emit('close');
    };

    const body = Buffer.from(args, 'utf8');
    const head =
      `POST /internal/store/${method} HTTP/1.1\r\n` +
      `host: localhost\r\n` +
      `x-aac-boot-secret: ${secret}\r\n` +
      `x-aac-principal: ${principal}\r\n` +
      `content-type: application/json\r\n` +
      `content-length: ${body.length}\r\n` +
      `connection: keep-alive\r\n\r\n`;
    const request = Buffer.concat([Buffer.from(head, 'utf8'), body]);

    void acquire(poolOf(socketPath), socketPath).then(
      (sock) => {
        if (settled) {
          // Destroyed while waiting for a socket: give it back and stop.
          release(poolOf(socketPath), socketPath, sock);
          return;
        }
        socket = sock;
        pool = poolOf(socketPath);
        let headChunks: Buffer[] = [];
        let headEnd = -1;
        let status = 0;
        let boot: string | null = null;
        let length = -1;
        let body: Buffer = Buffer.alloc(0);
        let bodyOffset = 0;
        const complete = () => {
          sock.removeListener('data', onData);
          sock.removeListener('error', onFail);
          sock.removeListener('close', onClose);
          finish(null, { status, boot, text: body.toString('utf8') });
        };
        const onData = (chunk: Buffer) => {
          if (headEnd < 0) {
            headChunks.push(chunk);
            const head = Buffer.concat(headChunks);
            headEnd = head.indexOf('\r\n\r\n');
            if (headEnd < 0) {
              if (head.length > 64 * 1024) finish(new Error('Rust store reply head too large'));
              return;
            }
            const lines = head.subarray(0, headEnd).toString('utf8').split('\r\n');
            const statusLine = /^HTTP\/1\.[01] (\d{3})(?: |$)/.exec(lines[0]);
            if (!statusLine) {
              finish(new Error('Rust store reply is not HTTP'));
              return;
            }
            status = Number(statusLine[1]);
            for (const line of lines.slice(1)) {
              const colon = line.indexOf(':');
              if (colon < 0) continue;
              const name = line.slice(0, colon).trim().toLowerCase();
              const value = line.slice(colon + 1).trim();
              if (name === 'content-length') length = Number(value);
              else if (name === 'x-aac-service-boot') boot = value;
            }
            if (!Number.isInteger(length) || length < 0) {
              finish(new Error('Rust store reply has no content-length'));
              return;
            }
            if (length > replyCap) {
              finish(new Error('Rust store response cap'));
              return;
            }
            body = Buffer.allocUnsafe(length);
            const start = headEnd + 4;
            const already = Math.min(head.length - start, length);
            if (already > 0) {
              head.copy(body, 0, start, start + already);
              bodyOffset = already;
            }
            headChunks = [];
            if (bodyOffset >= length) complete();
            return;
          }
          const remaining = length - bodyOffset;
          if (chunk.length > remaining) {
            finish(new Error('Rust store reply exceeded its content-length'));
            return;
          }
          chunk.copy(body, bodyOffset, 0, chunk.length);
          bodyOffset += chunk.length;
          if (bodyOffset >= length) complete();
        };
        const onFail = (error: Error) => finish(error);
        const onClose = () => finish(new Error('Rust store socket closed'));
        sock.on('data', onData);
        sock.once('error', onFail);
        sock.once('close', onClose);
        sock.write(request, (error) => {
          if (error) finish(error);
        });
      },
      (error) => finish(error instanceof Error ? error : new Error(String(error)))
    );
  });

  return { promise, handle };
}
