import fs from 'fs';
import { runManagedCommand } from '../utils/managed-process';
import path from 'path';
import { credentialFingerprint } from './registry';
import type { UbuntuNativeCredentialStore } from './ubuntu-driver';
import type { InstallReceipt, NativeCredential, VerifiedIdentity } from './types';

const MAX_BYTES = 65_536;
const FORMAT = 'antigravity-consumer-json';
const HASH = /^[a-f0-9]{64}$/;
const failure = () => new Error('antigravity-private-store-unavailable');
const record = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === 'object' && !Array.isArray(value);

/** Fixed trusted application paths only; construction spawns nothing. */
export function createUbuntuNativeCredentialStore(options: {
  home: string;
  scriptPath?: string;
  now?: () => number;
  invoke?: (packet: Record<string, unknown>) => Promise<unknown>;
}): UbuntuNativeCredentialStore {
  const script =
    options.scriptPath ??
    path.resolve(__dirname, '../../scripts/antigravity/native_credential_worker.py');
  if (!path.isAbsolute(script) || !path.isAbsolute(options.home)) throw failure();
  const invoke =
    options.invoke ??
    ((packet: Record<string, unknown>) => {
      const request = Buffer.from(JSON.stringify(packet));
      if (request.length > MAX_BYTES) return Promise.reject(failure());
      return runManagedCommand('/usr/bin/python3', ['-I', script, '--home', options.home], {
        input: request.toString('utf8'),
        timeoutMs: 25_000,
        graceMs: 0,
        maxBytes: MAX_BYTES,
        coalesce: packet.operation === 'read',
      }).then((result) => {
        if (result.failed) throw failure();
        try {
          return JSON.parse(result.stdout) as unknown;
        } catch {
          throw failure();
        }
      });
    });
  const encoded = (credential: NativeCredential) => {
    if (
      credential.format !== FORMAT ||
      !Buffer.isBuffer(credential.bytes) ||
      credential.bytes.length === 0 ||
      credential.bytes.length > 16_384
    )
      throw failure();
    return credential.bytes.toString('base64');
  };
  // Only inventory identities are retained, never credential bytes. Every mutation and
  // runtime proof still uses read(), which invokes the guarded Python store afresh.
  let cached: { key: string; identity: VerifiedIdentity; expires: number } | undefined;
  let pending: { key: string; work: Promise<VerifiedIdentity> } | undefined;
  let revision = 0;
  const invalidate = () => {
    cached = undefined;
    pending = undefined;
    revision++;
  };
  const fileIdentity = async (): Promise<string> => {
    const parts = [
      options.home,
      path.join(options.home, '.gemini'),
      path.join(options.home, '.gemini/antigravity-cli'),
      path.join(options.home, '.gemini/antigravity-cli/antigravity-oauth-token'),
    ];
    const identities: string[] = [];
    for (const [index, file] of parts.entries()) {
      const stat = await fs.promises.lstat(file, { bigint: true });
      const uid = process.getuid?.();
      if (
        (uid !== undefined && stat.uid !== BigInt(uid)) ||
        (index < 3
          ? !stat.isDirectory() || (stat.mode & 2n) !== 0n
          : !stat.isFile() ||
            (stat.mode & 511n) !== 384n ||
            stat.nlink !== 1n ||
            stat.size <= 0n ||
            stat.size > 16_384n)
      )
        throw failure();
      identities.push(
        (index < 3
          ? [stat.dev, stat.ino, stat.mode, stat.uid]
          : [
              stat.dev,
              stat.ino,
              stat.size,
              stat.mtimeNs,
              stat.ctimeNs,
              stat.mode,
              stat.uid,
              stat.nlink,
            ]
        ).join(':')
      );
    }
    return identities.join('/');
  };
  const read = async (): Promise<NativeCredential> => {
    const result = await invoke({ operation: 'read' });
    if (
      !record(result) ||
      result.format !== FORMAT ||
      typeof result.credentialBase64 !== 'string' ||
      typeof result.revision !== 'string' ||
      !HASH.test(result.revision)
    )
      throw failure();
    const credential = { format: FORMAT, bytes: Buffer.from(result.credentialBase64, 'base64') };
    encoded(credential);
    if (credentialFingerprint(credential) !== result.revision) throw failure();
    return credential;
  };
  return {
    read,
    async readInventoryIdentity(validate) {
      try {
        const key = await fileIdentity();
        const now = options.now ?? Date.now;
        if (cached?.key === key && now() < cached.expires) return { ...cached.identity };
        if (pending?.key === key) return { ...(await pending.work) };
        const generation = revision;
        const work = (async () => {
          const current = await read();
          const identity = await validate(current);
          if (generation !== revision || (await fileIdentity()) !== key) throw failure();
          cached = { key, identity: { ...identity }, expires: now() + 2000 };
          return identity;
        })();
        const entry = { key, work };
        pending = entry;
        try {
          return { ...(await work) };
        } finally {
          if (pending === entry) pending = undefined;
        }
      } catch {
        invalidate();
        throw failure();
      }
    },
    async install(credential, expectedFingerprint) {
      invalidate();
      if (!HASH.test(expectedFingerprint)) throw failure();
      const result = await invoke({
        operation: 'install',
        credentialBase64: encoded(credential),
        expectedFingerprint,
      }).finally(invalidate);
      if (
        !record(result) ||
        result.installedFingerprint !== credentialFingerprint(credential) ||
        !record(result.rollbackState)
      )
        throw failure();
      return result as unknown as InstallReceipt;
    },
    async rollback(receipt, previous) {
      invalidate();
      try {
        const result = await invoke({
          operation: 'rollback',
          receipt: receipt.rollbackState,
          previousCredentialBase64: encoded(previous),
        }).finally(invalidate);
        return record(result) && result.restored === true;
      } catch {
        return false;
      }
    },
  };
}
