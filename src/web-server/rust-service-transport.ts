/** Bounded HTTP over a private Unix socket. Never copy caller headers into this request. */
import fs from 'fs';
import http from 'http';
import { NetworkError } from '../errors/error-types';
import { storeSocketCall, type StoreCallHandle } from '../store/store-socket';

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

/** The front-door principal as the wire header: serialized once, not per store call. */
const FRONT_DOOR_PRINCIPAL_JSON = JSON.stringify(FRONT_DOOR_PRINCIPAL);

/** Reserved liveness envelope accepted by aac-api's principal validation. */
export const PUBLIC_HEALTH_PRINCIPAL: VerifiedRustPrincipal = {
  kind: 'session',
  id: 'public-health',
  role: 'public',
};

export interface RustConnection {
  socketPath: string;
  secret: string;
  bootId: string;
  /** R3: compiled service identity, populated by the lifecycle health check. */
  build?: { commit: string; storeSchema: number };
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
  /** L1 reply floor: the bytes as sent, and a view route's `ETag` and `x-aac-view`. */
  bytes?: Buffer;
  etag?: string | null;
  view?: string | null;
}

/** Total deadline per request, the retry of a reused connection included. */
export const RUST_DEADLINE_MS = 500;

/** R1: a view reply's cap (the finished Home answer of a large home is several hundred KiB). */
export const RUST_VIEW_CAP = 8 * 1024 * 1024;

/**
 * R1 request options: extra private headers (the fed request's ids, clock and page context), a reply passed
 * through as bytes without parsing (`raw`: `body` is then null), its cap, and a body already written as text (a
 * feed push whose data TS serialized once, for its change key). G18: the usage sample push carries a larger
 * body (the quota history) and its own deadline.
 */
export interface RustRequestOptions {
  headers?: Record<string, string>;
  raw?: boolean;
  cap?: number;
  text?: string;
  /** A longer total deadline for internal work that may take several seconds. */
  deadlineMs?: number;
  signal?: AbortSignal;
  payloadCap?: number;
}

/**
 * The service socket's read buffer. A fed view reply is up to the 2 MiB view cap; the default 64 KiB splits a
 * 200 KB Home answer into several chunks, each its own pooled Buffer. One larger buffer keeps the common reply
 * in one chunk (less allocation and copying per request, on every read route).
 */
const RUST_SOCKET_HWM = 256 * 1024;

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
  method: 'GET' | 'PUT' | 'PATCH' | 'POST',
  cap: number,
  deadline: number,
  options: RustRequestOptions = {}
): Promise<RustReply> {
  return new Promise((resolve, reject) => {
    // highWaterMark reaches the socket through the agent's net.createConnection (Node forwards unknown
    // request options); the typings do not name it on RequestOptions.
    const requestOptions: http.RequestOptions & { highWaterMark: number } = {
      socketPath: connection.socketPath,
      path: route,
      method,
      highWaterMark: RUST_SOCKET_HWM,
      signal: options.signal,
      agent: connection.agent ?? false,
      headers: {
        ...options.headers,
        'x-aac-boot-secret': connection.secret,
        'x-aac-principal': JSON.stringify(principal),
        ...(payload === undefined
          ? {}
          : { 'content-type': 'application/json', 'content-length': Buffer.byteLength(payload) }),
      },
    };
    const request = http.request(requestOptions);
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
        const etag = response.headers.etag;
        const view = response.headers['x-aac-view'];
        const bytes = chunks.length === 1 ? chunks[0] : Buffer.concat(chunks);
        try {
          resolve({
            status: response.statusCode ?? 503,
            // R1: a view passed through is never parsed here; the front door writes its bytes.
            body: options.raw ? null : JSON.parse(bytes.toString('utf8')),
            decline: typeof decline === 'string' ? decline : null,
            build: typeof build === 'string' ? build : null,
            bytes,
            etag: typeof etag === 'string' ? etag : null,
            view: typeof view === 'string' ? view : null,
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
  method: 'GET' | 'PUT' | 'PATCH' | 'POST' = 'GET',
  options: RustRequestOptions = {}
): Promise<RustReply> {
  const payload = options.text ?? (input === undefined ? undefined : JSON.stringify(input));
  if (payload !== undefined && Buffer.byteLength(payload) > (options.payloadCap ?? 4 * 1024 * 1024))
    return Promise.reject(new RustTransportError('cap', 'Rust service request cap'));
  const cap = options.cap ?? (payload === undefined ? 64 * 1024 : 2 * 1024 * 1024);
  const deadline = Date.now() + (options.deadlineMs ?? RUST_DEADLINE_MS);
  const attempt = () =>
    once(connection, route, principal, active, payload, method, cap, deadline, options);
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
 * R1: push a generation whose data and facts TS already wrote as JSON text (the dashboard's text is its change
 * key, so it is serialized once). The reply names the generation the service holds after the push: a newer one
 * when this push lost a race.
 */
export function pushRustFeedText(
  connection: RustConnection,
  source: string,
  generation: string,
  data: string,
  facts: string,
  active: Set<http.ClientRequest>
): Promise<RustReply> {
  return requestRust(
    connection,
    `/internal/feed/${encodeURIComponent(source)}`,
    FRONT_DOOR_PRINCIPAL,
    active,
    undefined,
    'PUT',
    { text: `{"generation":${JSON.stringify(generation)},"data":${data},"facts":${facts}}` }
  );
}

/** G17: the raw Home inputs the Rust feed producer produces generations from (the dashboard as read, this
 * process's Open progress, the runtime's inventory). The reply names the three held generations, or a
 * private 503 with the producer's named decline. */
export interface RustHomeInputs {
  platform: string;
  secure: boolean;
  dashboard: unknown;
  opens: unknown;
  antigravity: unknown;
}

export function putRustHomeInputs(
  connection: RustConnection,
  inputs: RustHomeInputs,
  active: Set<http.ClientRequest>
): Promise<RustReply> {
  return requestRust(
    connection,
    '/internal/home-inputs',
    FRONT_DOOR_PRINCIPAL,
    active,
    inputs,
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
      response.once('error', () => request?.destroy());
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

/** L5: current private connections, owned by the supervisors, bound to their exact AAC root. */
const agentConnections = new Map<
  symbol,
  {
    root: string;
    connection: RustConnection;
    active: Set<http.ClientRequest>;
    cacheWrites: Map<string, Promise<boolean>>;
    health: { replied?: () => void; failed?: (kind: RustFailureKind) => void };
  }
>();

/**
 * The root spelling a lease registers under: the lifecycle canonicalizes its root once, so a
 * symlinked scope (a `/tmp` CCS_HOME on macOS) names the same boot (Wonder L4). A root that
 * cannot be resolved keeps its own spelling and misses, as before.
 */
export function canonicalRoot(root: string): string {
  const cached = rootKeys.get(root);
  if (cached !== undefined) return cached;
  try {
    const real = fs.realpathSync(root);
    rootKeys.set(root, real);
    return real;
  } catch {
    // An unresolved root keeps its own spelling and is retried next call: it may appear later.
    return root;
  }
}

/** Resolved root spellings, cached: a store call resolves its root several times, and the
 * dashboard's roots do not change under it. Only successful resolutions are kept. */
const rootKeys = new Map<string, string>();
function rustAgentConnection(root: string) {
  const scope = canonicalRoot(root);
  return [...agentConnections.values()].reverse().find((entry) => entry.root === scope);
}

export function registerRustAgentConnection(
  root: string,
  connection: RustConnection,
  active: Set<http.ClientRequest>,
  health: { replied?: () => void; failed?: (kind: RustFailureKind) => void } = {}
): () => void {
  const key = Symbol();
  agentConnections.set(key, { root, connection, active, health, cacheWrites: new Map() });
  return () => agentConnections.delete(key);
}

/** Whether a live service connection is registered for this root (the M4c adapter's gate). */
export function rustAgentConnected(root: string): boolean {
  const scope = canonicalRoot(root);
  return [...agentConnections.values()].some((entry) => entry.root === scope);
}

/** Only an old or absent service can delegate a Cursor provider read to TS. */
export async function readRustCursorAccount(
  root: string,
  input: { sinceMs: number; untilMs: number; force: boolean } | { command: 'key-status' | 'quota' }
): Promise<unknown> {
  const held = rustAgentConnection(root);
  if (!held) return undefined;
  try {
    const reply = await requestRust(
      held.connection,
      '/internal/collection-cursor',
      FRONT_DOOR_PRINCIPAL,
      held.active,
      input,
      'PUT',
      { cap: 32 * 1024 * 1024, deadlineMs: 126_000 }
    );
    if (
      reply.status === 404 ||
      (reply.status === 503 && ['shape', 'unsupported'].includes(reply.decline ?? ''))
    )
      return undefined;
    return reply.status === 200 ? reply.body : null;
  } catch (error) {
    const code = (error as NodeJS.ErrnoException | undefined)?.code;
    return code === 'ECONNREFUSED' || code === 'ENOENT' ? undefined : null;
  }
}

/** Read a provider cache by a private binding. Undefined leaves the reference reader in charge. */
export async function readRustProviderCache(
  root: string,
  input:
    | { profileId: string; manifestHash: string }
    | { bindingHash: string }
    | { walletSource: true }
    | { cursorFingerprint: string }
): Promise<unknown> {
  const held = rustAgentConnection(root);
  if (!held) return undefined;
  try {
    const reply = await requestRust(
      held.connection,
      '/internal/collection-provider-cache',
      FRONT_DOOR_PRINCIPAL,
      held.active,
      input,
      'PUT',
      { cap: 128 * 1024 }
    );
    return reply.status === 200 ? (reply.body as { value?: unknown } | null)?.value : undefined;
  } catch {
    return undefined;
  }
}

/** The collector's source registry is a private read bound to this boot root. */
export async function readRustCollectionSources(root: string): Promise<unknown> {
  const held = rustAgentConnection(root);
  if (!held) return null;
  try {
    const reply = await requestRust(
      held.connection,
      '/internal/collection-sources',
      FRONT_DOOR_PRINCIPAL,
      held.active
    );
    return reply.status === 200 ? reply.body : null;
  } catch {
    return null;
  }
}

/** Publish a background dashboard to the same boot and feed used by the interactive views. */
export async function publishCollectionFeed(
  root: string,
  source: string,
  generation: string,
  data: string,
  facts: string
): Promise<void> {
  const held = rustAgentConnection(root);
  if (held) {
    await pushRustFeedText(held.connection, source, generation, data, facts, held.active).catch(
      () => undefined
    );
  }
}

/** Hold the idle sampling interval on Rust's clock, with no pooled socket or read breaker. */
export function scheduleRustCollection(
  root: string,
  intervalMs: number,
  settled: (due: boolean) => void
): (() => void) | undefined {
  const held = rustAgentConnection(root);
  if (!held) return undefined;
  const stop = new AbortController();
  void once(
    { ...held.connection, agent: undefined },
    '/internal/collection-tick',
    FRONT_DOOR_PRINCIPAL,
    held.active,
    JSON.stringify({ intervalMs }),
    'PUT',
    1024,
    Date.now() + intervalMs + RUST_DEADLINE_MS,
    { signal: stop.signal }
  ).then(
    (reply) => {
      if (!stop.signal.aborted)
        settled(reply.status === 200 && (reply.body as { due?: unknown } | null)?.due === true);
    },
    () => {
      if (!stop.signal.aborted) settled(false);
    }
  );
  return () => stop.abort();
}

/** Save only through the owner selected at boot. A lost service leaves the in-memory quota usable. */
export async function writeRustCollectionCache(
  root: string,
  kind:
    | 'account-usage-observations'
    | 'claude-live-snapshots'
    | 'opencode-wallet-account'
    | 'account-usage-cooldown'
    | 'cursor-quota-observation',
  key: string,
  bytes: string
): Promise<boolean> {
  const held = rustAgentConnection(root);
  if (!held) return false;
  const id = `${kind}/${key}`;
  const write = async () => {
    try {
      const reply = await requestRust(
        held.connection,
        `/internal/collection-cache/${kind}/${encodeURIComponent(key)}`,
        FRONT_DOOR_PRINCIPAL,
        held.active,
        undefined,
        'PUT',
        { text: bytes, cap: 1024 }
      );
      return reply.status === 200 && (reply.body as { saved?: unknown } | null)?.saved === true;
    } catch {
      return false;
    }
  };
  // A later cooldown or completed result must not be overwritten by the
  // earlier "attempt started" record on a different socket.
  const pending = (held.cacheWrites.get(id) ?? Promise.resolve()).then(write);
  held.cacheWrites.set(id, pending);
  try {
    return await pending;
  } finally {
    if (held.cacheWrites.get(id) === pending) held.cacheWrites.delete(id);
  }
}

/** A native quota read may have reached the provider before its reply was lost. */
export class CollectionReadIndeterminate extends Error {
  constructor() {
    super('Account usage request failed.');
    this.name = 'CollectionReadIndeterminate';
  }
}

/**
 * POST is never retried: key-store may write. Undefined means an absent/old service, so the
 * labelled TS transport can run. Null means a handled refusal/failure and goes to the helper.
 * The caller's timeout includes this whole hop and keeps its Python reserve outside it.
 */
export async function requestRustAgent(
  root: string,
  verb: string,
  payload: {
    computer: unknown;
    args: unknown;
    deadlineMs: number;
    timeoutMs: number;
    maxBytes: number;
    callId: string;
  },
  onProgress?: (data: unknown) => void,
  singleShot = false,
  collect = false,
  signal?: AbortSignal
): Promise<string | null | undefined> {
  const held = rustAgentConnection(root);
  if (!held) return undefined;
  const bytes = JSON.stringify(payload);
  if (Buffer.byteLength(bytes) > 8 * 1024 * 1024 + 8192) return null;
  return new Promise((resolve, reject) => {
    const uncertain = () => {
      if (collect && ['quota-read', 'failover-collect'].includes(verb))
        reject(new CollectionReadIndeterminate());
      else if (singleShot)
        reject(new RustTransportError('reply', 'Agent write outcome unconfirmed'));
      else resolve(null);
    };
    const request = http.request({
      socketPath: held.connection.socketPath,
      signal,
      path: `/internal/${collect ? 'collect' : 'agent'}/${encodeURIComponent(verb)}`,
      method: 'POST',
      agent: held.connection.agent ?? false,
      headers: {
        'x-aac-boot-secret': held.connection.secret,
        'x-aac-principal': JSON.stringify(FRONT_DOOR_PRINCIPAL),
        'content-type': 'application/json',
        'content-length': Buffer.byteLength(bytes),
      },
    });
    held.active.add(request);
    const timer = setTimeout(
      () => request.destroy(new RustTransportError('deadline', 'Rust agent deadline')),
      payload.timeoutMs
    );
    request.once('close', () => {
      clearTimeout(timer);
      held.active.delete(request);
    });
    request.once('error', (error: NodeJS.ErrnoException) => {
      if (!signal?.aborted) held.health.failed?.(rustFailureKind(error));
      if (['ENOENT', 'ECONNREFUSED'].includes(error.code ?? '')) resolve(undefined);
      else uncertain();
    });
    request.once('response', (response) => {
      if (response.headers['x-aac-service-boot'] !== held.connection.bootId) {
        response.resume();
        held.health.failed?.('boot');
        uncertain();
        return;
      }
      // Headers can precede a stalled NDJSON body. Only a completed response resets L1's
      // consecutive transport timeouts, as on the ordinary read path.
      response.once('end', () => held.health.replied?.());
      if (response.statusCode === 404 || response.statusCode === 405) {
        response.resume();
        resolve(undefined);
        return;
      }
      if (
        response.statusCode !== 200 ||
        !String(response.headers['content-type']).startsWith('application/x-ndjson')
      ) {
        response.resume();
        uncertain();
        return;
      }
      let size = 0;
      let buffer = '';
      let terminal = false;
      let result: string | null = null;
      let fallbackProven = false;
      response.setEncoding('utf8');
      response.on('data', (chunk: string) => {
        size += Buffer.byteLength(chunk);
        if (size > payload.maxBytes) {
          request.destroy();
          uncertain();
          return;
        }
        buffer += chunk;
        let end: number;
        while ((end = buffer.indexOf('\n')) !== -1) {
          const text = buffer.slice(0, end);
          buffer = buffer.slice(end + 1);
          try {
            const event = JSON.parse(text);
            if (terminal || event.protocol !== 1 || event.id !== payload.callId)
              throw new RustTransportError('reply', 'Invalid agent stream');
            if (event.type === 'progress') {
              onProgress?.(event.data);
              continue;
            }
            terminal = true;
            if (event.type === 'result' && event.value && typeof event.value === 'object') {
              const value = JSON.stringify(event.value);
              fallbackProven = value === '{"fallback":"python"}';
              result = fallbackProven ? null : value;
            } else if (event.type === 'fallback') {
              fallbackProven =
                (collect ||
                  ['cursor-open', 'lifecycle-signin', 'muse-login-identity'].includes(verb)) &&
                ['shape', 'unavailable', 'busy', 'unsupported'].includes(event.reason);
            } else if (event.type !== 'failure') {
              throw new RustTransportError('reply', 'Invalid agent stream');
            }
          } catch {
            result = null;
            request.destroy();
            uncertain();
            return;
          }
        }
      });
      response.once('end', () => {
        if (terminal && buffer === '' && (result !== null || fallbackProven)) resolve(result);
        else uncertain();
      });
      response.once('error', uncertain);
      response.once('aborted', uncertain);
    });
    request.end(bytes);
  });
}

/**
 * M4c: one interactive control frame for a running agent call (`{"input":"..."}` or
 * `{"cancel":true}`). Input acknowledgement means queued; cancel acknowledgement is true
 * only after the agent's cancelled terminal frame and the child exit. Null when the
 * service is absent or the call is unknown; the caller keeps its labelled fallback.
 */
export async function controlRustAgent(
  root: string,
  callId: string,
  frame: { input: string } | { cancel: true }
): Promise<{ accepted?: boolean; cancelled?: boolean } | null | undefined> {
  const scope = canonicalRoot(root);
  const held = [...agentConnections.values()].reverse().find((entry) => entry.root === scope);
  if (!held) return undefined;
  const bytes = JSON.stringify(frame);
  if (Buffer.byteLength(bytes) > 65536) return null;
  const { promise, resolve } = Promise.withResolvers<{
    accepted?: boolean;
    cancelled?: boolean;
  } | null>();
  const request = http.request({
    socketPath: held.connection.socketPath,
    path: `/internal/agent/${encodeURIComponent(callId)}/control`,
    method: 'POST',
    agent: held.connection.agent ?? false,
    headers: {
      'x-aac-boot-secret': held.connection.secret,
      'x-aac-principal': JSON.stringify(FRONT_DOOR_PRINCIPAL),
      'content-type': 'application/json',
      'content-length': Buffer.byteLength(bytes),
    },
  });
  held.active.add(request);
  const timer = setTimeout(() => request.destroy(new Error('control deadline')), 2_500);
  request.once('close', () => {
    clearTimeout(timer);
    held.active.delete(request);
  });
  request.once('error', () => resolve(null));
  request.once('response', (response) => {
    if (response.statusCode !== 200) {
      response.resume();
      resolve(response.statusCode === 404 || response.statusCode === 405 ? undefined : null);
      return;
    }
    let body = '';
    response.setEncoding('utf8');
    response.on('data', (chunk: string) => {
      body += chunk;
      if (Buffer.byteLength(body) > 4096) request.destroy();
    });
    response.once('end', () => {
      try {
        resolve(JSON.parse(body));
      } catch {
        resolve(null);
      }
    });
    response.once('error', () => resolve(null));
  });
  request.end(bytes);
  return promise;
}

/**
 * R3: the store host's boot config as the supervisor resolved it (the open arguments TS
 * would have given the NAPI module, plus the backup job's share inputs). The lifecycle
 * adds `dir` (`<canonical root>/store`) and puts the whole object in the boot JSON.
 */
export interface RustStoreBootInput {
  bindingIdentity: unknown;
  retentionDays: number;
  cacheBytes?: number;
  activityRetentionDays?: number;
  snapshotDirectory: string | null;
  shareBackupDir: string | null;
  shareBackupOff: boolean;
}

/** The boot JSON's store object: the input plus its folder. */
export interface RustStoreBoot extends RustStoreBootInput {
  dir: string;
}

/**
 * G10: the boot JSON's renewal object — the homes the saved-login renewal timer reads,
 * resolved by TS at start (no home discovery in the service). Null keeps the timer in TS.
 */
export interface RustRenewalBoot {
  codexHome: string;
  homeDir: string;
}

interface RustStoreEntry {
  connection: RustConnection;
  active: Set<http.ClientRequest>;
  health: {
    replied?: () => void;
    failed?: (kind: RustFailureKind) => void;
    unresponsive?: () => void;
  };
}

/**
 * R3: per-root store-host state, owned by the supervisor, read by the store's socket
 * handle (the L5 agent-connection pattern). Only an owned boot is registered: an attached
 * boot belongs to another front door, whose store this process must not drive. Keys are
 * canonical roots, as the lease's connection carries.
 */
const storeHosts = new Map<
  string,
  { entry: RustStoreEntry | null; attached: boolean; gaveUp: boolean }
>();
const storeGaveUpHandlers = new Map<string, Set<() => void>>();

function storeRootKey(root: string): string {
  return canonicalRoot(root);
}

function storeHostState(root: string): {
  entry: RustStoreEntry | null;
  attached: boolean;
  gaveUp: boolean;
} {
  const key = storeRootKey(root);
  let state = storeHosts.get(key);
  if (!state) {
    state = { entry: null, attached: false, gaveUp: false };
    storeHosts.set(key, state);
  }
  return state;
}

/** The supervisor resolves the switch once at start; the store opens hosted only for it. */
export function setRustStoreHosted(root: string, hosted: boolean): void {
  const key = storeRootKey(root);
  if (hosted) storeHostState(key);
  else storeHosts.delete(key);
}

export function rustStoreHosted(root: string): boolean {
  return storeHosts.has(storeRootKey(root));
}

export function registerRustStoreConnection(
  root: string,
  connection: RustConnection,
  active: Set<http.ClientRequest>,
  health: RustStoreEntry['health'],
  owned: boolean
): () => void {
  const state = storeHostState(root);
  if (owned) state.entry = { connection, active, health };
  else state.attached = true;
  return () => {
    if (owned && state.entry?.connection === connection) state.entry = null;
    if (!owned) state.attached = false;
  };
}

export function rustStoreConnection(root: string): RustStoreEntry | null {
  return storeHosts.get(storeRootKey(root))?.entry ?? null;
}

/** Another front door owns the service (and its store): this process reads `lock_held`. */
export function rustStoreAttached(root: string): boolean {
  return storeHosts.get(storeRootKey(root))?.attached === true;
}

export function rustStoreGaveUp(root: string): boolean {
  return storeHosts.get(storeRootKey(root))?.gaveUp === true;
}

/** The supervisor used up its restart budget: the store falls back in process (design 1.3). */
export function notifyRustStoreGaveUp(root: string): void {
  const key = storeRootKey(root);
  storeHostState(key).gaveUp = true;
  for (const handler of storeGaveUpHandlers.get(key) ?? []) {
    try {
      handler();
    } catch {
      /* A handler never stops the supervisor. */
    }
  }
}

export function onRustStoreGaveUp(root: string, handler: () => void): () => void {
  const key = storeRootKey(root);
  let handlers = storeGaveUpHandlers.get(key);
  if (!handlers) {
    handlers = new Set();
    storeGaveUpHandlers.set(key, handlers);
  }
  handlers.add(handler);
  return () => handlers.delete(handler);
}

/** A store error the service answered with (`<code>: <message>`), as the binding rejects. */
export class RustStoreError extends Error {}

/** The longest store request (the host's cap; `captureReports` caps its batch lower). */
export const RUST_STORE_REQUEST_CAP = 16 * 1024 * 1024;
/** The longest store reply this front door reads (history answers are the big ones). */
export const RUST_STORE_REPLY_CAP = 64 * 1024 * 1024;

/** Read/control admission is bounded; long engine jobs are instead watched for liveness. */
export const RUST_STORE_DEADLINE_MS = 2500;
const STORE_LONG_JOBS = new Set([
  'backup',
  'maintain',
  'restore',
  'record',
  'captureReports',
  'activityIngest',
  'activityImportCodexV1',
  'activityBackfill',
  'runsScan',
  'runsRecord',
  'setMeta',
]);
const storeWatches = new WeakMap<
  RustStoreEntry,
  {
    requests: Set<StoreCallHandle>;
    timer: ReturnType<typeof setTimeout> | null;
    probing: boolean;
  }
>();

/** One independent, bounded health probe per boot while store calls are pending. */
function watchStore(entry: RustStoreEntry, request: StoreCallHandle): void {
  let state = storeWatches.get(entry);
  if (!state) {
    state = { requests: new Set(), timer: null, probing: false };
    storeWatches.set(entry, state);
  }
  const held = state;
  held.requests.add(request);
  const schedule = () => {
    if (held.timer || held.probing || !held.requests.size) return;
    held.timer = setTimeout(() => {
      held.timer = null;
      held.probing = true;
      void requestRust(
        { ...entry.connection, agent: undefined },
        '/api/health',
        PUBLIC_HEALTH_PRINCIPAL,
        entry.active
      )
        .then((reply) => {
          if (reply.status !== 200)
            throw new RustTransportError('reply', 'Rust store health failed');
        })
        .catch(() => {
          if (!held.requests.size) return;
          entry.health.unresponsive?.();
          // A lost response to a write is uncertain. Never retry it here.
          for (const pending of held.requests)
            pending.destroy(new RustTransportError('deadline', 'Rust store stopped answering'));
        })
        .finally(() => {
          held.probing = false;
          schedule();
        });
    }, 1000);
    held.timer.unref();
  };
  request.once('close', () => {
    held.requests.delete(request);
    if (!held.requests.size && held.timer) {
      clearTimeout(held.timer);
      held.timer = null;
    }
  });
  schedule();
}

/**
 * R3: one store method over the socket, the NAPI `call` contract: resolves to the answer's
 * JSON text, rejects with the store's `<code>: <message>` (a RustStoreError) or a
 * RustTransportError when the service did not answer. Read/control calls have a deadline;
 * long writes/backup keep running while the independent health probe answers. Never retried.
 */
export function requestRustStore(
  entry: RustStoreEntry,
  method: string,
  args: string
): Promise<string> {
  if (Buffer.byteLength(args) > RUST_STORE_REQUEST_CAP)
    return Promise.reject(new RustStoreError('invalid: store request too large'));
  const { connection } = entry;
  const { promise, handle } = storeSocketCall(
    connection.socketPath,
    connection.secret,
    FRONT_DOOR_PRINCIPAL_JSON,
    method,
    args,
    RUST_STORE_REPLY_CAP
  );
  watchStore(entry, handle);
  // The active set is the front door's shutdown drain: it only ever calls `destroy`, which a
  // store call implements like an HTTP request, so it joins the same set (and the same tests).
  const drained = handle as unknown as http.ClientRequest;
  entry.active.add(drained);
  const timer = !STORE_LONG_JOBS.has(method)
    ? setTimeout(
        () => handle.destroy(new RustTransportError('deadline', 'Rust store deadline')),
        RUST_STORE_DEADLINE_MS
      )
    : undefined;
  handle.once('close', () => {
    clearTimeout(timer);
    entry.active.delete(drained);
  });
  return promise.then(
    (reply) => {
      if (reply.boot !== connection.bootId) {
        entry.health.failed?.('boot');
        throw new RustTransportError('boot', 'Rust service boot mismatch');
      }
      entry.health.replied?.();
      if (reply.status === 200) return reply.text;
      try {
        const parsed = JSON.parse(reply.text) as { error?: unknown };
        if (typeof parsed?.error === 'string') throw new RustStoreError(parsed.error);
      } catch (error) {
        if (error instanceof RustStoreError) throw error;
        /* Not the store's error shape: a transport refusal below. */
      }
      throw new RustTransportError('reply', 'Rust store invalid response');
    },
    (error: unknown) => {
      entry.health.failed?.(rustFailureKind(error));
      throw error instanceof RustTransportError
        ? error
        : new RustTransportError(rustFailureKind(error), 'Rust store transport failed');
    }
  );
}

/**
 * G10: per-root renewal-timer ownership, the store-host pattern. The supervisor reserves
 * it before the first await of `start`; the TS renewal service defers while it is set and
 * takes the timer back when the service gives up. One writer: two renewal timers would
 * rotate the same refresh tokens twice.
 */
const renewalHostedRoots = new Map<string, boolean>();
const renewalHostedHandlers = new Map<string, Set<(hosted: boolean) => void>>();

function renewalRootKey(root: string): string {
  try {
    return fs.realpathSync(root);
  } catch {
    return root;
  }
}

/** The supervisor's reservation: true while the Rust boot owns this root's renewal timer. */
export function setRustRenewalHosted(root: string, hosted: boolean): void {
  const key = renewalRootKey(root);
  if (renewalHostedRoots.get(key) === hosted) return;
  if (hosted) renewalHostedRoots.set(key, true);
  else renewalHostedRoots.delete(key);
  for (const handler of renewalHostedHandlers.get(key) ?? []) {
    try {
      handler(hosted);
    } catch {
      /* A handler never stops the supervisor. */
    }
  }
}

/** Whether the Rust boot owns this root's renewal timer. */
export function rustRenewalHosted(root: string): boolean {
  return renewalHostedRoots.has(renewalRootKey(root));
}

/** Watch ownership changes; returns the unsubscribe. */
export function onRustRenewalHostedChange(
  root: string,
  handler: (hosted: boolean) => void
): () => void {
  const key = renewalRootKey(root);
  let handlers = renewalHostedHandlers.get(key);
  if (!handlers) {
    handlers = new Set();
    renewalHostedHandlers.set(key, handlers);
  }
  handlers.add(handler);
  return () => handlers.delete(handler);
}

/** The service is absent or gave up: the TS timer takes back over. */
export function notifyRustRenewalGaveUp(root: string): void {
  setRustRenewalHosted(root, false);
}
