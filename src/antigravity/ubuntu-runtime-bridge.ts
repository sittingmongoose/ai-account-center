import { AntigravityError } from './errors';
/** Uninstalled bridge to one already-resident same-user Ubuntu PTY broker. */
import { lstat } from 'node:fs/promises';
import { connect } from 'node:net';
import { dirname } from 'node:path';
import { randomUUID } from 'node:crypto';
import type {
  ProcessPlan,
  RuntimeProof,
  StopReceipt,
  VerifiedIdentity,
  QuiescedHostProof,
} from './types';
import type { AntigravityAutoHostCensus } from './auto-switch/types';

const MAX_FRAME_BYTES = 256 * 1024;
const SHA256 = /^[a-f0-9]{64}$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
type Method =
  | 'capability'
  | 'inspect'
  | 'stop'
  | 'restart'
  | 'prove'
  | 'stop-owned-restarts'
  | 'census'
  | 'approve-idle-plan'
  | 'approve-quiesced-stop'
  | 'complete-transaction';
type Request = (method: Method, parameters?: Record<string, unknown>) => Promise<unknown>;

export interface UbuntuRuntimeBridge {
  /** Private monitor census; cached checkpoints never supply fresh idle state. */
  readHostCensus(): Promise<AntigravityAutoHostCensus>;
  canProveRuntimeIdentity(): Promise<boolean>;
  inspectProcesses(): Promise<ProcessPlan>;
  stopProcesses(plan: ProcessPlan): Promise<StopReceipt>;
  restartProcesses(receipt: StopReceipt): Promise<void>;
  proveRuntimeIdentity(expected: VerifiedIdentity): Promise<RuntimeProof>;
  stopOwnedRestarts(): Promise<void>;
  approveOwnedIdlePlan?(
    plan: ProcessPlan,
    expected: VerifiedIdentity,
    credentialFingerprint: string
  ): Promise<boolean>;
  revalidateQuiescedStop?(
    receipt: StopReceipt,
    expected: VerifiedIdentity,
    credentialFingerprint: string
  ): Promise<QuiescedHostProof | null>;
  completeActivation?(expected: VerifiedIdentity): Promise<void>;
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
function exactKeys(value: Record<string, unknown>, keys: string[]): boolean {
  return Object.keys(value).sort().join('|') === [...keys].sort().join('|');
}
function fail(): never {
  // Never include a native error, IPC body, path, argv or credential in an error.
  throw new AntigravityError('antigravity-runtime-unavailable');
}
function identity(value: unknown): boolean {
  return (
    record(value) &&
    exactKeys(value, ['pid', 'startTime', 'ownerId', 'fingerprint']) &&
    Number.isSafeInteger(value.pid) &&
    (value.pid as number) > 1 &&
    typeof value.startTime === 'string' &&
    value.startTime.length < 128 &&
    typeof value.ownerId === 'string' &&
    /^\d+$/.test(value.ownerId) &&
    typeof value.fingerprint === 'string' &&
    SHA256.test(value.fingerprint)
  );
}
function plan(value: unknown): value is ProcessPlan {
  if (
    !record(value) ||
    !exactKeys(value, ['complete', 'processes', 'continuity']) ||
    typeof value.complete !== 'boolean' ||
    !Array.isArray(value.processes) ||
    value.processes.length > 128 ||
    !value.processes.every(
      (row) =>
        record(row) &&
        exactKeys(row, ['identity', 'role']) &&
        identity(row.identity) &&
        ['cli', 'desktop', 'language-server'].includes(String(row.role))
    )
  )
    return false;
  return (
    value.continuity === null ||
    (record(value.continuity) &&
      exactKeys(value.continuity, ['fingerprint', 'restorable']) &&
      typeof value.continuity.fingerprint === 'string' &&
      SHA256.test(value.continuity.fingerprint) &&
      typeof value.continuity.restorable === 'boolean')
  );
}
function receipt(value: unknown): value is StopReceipt {
  return (
    record(value) &&
    exactKeys(value, ['stopped', 'complete', 'restartState']) &&
    Array.isArray(value.stopped) &&
    value.stopped.length <= 128 &&
    value.stopped.every(identity) &&
    typeof value.complete === 'boolean' &&
    typeof value.restartState === 'string' &&
    UUID.test(value.restartState)
  );
}

/** Importing/constructing connects to nothing; no daemon start or native launch. */
export function createUbuntuRuntimeBridge(options: {
  socketPath: string;
  request?: Request;
  timeoutMs?: number;
}): UbuntuRuntimeBridge {
  const transport =
    options.request ?? createPrivateUnixRequest(options.socketPath, options.timeoutMs);
  const request: Request = async (method, parameters) => {
    try {
      return await transport(method, parameters);
    } catch {
      return fail();
    }
  };
  return {
    async approveOwnedIdlePlan(processPlan, expected, credentialFingerprint) {
      if (!plan(processPlan) || !SHA256.test(credentialFingerprint)) return false;
      const value = await request('approve-idle-plan', {
        plan: processPlan,
        expected,
        credentialFingerprint,
      });
      return record(value) && exactKeys(value, ['approved']) && value.approved === true;
    },
    async revalidateQuiescedStop(stopReceipt, expected, credentialFingerprint) {
      if (!receipt(stopReceipt) || !stopReceipt.complete || !SHA256.test(credentialFingerprint))
        return null;
      const value = await request('approve-quiesced-stop', {
        receipt: stopReceipt,
        expected,
        credentialFingerprint,
      });
      if (
        !record(value) ||
        !exactKeys(value, [
          'available',
          'complete',
          'busy',
          'manualActivationInProgress',
          'sampledAt',
        ]) ||
        value.available !== true ||
        value.complete !== true ||
        value.busy !== false ||
        value.manualActivationInProgress !== false ||
        typeof value.sampledAt !== 'string' ||
        !Number.isFinite(Date.parse(value.sampledAt))
      )
        return null;
      return {
        hostId: 'ubuntu',
        available: true,
        complete: true,
        busy: false,
        manualActivationInProgress: false,
        sampledAt: value.sampledAt,
      };
    },
    async completeActivation(expected) {
      const value = await request('complete-transaction', { expected });
      if (!record(value) || !exactKeys(value, ['ok']) || value.ok !== true) fail();
    },
    async readHostCensus() {
      const unavailable: AntigravityAutoHostCensus = {
        hostId: 'ubuntu',
        available: false,
        complete: false,
        busy: true,
        manualActivationInProgress: false,
        sampledAt: '',
      };
      try {
        const value = await request('census');
        if (
          !record(value) ||
          !exactKeys(value, [
            'available',
            'complete',
            'busy',
            'manualActivationInProgress',
            'sampledAt',
          ]) ||
          typeof value.available !== 'boolean' ||
          typeof value.complete !== 'boolean' ||
          typeof value.busy !== 'boolean' ||
          typeof value.manualActivationInProgress !== 'boolean' ||
          typeof value.sampledAt !== 'string' ||
          !Number.isFinite(Date.parse(value.sampledAt))
        )
          return unavailable;
        return {
          hostId: 'ubuntu',
          available: value.available,
          complete: value.complete,
          busy: value.busy,
          manualActivationInProgress: value.manualActivationInProgress,
          sampledAt: value.sampledAt,
        };
      } catch {
        return unavailable;
      }
    },
    async canProveRuntimeIdentity() {
      try {
        const value = await request('capability');
        return (
          record(value) &&
          exactKeys(value, ['canProveRuntimeIdentity']) &&
          value.canProveRuntimeIdentity === true
        );
      } catch {
        return false;
      }
    },
    async inspectProcesses() {
      const value = await request('inspect');
      if (!plan(value)) fail();
      return value;
    },
    async stopProcesses(processPlan) {
      if (!plan(processPlan)) fail();
      const value = await request('stop', { plan: processPlan });
      if (!receipt(value)) fail();
      // Keep a partial stopped-set/receipt intact; never turn it into success.
      return value;
    },
    async restartProcesses(stopReceipt) {
      if (!receipt(stopReceipt) || !stopReceipt.complete) fail();
      const value = await request('restart', { receipt: stopReceipt });
      if (!record(value) || !exactKeys(value, ['ok']) || value.ok !== true) fail();
    },
    async proveRuntimeIdentity(expected) {
      const value = await request('prove', { expected });
      if (
        !record(value) ||
        !exactKeys(value, [
          'identity',
          'credentialFingerprint',
          'runtimeStarted',
          'sessionRestored',
        ]) ||
        !record(value.identity) ||
        !exactKeys(value.identity, ['email', 'subject', 'plan', 'verifiedAt', 'source']) ||
        value.identity.email !== expected.email ||
        value.identity.subject !== expected.subject ||
        value.identity.source !== 'native-runtime' ||
        (value.identity.plan !== null && typeof value.identity.plan !== 'string') ||
        typeof value.identity.verifiedAt !== 'string' ||
        !Number.isFinite(Date.parse(value.identity.verifiedAt)) ||
        typeof value.credentialFingerprint !== 'string' ||
        !SHA256.test(value.credentialFingerprint) ||
        value.runtimeStarted !== true ||
        value.sessionRestored !== true
      )
        fail();
      return value as unknown as RuntimeProof;
    },
    async stopOwnedRestarts() {
      const value = await request('stop-owned-restarts');
      if (!record(value) || !exactKeys(value, ['ok']) || value.ok !== true) fail();
    },
  };
}

export function createPrivateUnixRequest(socketPath: string, timeoutMs = 15000): Request {
  if (!Number.isInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 20000) fail();
  return async (method, parameters = {}) => {
    if (process.platform !== 'linux' || typeof process.getuid !== 'function') fail();
    const directory = await lstat(dirname(socketPath));
    const socketBefore = await lstat(socketPath);
    const uid = process.getuid();
    if (
      !directory.isDirectory() ||
      directory.uid !== uid ||
      (directory.mode & 0o777) !== 0o700 ||
      !socketBefore.isSocket() ||
      socketBefore.uid !== uid ||
      (socketBefore.mode & 0o777) !== 0o600
    )
      fail();
    const requestId = randomUUID();
    const body = Buffer.from(JSON.stringify({ requestId, method, ...parameters }));
    if (body.length > MAX_FRAME_BYTES) fail();
    const length = Buffer.alloc(4);
    length.writeUInt32BE(body.length);
    return new Promise<unknown>((resolve, reject) => {
      const socket = connect(socketPath);
      let pending = Buffer.alloc(0);
      let done = false;
      const finish = (value?: unknown, failed = false) => {
        if (done) return;
        done = true;
        socket.destroy();
        if (failed) reject(new Error('antigravity-runtime-unavailable'));
        else resolve(value);
      };
      socket.setTimeout(timeoutMs, () => finish(undefined, true));
      socket.on('error', () => finish(undefined, true));
      socket.on('end', () => {
        if (!done) finish(undefined, true);
      });
      socket.on('connect', () => {
        void lstat(socketPath)
          .then((after) => {
            if (
              after.ino !== socketBefore.ino ||
              after.dev !== socketBefore.dev ||
              after.uid !== uid ||
              !after.isSocket()
            ) {
              finish(undefined, true);
              return;
            }
            socket.write(Buffer.concat([length, body]));
          })
          .catch(() => finish(undefined, true));
      });
      socket.on('data', (chunk) => {
        pending = Buffer.concat([pending, chunk]);
        if (pending.length > MAX_FRAME_BYTES + 4) {
          finish(undefined, true);
          return;
        }
        if (pending.length < 4) return;
        const size = pending.readUInt32BE(0);
        if (size > MAX_FRAME_BYTES) {
          finish(undefined, true);
          return;
        }
        if (pending.length < size + 4) return;
        if (pending.length !== size + 4) {
          finish(undefined, true);
          return;
        }
        try {
          const message: unknown = JSON.parse(pending.subarray(4).toString('utf8'));
          if (
            !record(message) ||
            !exactKeys(message, ['requestId', 'result']) ||
            message.requestId !== requestId
          ) {
            finish(undefined, true);
            return;
          }
          finish(message.result);
        } catch {
          finish(undefined, true);
        }
      });
    });
  };
}
