import { spawn } from 'child_process';
import path from 'path';
import { credentialFingerprint } from './registry';
import type { UbuntuNativeCredentialStore } from './ubuntu-driver';
import type { InstallReceipt, NativeCredential } from './types';

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
      return new Promise<unknown>((resolve, reject) => {
        const child = spawn('/usr/bin/python3', ['-I', script, '--home', options.home], {
          stdio: ['pipe', 'pipe', 'pipe'],
          windowsHide: true,
        });
        const chunks: Buffer[] = [];
        let bytes = 0;
        let settled = false;
        const finish = (value: unknown, error = false) => {
          if (settled) return;
          settled = true;
          clearTimeout(timer);
          if (error) {
            try {
              child.kill('SIGTERM');
            } catch {
              /* Owned helper only. */
            }
            reject(failure());
          } else resolve(value);
        };
        const timer = setTimeout(() => finish(undefined, true), 25_000);
        child.on('error', () => finish(undefined, true));
        child.stdin.on('error', () => finish(undefined, true));
        child.stderr.resume();
        child.stdout.on('data', (chunk: Buffer) => {
          if (settled) return;
          bytes += chunk.length;
          if (bytes > MAX_BYTES) {
            finish(undefined, true);
            return;
          }
          chunks.push(chunk);
        });
        child.once('close', (code) => {
          if (settled) return;
          if (code !== 0) {
            finish(undefined, true);
            return;
          }
          try {
            finish(JSON.parse(Buffer.concat(chunks).toString('utf8')) as unknown);
          } catch {
            finish(undefined, true);
          }
        });
        child.stdin.end(request);
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
  return {
    async read() {
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
    },
    async install(credential, expectedFingerprint) {
      if (!HASH.test(expectedFingerprint)) throw failure();
      const result = await invoke({
        operation: 'install',
        credentialBase64: encoded(credential),
        expectedFingerprint,
      });
      if (
        !record(result) ||
        result.installedFingerprint !== credentialFingerprint(credential) ||
        !record(result.rollbackState)
      )
        throw failure();
      return result as unknown as InstallReceipt;
    },
    async rollback(receipt, previous) {
      try {
        const result = await invoke({
          operation: 'rollback',
          receipt: receipt.rollbackState,
          previousCredentialBase64: encoded(previous),
        });
        return record(result) && result.restored === true;
      } catch {
        return false;
      }
    },
  };
}
