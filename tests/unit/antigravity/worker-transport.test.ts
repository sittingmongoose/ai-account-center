import { describe, expect, test } from 'bun:test';
import type { ChildProcessWithoutNullStreams } from 'child_process';
import { createHash } from 'crypto';
import { EventEmitter } from 'events';
import { PassThrough, Writable } from 'stream';
import {
  createAntigravityQuotaWorker,
  type AntigravityWorkerOptions,
  type PrivateWorkerSpawner,
} from '../../../src/antigravity/quota-worker-transport';
import { credentialFingerprint, identityKey } from '../../../src/antigravity/registry';
import type {
  SavedQuotaRequest,
  SavedQuotaSnapshot,
} from '../../../src/antigravity/runtime-composition';
import type { NativeCredential, VerifiedIdentity } from '../../../src/antigravity/types';

const SAFE_FAILURE = 'Antigravity private account request failed safely.';
const FIXTURE_PRIVATE = 'invented-fixture-only-native-credential';
const STDERR_SENTINEL = 'invented-fixture-only-worker-secret';
const MAX_CREDENTIAL_BYTES = 16_384;
const MAX_RESPONSE_BYTES = 65_536;
const SCRIPT = '/fixture-only/owned/quota_worker.py';
const COLLECTORS = '/fixture-only/owned/account-usage';
const NOW = '2026-10-01T16:00:00.000Z';

type WorkerResponse = (child: FakeChild, packet: Record<string, unknown>) => void;

/** In-memory pipes and events only: no process, filesystem, environment, or provider access. */
class FakeChild extends EventEmitter {
  readonly stdout = new PassThrough();
  readonly stderr = new PassThrough();
  readonly stdin: Writable;
  readonly requests: Buffer[] = [];
  readonly signals: string[] = [];
  exitCode: number | null = null;
  stderrResumed = false;

  constructor(response: WorkerResponse) {
    super();
    const originalResume = this.stderr.resume.bind(this.stderr);
    this.stderr.resume = () => {
      this.stderrResumed = true;
      return originalResume();
    };
    this.stdin = new Writable({
      write: (chunk: Buffer | string, _encoding, callback) => {
        this.requests.push(Buffer.isBuffer(chunk) ? Buffer.from(chunk) : Buffer.from(chunk));
        callback();
      },
      final: (callback) => {
        callback();
        queueMicrotask(() => {
          const packet = JSON.parse(Buffer.concat(this.requests).toString('utf8'));
          response(this, packet);
        });
      },
    });
  }

  respond(value: unknown, code = 0): void {
    this.respondBytes(Buffer.from(`${JSON.stringify(value)}\n`), code);
  }

  respondBytes(bytes: Buffer | string, code = 0): void {
    this.stdout.emit('data', bytes);
    this.close(code);
  }

  close(code: number): void {
    this.exitCode = code;
    this.emit('close', code);
  }

  kill(signal = 'SIGTERM'): boolean {
    this.signals.push(signal);
    return true;
  }
}

interface Launch {
  binary: string;
  args: string[];
  options: Parameters<PrivateWorkerSpawner>[2];
  child: FakeChild;
}

class FixtureTimers {
  readonly pending = new Map<number, () => void>();
  readonly milliseconds: number[] = [];
  readonly cleared: unknown[] = [];
  private nextToken = 1;

  readonly set = (callback: () => void, milliseconds: number): unknown => {
    const token = this.nextToken++;
    this.pending.set(token, callback);
    this.milliseconds.push(milliseconds);
    return token;
  };

  readonly clear = (token: unknown): void => {
    this.cleared.push(token);
    if (typeof token === 'number') this.pending.delete(token);
  };

  fireNext(): void {
    const callback = this.pending.values().next().value;
    if (callback) callback();
  }
}

function fixtureCredential(bytes = Buffer.from(FIXTURE_PRIVATE)): NativeCredential {
  return { format: 'fixture-native-json', bytes };
}

function fixtureIdentity(): VerifiedIdentity {
  return {
    email: 'party@example.com',
    subject: 'invented-authoritative-fixture-subject',
    plan: 'fixture-pro-plan',
    verifiedAt: NOW,
    source: 'provider-userinfo',
  };
}

function quotaRequest(credential = fixtureCredential()): SavedQuotaRequest {
  const identity = fixtureIdentity();
  return {
    profileId: 'party',
    email: identity.email,
    identity,
    identityKey: identityKey(identity),
    credentialRevision: credentialFingerprint(credential),
    credential,
  };
}

function identityResponse(credential = fixtureCredential()): Record<string, unknown> {
  const identity = fixtureIdentity();
  return {
    ...identity,
    identityKey: identityKey(identity),
    credentialRevision: credentialFingerprint(credential),
  };
}

function quotaResponse(request = quotaRequest()): SavedQuotaSnapshot {
  return {
    profileId: request.profileId,
    email: request.email,
    identityKey: request.identityKey,
    credentialRevision: request.credentialRevision,
    plan: request.identity.plan,
    source: 'native-consumer',
    status: 'fresh',
    identityVerified: true,
    identityValidation: 'verified',
    fetchedAt: NOW,
    sampledAt: NOW,
    windows: [
      {
        key: 'fixture-model-quota',
        label: 'Fixture model quota',
        remainingPercent: 74.125,
        resetAt: '2026-10-01T21:00:00.000Z',
        modelIds: ['fixture-model'],
        poolId: 'fixture-model-quota',
        poolIdSource: 'provider-id',
      },
    ],
    pools: [
      {
        id: 'fixture-model-quota',
        idSource: 'provider-id',
        eligibility: 'reported-quota',
        complete: true,
        windows: [
          {
            key: 'fixture-model-quota',
            kind: 'rate_limit',
            remainingPercent: 74.125,
            resetAt: '2026-10-01T21:00:00.000Z',
          },
        ],
      },
    ],
  };
}

function harness(response: WorkerResponse, extra: Partial<AntigravityWorkerOptions> = {}) {
  const launches: Launch[] = [];
  const spawnWorker: PrivateWorkerSpawner = (binary, args, options) => {
    const child = new FakeChild(response);
    launches.push({ binary, args: [...args], options, child });
    return child as unknown as ChildProcessWithoutNullStreams;
  };
  return {
    launches,
    worker: createAntigravityQuotaWorker({
      scriptPath: SCRIPT,
      collectorDirectory: COLLECTORS,
      spawnWorker,
      ...extra,
    }),
  };
}

async function expectSafeRejection(pending: Promise<unknown>): Promise<void> {
  const error: unknown = await pending.then(
    () => undefined,
    (caught: unknown) => caught
  );
  expect(error).toBeInstanceOf(Error);
  expect((error as Error).message).toBe(SAFE_FAILURE);
  expect(String(error)).not.toContain(STDERR_SENTINEL);
  expect(String(error)).not.toContain(FIXTURE_PRIVATE);
}

describe('Antigravity private quota worker transport, invented fixtures only', () => {
  test('construction does not spawn a worker or read native account data', () => {
    const { launches } = harness((child) => child.respond({}));
    expect(launches).toEqual([]);
  });

  for (const options of [
    { scriptPath: 'relative/quota_worker.py' },
    { collectorDirectory: 'relative/account-usage' },
  ]) {
    test(`rejects relative trusted package path ${Object.keys(options)[0]}`, () => {
      expect(() => harness((child) => child.respond({}), options)).toThrow(SAFE_FAILURE);
    });
  }

  test('uses fixed isolated Python invocation and keeps credentials off argv and spawn environment', async () => {
    const request = quotaRequest();
    const { launches, worker } = harness((child) => child.respond(quotaResponse(request)));
    const result = await worker.collectQuota(request);
    expect(launches).toHaveLength(1);
    const launched = launches[0];
    expect(launched.binary).toBe('/usr/bin/python3');
    expect(launched.args).toEqual(['-I', SCRIPT, '--collector-dir', COLLECTORS]);
    expect(launched.options).toEqual({ stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    expect(Object.keys(launched.options)).not.toContain('env');
    const launchDescription = JSON.stringify({
      binary: launched.binary,
      args: launched.args,
      options: launched.options,
    });
    for (const privateValue of [
      FIXTURE_PRIVATE,
      request.credential.bytes.toString('base64'),
      request.email,
      request.identity.subject,
      request.identityKey,
      request.credentialRevision,
    ])
      expect(launchDescription).not.toContain(privateValue);
    expect(launched.child.stderrResumed).toBe(true);
    expect(result).toEqual(quotaResponse(request));
  });

  test('quota stdin contains the exact private operation and transaction identity bindings', async () => {
    const request = quotaRequest();
    let received: Record<string, unknown> | undefined;
    const { launches, worker } = harness((child, packet) => {
      received = packet;
      child.respond(quotaResponse(request));
    });
    await worker.collectQuota(request);
    expect(received).toEqual({
      operation: 'quota',
      profileId: request.profileId,
      email: request.email,
      identityKey: request.identityKey,
      credentialRevision: request.credentialRevision,
      credentialBase64: request.credential.bytes.toString('base64'),
    });
    expect(Buffer.from(received!.credentialBase64 as string, 'base64')).toEqual(
      request.credential.bytes
    );
    expect(received).not.toHaveProperty('subject');
    expect(received).not.toHaveProperty('credential');
    expect(Buffer.concat(launches[0].child.requests).length).toBeLessThanOrEqual(
      MAX_RESPONSE_BYTES
    );
  });

  test('identity validation sends only its operation and encoded credential on stdin', async () => {
    const credential = fixtureCredential();
    let received: Record<string, unknown> | undefined;
    const { worker } = harness((child, packet) => {
      received = packet;
      child.respond(identityResponse(credential));
    });
    expect(await worker.validateCredential(credential)).toEqual(fixtureIdentity());
    expect(received).toEqual({
      operation: 'identity',
      credentialBase64: credential.bytes.toString('base64'),
    });
  });

  test('uses the transaction canonical identity SHA-256 and exact saved byte revision', async () => {
    const request = quotaRequest();
    request.identity.email = 'Party@Example.COM';
    const expectedIdentityHash = createHash('sha256')
      .update(JSON.stringify(['antigravity', 'party@example.com', request.identity.subject]))
      .digest('hex');
    const expectedCredentialHash = createHash('sha256')
      .update(request.credential.bytes)
      .digest('hex');
    expect(request.identityKey).toBe(expectedIdentityHash);
    expect(request.credentialRevision).toBe(expectedCredentialHash);
    const { launches, worker } = harness((child) => child.respond(quotaResponse(request)));
    await worker.collectQuota(request);
    expect(launches).toHaveLength(1);
  });

  for (const size of [1, MAX_CREDENTIAL_BYTES]) {
    test(`accepts an owned credential at the ${size}-byte allowed boundary`, async () => {
      const credential = fixtureCredential(Buffer.alloc(size, 'f'));
      const { launches, worker } = harness((child) => child.respond(identityResponse(credential)));
      expect(await worker.validateCredential(credential)).toEqual(fixtureIdentity());
      const packet = JSON.parse(Buffer.concat(launches[0].child.requests).toString('utf8'));
      expect(Buffer.from(packet.credentialBase64, 'base64')).toEqual(credential.bytes);
    });
  }

  for (const credential of [
    fixtureCredential(Buffer.alloc(0)),
    fixtureCredential(Buffer.alloc(MAX_CREDENTIAL_BYTES + 1, 'f')),
    {
      format: 'fixture-native-json',
      bytes: new Uint8Array([1, 2, 3]),
    } as unknown as NativeCredential,
  ]) {
    test(`rejects an invalid credential buffer of ${credential.bytes.length} bytes before spawning`, async () => {
      const { launches, worker } = harness((child) => child.respond({}));
      await expectSafeRejection(worker.validateCredential(credential));
      expect(launches).toEqual([]);
    });
  }

  for (const mutate of [
    (request: SavedQuotaRequest) => {
      request.identityKey = '0'.repeat(64);
    },
    (request: SavedQuotaRequest) => {
      request.credentialRevision = '0'.repeat(64);
    },
    (request: SavedQuotaRequest) => {
      request.identityKey = request.identityKey.toUpperCase();
    },
    (request: SavedQuotaRequest) => {
      request.credentialRevision = 'invalid-revision';
    },
    (request: SavedQuotaRequest) => {
      request.identity.subject = 'a-different-authoritative-subject';
    },
    (request: SavedQuotaRequest) => {
      request.credential.bytes = Buffer.from('changed-fixture-bytes');
    },
    (request: SavedQuotaRequest) => {
      request.profileId = '../foreign-profile';
    },
  ]) {
    test(`rejects mismatched or unsafe saved request before spawning: ${mutate.toString().split('{')[1]?.trim().split(';')[0]}`, async () => {
      const request = quotaRequest();
      mutate(request);
      const { launches, worker } = harness((child) => child.respond({}));
      await expectSafeRejection(worker.collectQuota(request));
      expect(launches).toEqual([]);
    });
  }

  test('rejects a quota email that is not the checked authoritative identity before spawning', async () => {
    const request = { ...quotaRequest(), email: 'another@example.com' };
    const { launches, worker } = harness((child) => child.respond({}));
    await expectSafeRejection(worker.collectQuota(request));
    expect(launches).toEqual([]);
  });

  test('rejects an over-budget request packet before spawning', async () => {
    const request = { ...quotaRequest(), email: 'f'.repeat(MAX_RESPONSE_BYTES + 1) };
    const { launches, worker } = harness((child) => child.respond({}));
    await expectSafeRejection(worker.collectQuota(request));
    expect(launches).toEqual([]);
  });

  test('accepts a 65KB response plus its single trailing newline', async () => {
    const request = quotaRequest();
    const output = `${JSON.stringify(quotaResponse(request)).padEnd(MAX_RESPONSE_BYTES, ' ')}\n`;
    expect(Buffer.byteLength(output)).toBe(MAX_RESPONSE_BYTES + 1);
    const { worker } = harness((child) => {
      child.stdout.emit('data', Buffer.from(output.slice(0, 1024)));
      child.respondBytes(output.slice(1024));
    });
    expect(await worker.collectQuota(request)).toEqual(quotaResponse(request));
  });

  test('bounds aggregate response bytes across chunks and stops only the owned fake worker', async () => {
    const { launches, worker } = harness((child) => {
      child.stdout.emit('data', Buffer.alloc(MAX_RESPONSE_BYTES, ' '));
      child.stdout.emit('data', Buffer.alloc(2, ' '));
    });
    await expectSafeRejection(worker.collectQuota(quotaRequest()));
    expect(launches).toHaveLength(1);
    expect(launches[0].child.signals).toEqual(['SIGTERM']);
  });

  test('response byte bound counts UTF-8 bytes rather than string characters', async () => {
    const output = JSON.stringify({ fixture: 'é'.repeat(MAX_RESPONSE_BYTES / 2) });
    expect(output.length).toBeLessThan(MAX_RESPONSE_BYTES);
    expect(Buffer.byteLength(output)).toBeGreaterThan(MAX_RESPONSE_BYTES + 1);
    const { launches, worker } = harness((child) => child.respondBytes(output));
    await expectSafeRejection(worker.collectQuota(quotaRequest()));
    expect(launches[0].child.signals).toEqual(['SIGTERM']);
  });

  for (const output of ['', '{invalid-json', 'null', '[]', '42', '"fixture-text"', '{}\n{}\n']) {
    test(`rejects malformed or non-record response ${JSON.stringify(output)} with the fixed safe error`, async () => {
      const { worker } = harness((child) => child.respondBytes(output));
      await expectSafeRejection(worker.collectQuota(quotaRequest()));
    });
  }

  test('discards raw worker stderr during a successful internal quota response', async () => {
    const request = quotaRequest();
    const { launches, worker } = harness((child) => {
      child.stderr.write(STDERR_SENTINEL);
      child.respond(quotaResponse(request));
    });
    const result = await worker.collectQuota(request);
    expect(launches[0].child.stderrResumed).toBe(true);
    expect(JSON.stringify(result)).not.toContain(STDERR_SENTINEL);
    expect(result).toEqual(quotaResponse(request));
  });

  test('nonzero worker exit cannot expose stderr or an otherwise valid response', async () => {
    const { worker } = harness((child) => {
      child.stderr.write(STDERR_SENTINEL);
      child.respond(quotaResponse(), 7);
    });
    await expectSafeRejection(worker.collectQuota(quotaRequest()));
  });

  test('a synchronous spawn error is replaced by the fixed safe error', async () => {
    const { worker } = harness((child) => child.respond({}), {
      spawnWorker: () => {
        throw new Error(STDERR_SENTINEL);
      },
    });
    await expectSafeRejection(worker.collectQuota(quotaRequest()));
  });

  test('an asynchronous process error is replaced by the fixed safe error', async () => {
    const { worker } = harness((child) => child.emit('error', new Error(STDERR_SENTINEL)));
    await expectSafeRejection(worker.collectQuota(quotaRequest()));
  });

  test('an asynchronous stdin error uses the fixed safe error and stops its owned worker', async () => {
    const { launches, worker } = harness((child) =>
      child.stdin.emit('error', new Error(STDERR_SENTINEL))
    );
    await expectSafeRejection(worker.collectQuota(quotaRequest()));
    expect(launches[0].child.signals).toEqual(['SIGTERM']);
  });

  test('the 25-second deadline is tested with an injected clock and stops only its owned worker', async () => {
    const timers = new FixtureTimers();
    const { launches, worker } = harness(() => undefined, {
      setTimer: timers.set,
      clearTimer: timers.clear,
    });
    const pending = worker.collectQuota(quotaRequest());
    expect(timers.milliseconds).toEqual([25_000]);
    expect(timers.pending.size).toBe(1);
    timers.fireNext();
    await expectSafeRejection(pending);
    expect(launches[0].child.signals).toEqual(['SIGTERM']);
    expect(timers.pending.size).toBe(0);
    expect(timers.cleared).toEqual([1]);
  });

  test('a successful response cancels the owned deadline once and ignores later events', async () => {
    const timers = new FixtureTimers();
    const { launches, worker } = harness((child) => child.respond(quotaResponse()), {
      setTimer: timers.set,
      clearTimer: timers.clear,
    });
    expect(await worker.collectQuota(quotaRequest())).toEqual(quotaResponse());
    const child = launches[0].child;
    child.stdout.emit('data', Buffer.alloc(MAX_RESPONSE_BYTES + 2));
    child.stdin.emit('error', new Error(STDERR_SENTINEL));
    child.emit('error', new Error(STDERR_SENTINEL));
    timers.fireNext();
    expect(child.signals).toEqual([]);
    expect(timers.pending.size).toBe(0);
    expect(timers.cleared).toEqual([1]);
  });

  test('a helper already exited before its deadline is never signalled', async () => {
    const timers = new FixtureTimers();
    const { launches, worker } = harness(() => undefined, {
      setTimer: timers.set,
      clearTimer: timers.clear,
    });
    const pending = worker.collectQuota(quotaRequest());
    launches[0].child.exitCode = 0;
    timers.fireNext();
    await expectSafeRejection(pending);
    expect(launches[0].child.signals).toEqual([]);
    expect(timers.pending.size).toBe(0);
  });

  test('a failed owned-helper signal cannot expose the raw failure', async () => {
    const timers = new FixtureTimers();
    const { launches, worker } = harness(() => undefined, {
      setTimer: timers.set,
      clearTimer: timers.clear,
    });
    const pending = worker.collectQuota(quotaRequest());
    launches[0].child.kill = () => {
      throw new Error(STDERR_SENTINEL);
    };
    timers.fireNext();
    await expectSafeRejection(pending);
    expect(timers.pending.size).toBe(0);
  });

  test('a synchronous stdin failure cannot escape raw diagnostics', async () => {
    const { launches, worker } = harness((child) => child.respond({}), {
      spawnWorker: (binary, args, options) => {
        const child = new FakeChild(() => undefined);
        launches.push({ binary, args, options, child });
        child.stdin.end = (() => {
          throw new Error(STDERR_SENTINEL);
        }) as Writable['end'];
        return child as unknown as ChildProcessWithoutNullStreams;
      },
    });
    await expectSafeRejection(worker.collectQuota(quotaRequest()));
    expect(launches[0].child.signals).toEqual(['SIGTERM']);
  });

  test('returns the private quota snapshot to composition without calling it a public HTTP response', async () => {
    const request = quotaRequest();
    const internalSnapshot = {
      ...quotaResponse(request),
      status: 'cached' as const,
      retryAfterSeconds: 600,
    };
    const { worker } = harness((child) => child.respond(internalSnapshot));
    const result = await worker.collectQuota(request);
    expect(result).toEqual(internalSnapshot);
    expect(result.identityKey).toBe(request.identityKey);
    expect(result.credentialRevision).toBe(request.credentialRevision);
    expect(result.windows[0]).toMatchObject({ remainingPercent: 74.125 });
    expect(result.status).toBe('cached');
    expect(result.sampledAt).toBe(NOW);
  });

  for (const source of ['provider-userinfo', 'native-runtime'] as const) {
    test(`accepts an authoritative ${source} identity with matching canonical key and saved-byte fingerprint`, async () => {
      const credential = fixtureCredential();
      const result = identityResponse(credential);
      result.source = source;
      result.email = 'Party@EXAMPLE.com';
      result.verifiedAt = '2026-10-01T12:00:00-04:00';
      const { worker } = harness((child) => child.respond(result));
      expect(await worker.validateCredential(credential)).toEqual({ ...fixtureIdentity(), source });
    });
  }

  for (const change of [
    { identityKey: '0'.repeat(64) },
    { credentialRevision: '0'.repeat(64) },
    { credentialRevision: undefined },
    { identityKey: undefined },
    { email: 'another@example.com' },
    { subject: 'different-authoritative-subject' },
    { email: 'invalid-address' },
    { subject: '' },
    { subject: 'fixture\nsubject' },
    { verifiedAt: 'invalid-date' },
    { source: 'decoded-jwt' },
    { plan: '\ninvalid-plan' },
  ]) {
    test(`rejects an unbound or invalid authoritative identity response ${Object.keys(change)[0]}=${String(Object.values(change)[0])}`, async () => {
      const { worker } = harness((child) => child.respond({ ...identityResponse(), ...change }));
      await expectSafeRejection(worker.validateCredential(fixtureCredential()));
    });
  }

  test('invalid saved identity fields fail safely before any child is created', async () => {
    const request = quotaRequest();
    request.identity.email = STDERR_SENTINEL;
    const { launches, worker } = harness((child) => child.respond({}));
    await expectSafeRejection(worker.collectQuota(request));
    expect(launches).toEqual([]);
  });

  test('malformed private credential values fail safely before creating a child', async () => {
    const { launches, worker } = harness((child) => child.respond({}));
    await expectSafeRejection(worker.validateCredential(null as unknown as NativeCredential));
    await expectSafeRejection(
      worker.collectQuota({ ...quotaRequest(), credential: null as unknown as NativeCredential })
    );
    await expectSafeRejection(
      worker.collectQuota({
        ...quotaRequest(),
        credential: {
          format: 'fixture-native-json',
          bytes: undefined,
        } as unknown as NativeCredential,
      })
    );
    expect(launches).toEqual([]);
  });
});
