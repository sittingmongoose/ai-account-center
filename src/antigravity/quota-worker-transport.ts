import { spawn, type ChildProcessWithoutNullStreams } from 'child_process';
import path from 'path';
import { checkedIdentity, credentialFingerprint, identityKey } from './registry';
import type { NativeCredential, VerifiedIdentity } from './types';
import type { SavedQuotaRequest, SavedQuotaSnapshot } from './runtime-composition';
import { record, safeId } from './usage-normalization';

const MAX_BYTES = 65_536;
const MAX_CREDENTIAL_BYTES = 16_384;
const TIMEOUT_MS = 25_000;
const HASH = /^[a-f0-9]{64}$/;

export type PrivateWorkerSpawner = (
  binary: string,
  args: string[],
  options: {
    stdio: ['pipe', 'pipe', 'pipe'];
    windowsHide: boolean;
  }
) => ChildProcessWithoutNullStreams;

/** Fixed trusted package paths, never supplied by a dashboard request. */
export interface AntigravityWorkerOptions {
  scriptPath?: string;
  collectorDirectory?: string;
  spawnWorker?: PrivateWorkerSpawner;
  setTimer?: (callback: () => void, milliseconds: number) => unknown;
  clearTimer?: (timer: unknown) => void;
}

function failure(): Error {
  return new Error('Antigravity private account request failed safely.');
}

/** Bounded owned helper only; credentials travel on stdin, never argv, env, logs or HTTP. */
export function createAntigravityQuotaWorker(options: AntigravityWorkerOptions = {}) {
  const script =
    options.scriptPath ?? path.resolve(__dirname, '../../scripts/antigravity/quota_worker.py');
  const collectors =
    options.collectorDirectory ?? path.resolve(__dirname, '../../scripts/account-usage');
  if (!path.isAbsolute(script) || !path.isAbsolute(collectors)) throw failure();
  const launch: PrivateWorkerSpawner =
    options.spawnWorker ?? ((binary, args, processOptions) => spawn(binary, args, processOptions));
  const invoke = (packet: Record<string, unknown>): Promise<Record<string, unknown>> => {
    const request = Buffer.from(JSON.stringify(packet));
    if (request.length > MAX_BYTES) return Promise.reject(failure());
    return new Promise((resolve, reject) => {
      let child: ChildProcessWithoutNullStreams;
      try {
        child = launch('/usr/bin/python3', ['-I', script, '--collector-dir', collectors], {
          stdio: ['pipe', 'pipe', 'pipe'],
          windowsHide: true,
        });
      } catch {
        reject(failure());
        return;
      }
      const chunks: Buffer[] = [];
      let bytes = 0;
      let settled = false;
      let exited = false;
      let timer: unknown = undefined;
      const finish = (error?: Error, value?: Record<string, unknown>) => {
        if (settled) return;
        settled = true;
        if (options.clearTimer) options.clearTimer(timer);
        else clearTimeout(timer as ReturnType<typeof setTimeout>);
        if (error) reject(error);
        else resolve(value as Record<string, unknown>);
      };
      const stopOwnedWorker = () => {
        try {
          if (!exited && child.exitCode === null) child.kill('SIGTERM');
        } catch {
          /* Owned helper failure stays private. */
        }
      };
      timer = (options.setTimer ?? setTimeout)(() => {
        stopOwnedWorker();
        finish(failure());
      }, TIMEOUT_MS);
      child.once('error', () => finish(failure()));
      child.stdin.once('error', () => {
        stopOwnedWorker();
        finish(failure());
      });
      child.stdout.on('data', (chunk: Buffer | string) => {
        if (settled) return;
        const data = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
        bytes += data.length;
        if (bytes > MAX_BYTES + 1) {
          stopOwnedWorker();
          finish(failure());
          return;
        }
        chunks.push(data);
      });
      // Discard worker diagnostics; raw stderr can never become a credential channel.
      child.stderr.resume();
      child.once('close', (code) => {
        exited = true;
        if (settled) return;
        if (code !== 0 || bytes > MAX_BYTES + 1) {
          finish(failure());
          return;
        }
        try {
          const output: unknown = JSON.parse(Buffer.concat(chunks).toString('utf8'));
          if (!record(output)) throw failure();
          finish(undefined, output);
        } catch {
          finish(failure());
        }
      });
      try {
        child.stdin.end(request);
      } catch {
        stopOwnedWorker();
        finish(failure());
      }
    });
  };
  const encoded = (credential: NativeCredential): string => {
    if (
      !credential ||
      !Buffer.isBuffer(credential.bytes) ||
      !credential.bytes.length ||
      credential.bytes.length > MAX_CREDENTIAL_BYTES
    )
      throw failure();
    return credential.bytes.toString('base64');
  };
  return {
    collectQuota: async (request: SavedQuotaRequest): Promise<SavedQuotaSnapshot> => {
      let verified: VerifiedIdentity;
      try {
        verified = checkedIdentity(request.identity);
      } catch {
        throw failure();
      }
      if (
        !safeId(request.profileId) ||
        !HASH.test(request.identityKey) ||
        !HASH.test(request.credentialRevision) ||
        typeof request.email !== 'string' ||
        verified.email !== request.email.toLowerCase() ||
        identityKey(verified) !== request.identityKey
      )
        throw failure();
      try {
        if (credentialFingerprint(request.credential) !== request.credentialRevision)
          throw failure();
      } catch {
        throw failure();
      }
      const result = await invoke({
        operation: 'quota',
        profileId: request.profileId,
        email: request.email,
        identityKey: request.identityKey,
        credentialRevision: request.credentialRevision,
        credentialBase64: encoded(request.credential),
      });
      // The composition/cache bind and project this internal DTO before any publication.
      return result as unknown as SavedQuotaSnapshot;
    },
    validateCredential: async (credential: NativeCredential): Promise<VerifiedIdentity> => {
      const result = await invoke({ operation: 'identity', credentialBase64: encoded(credential) });
      try {
        const identity = checkedIdentity({
          email: result.email as string,
          subject: result.subject as string,
          plan: result.plan as string | null,
          verifiedAt: result.verifiedAt as string,
          source: result.source as VerifiedIdentity['source'],
        });
        if (
          result.credentialRevision !== credentialFingerprint(credential) ||
          result.identityKey !== identityKey(identity)
        )
          throw failure();
        return identity;
      } catch {
        throw failure();
      }
    },
  };
}
