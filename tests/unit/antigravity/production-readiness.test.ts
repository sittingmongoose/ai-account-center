import { afterEach, expect, test } from 'bun:test';
import { createServer, type Server, type Socket } from 'node:net';
import { chmodSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createUbuntuAntigravityDriver } from '../../../src/antigravity/ubuntu-driver';
import {
  AntigravityRuntimeStartupNotReadyError,
  createPrivateUnixRequest,
  createUbuntuRuntimeBridge,
  type UbuntuRuntimeBridge,
} from '../../../src/antigravity/ubuntu-runtime-bridge';
import { credentialFingerprint } from '../../../src/antigravity/registry';
import type {
  NativeCredential,
  RuntimeProof,
  VerifiedIdentity,
} from '../../../src/antigravity/types';

// All identities/credentials/proofs are invented; only disposable Unix sockets
// are opened. No real account reader, native CLI, provider or registry is used.
function fixture() {
  const current: NativeCredential = {
    format: 'fixture-only',
    bytes: Buffer.from('invented credential'),
  };
  const expected: VerifiedIdentity = {
    email: 'fixture@example.com',
    subject: '123456789',
    plan: null,
    source: 'provider-userinfo',
    verifiedAt: new Date().toISOString(),
  };
  const proof: RuntimeProof = {
    identity: { ...expected, source: 'native-runtime' },
    credentialFingerprint: credentialFingerprint(current),
    runtimeStarted: true,
    sessionRestored: true,
  };
  const state = {
    clock: 0,
    released: true,
    attempts: 0,
    reads: 0,
    validates: 0,
    waits: [] as number[],
    timeouts: [] as number[],
    observed: [] as VerifiedIdentity[],
    current,
    actual: { ...expected },
    proof,
    pending: 0,
    failure: null as unknown,
    onPause: () => {},
    onProof: () => {},
  };
  const bridge = {
    canProveRuntimeIdentity: async () => true,
    proveRuntimeIdentity: async (identity: VerifiedIdentity, timeoutMs?: number) => {
      state.attempts++;
      state.observed.push(identity);
      state.timeouts.push(timeoutMs ?? 0);
      state.onProof();
      if (state.failure) throw state.failure;
      if (state.attempts <= state.pending) throw new AntigravityRuntimeStartupNotReadyError();
      return state.proof;
    },
  } as UbuntuRuntimeBridge;
  const driver = createUbuntuAntigravityDriver({
    nativeStore: {
      read: async () => {
        state.reads++;
        return state.current;
      },
      install: async () => {
        throw new Error('fixture forbidden install');
      },
      rollback: async () => false,
    },
    quotaWorker: {
      validateCredential: async () => {
        state.validates++;
        return state.actual;
      },
    },
    bridge,
    releaseGate: async () => state.released,
    startupClock: () => state.clock,
    pauseStartup: async (milliseconds) => {
      state.waits.push(milliseconds);
      state.clock += milliseconds;
      state.onPause();
    },
  });
  return { state, driver, expected };
}

test('owned startup readiness retries earn one final exact current identity validation', async () => {
  const f = fixture();
  f.state.pending = 2;
  expect(await f.driver.proveRuntimeIdentity(f.expected)).toEqual(f.state.proof);
  expect(f.state.attempts).toBe(3);
  expect(f.state.waits).toEqual([25, 25]);
  expect(f.state.timeouts).toEqual([10000, 9975, 9950]);
  expect(f.state.reads).toBe(1);
  expect(f.state.validates).toBe(1);
});

test('readiness budget expires without native writes or final account requests', async () => {
  const f = fixture();
  f.state.pending = 10;
  f.state.onProof = () => {
    f.state.clock += 4990;
  };
  await expect(f.driver.proveRuntimeIdentity(f.expected)).rejects.toThrow(
    'antigravity-runtime-unavailable'
  );
  expect(f.state.attempts).toBe(2);
  expect(f.state.reads).toBe(0);
  expect(f.state.validates).toBe(0);
  expect(f.state.waits).toEqual([25]);
});

test('generic and forged readiness errors fail immediately without retry or private error projection', async () => {
  for (const failure of [
    new Error('fixture private canary'),
    Object.assign(new Error('runtime-startup-not-ready'), { code: 'runtime-startup-not-ready' }),
  ]) {
    const f = fixture();
    f.state.failure = failure;
    await expect(f.driver.proveRuntimeIdentity(f.expected)).rejects.toThrow();
    expect(f.state.attempts).toBe(1);
    expect(f.state.waits).toEqual([]);
    expect(f.state.reads).toBe(0);
  }
});

test('release revocation while waiting fails before another startup request', async () => {
  const f = fixture();
  f.state.pending = 10;
  f.state.onPause = () => {
    f.state.released = false;
  };
  await expect(f.driver.proveRuntimeIdentity(f.expected)).rejects.toThrow(
    'antigravity-runtime-unavailable'
  );
  expect(f.state.attempts).toBe(1);
  expect(f.state.reads).toBe(0);
});

test('expected identity stays bound through asynchronous readiness wait', async () => {
  const f = fixture();
  f.state.pending = 1;
  f.state.onPause = () => {
    f.expected.subject = '987654321';
  };
  expect(await f.driver.proveRuntimeIdentity(f.expected)).toEqual(f.state.proof);
  expect(f.state.observed.map((identity) => identity.subject)).toEqual(['123456789', '123456789']);
});

test('known foreign current identity or changed revision after proof is refused without retry', async () => {
  for (const mutate of [
    (f: ReturnType<typeof fixture>) => {
      f.state.actual.subject = '987654321';
    },
    (f: ReturnType<typeof fixture>) => {
      f.state.current = { ...f.state.current, bytes: Buffer.from('foreign revision') };
    },
    (f: ReturnType<typeof fixture>) => {
      f.state.proof.identity.subject = '987654321';
    },
  ]) {
    const f = fixture();
    mutate(f);
    await expect(f.driver.proveRuntimeIdentity(f.expected)).rejects.toThrow(
      'antigravity-runtime-unavailable'
    );
    expect(f.state.attempts).toBe(1);
    expect(f.state.waits).toEqual([]);
    expect(f.state.reads).toBe(1);
    expect(f.state.validates).toBe(1);
  }
});

test('even a success arriving after the startup deadline is not accepted', async () => {
  const f = fixture();
  f.state.onProof = () => {
    f.state.clock = 10001;
  };
  await expect(f.driver.proveRuntimeIdentity(f.expected)).rejects.toThrow(
    'antigravity-runtime-unavailable'
  );
  expect(f.state.reads).toBe(0);
  expect(f.state.attempts).toBe(1);
});

const sockets: Array<{ server: Server; directory: string; clients: Set<Socket> }> = [];
afterEach(async () => {
  for (const { server, directory, clients } of sockets.splice(0)) {
    for (const client of clients) client.destroy();
    await new Promise<void>((resolve) => server.close(() => resolve()));
    rmSync(directory, { recursive: true, force: true });
  }
});

async function socketFixture(reply: (request: Record<string, unknown>, socket: Socket) => void) {
  const directory = mkdtempSync(join(tmpdir(), 'aac-ready-'));
  chmodSync(directory, 0o700);
  const socketPath = join(directory, 'c');
  const clients = new Set<Socket>();
  const server = createServer((socket) => {
    clients.add(socket);
    socket.on('close', () => clients.delete(socket));
    socket.on('error', () => {});
    let pending = Buffer.alloc(0);
    socket.on('data', (chunk) => {
      pending = Buffer.concat([pending, chunk]);
      if (pending.length < 4 || pending.length < pending.readUInt32BE(0) + 4) return;
      reply(JSON.parse(pending.subarray(4).toString('utf8')) as Record<string, unknown>, socket);
    });
  });
  sockets.push({ server, directory, clients });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(socketPath, resolve);
  });
  chmodSync(socketPath, 0o600);
  return socketPath;
}

function packet(message: unknown) {
  const body = Buffer.from(JSON.stringify(message));
  const length = Buffer.alloc(4);
  length.writeUInt32BE(body.length);
  return Buffer.concat([length, body]);
}

test('actual private framed prove response alone earns typed startup readiness', async () => {
  const socketPath = await socketFixture((request, socket) => {
    socket.end(packet({ requestId: request.requestId, error: 'runtime-startup-not-ready' }));
  });
  const bridge = createUbuntuRuntimeBridge({ socketPath });
  await expect(bridge.proveRuntimeIdentity(fixture().expected)).rejects.toBeInstanceOf(
    AntigravityRuntimeStartupNotReadyError
  );
});

test('wrong request ID, extra/private fields, unknown failures and other methods never earn readiness', async () => {
  for (const kind of ['wrong-id', 'extra', 'unknown', 'ordinary-error', 'inspect']) {
    const socketPath = await socketFixture((request, socket) => {
      socket.end(
        packet({
          requestId: kind === 'wrong-id' ? 'foreign' : request.requestId,
          error:
            kind === 'unknown'
              ? 'fixture private canary'
              : kind === 'ordinary-error'
                ? 'runtime-proof-unavailable'
                : 'runtime-startup-not-ready',
          ...(kind === 'extra' ? { private: 'fixture private canary' } : {}),
        })
      );
    });
    const request = createPrivateUnixRequest(socketPath);
    let error: unknown;
    try {
      await request(kind === 'inspect' ? 'inspect' : 'prove', {});
    } catch (caught) {
      error = caught;
    }
    expect(error).toBeInstanceOf(Error);
    expect(error).not.toBeInstanceOf(AntigravityRuntimeStartupNotReadyError);
    expect(String(error)).not.toContain('canary');
  }
});

test('closed peer and malformed frames are hard failures', async () => {
  for (const mode of ['closed', 'malformed']) {
    const socketPath = await socketFixture((_request, socket) => {
      socket.end(mode === 'closed' ? undefined : Buffer.from('invalid frame'));
    });
    let error: unknown;
    try {
      await createPrivateUnixRequest(socketPath)('prove', {});
    } catch (caught) {
      error = caught;
    }
    expect(error).toBeInstanceOf(Error);
    expect(error).not.toBeInstanceOf(AntigravityRuntimeStartupNotReadyError);
  }
});

test('absolute private request deadline rejects a dripping frame rather than resetting its budget', async () => {
  const socketPath = await socketFixture((request, socket) => {
    const frame = packet({ requestId: request.requestId, error: 'runtime-startup-not-ready' });
    let offset = 0;
    const timer = setInterval(() => {
      if (offset < frame.length) socket.write(frame.subarray(offset, ++offset));
    }, 10);
    socket.once('close', () => clearInterval(timer));
  });
  const started = performance.now();
  let error: unknown;
  try {
    await createPrivateUnixRequest(socketPath, 500)('prove', {}, 35);
  } catch (caught) {
    error = caught;
  }
  expect(error).toBeInstanceOf(Error);
  expect(error).not.toBeInstanceOf(AntigravityRuntimeStartupNotReadyError);
  expect(performance.now() - started).toBeLessThan(250);
});
