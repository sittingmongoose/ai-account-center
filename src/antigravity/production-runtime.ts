import fs from 'fs';
import os from 'os';
import path from 'path';
import { createHash } from 'crypto';
import {
  createPersistentAntigravityRuntime,
  type AntigravityRuntimeDependencies,
} from './runtime-composition';
import type { AntigravityRuntimeFactory } from './runtime-service';
import { createAntigravityQuotaWorker } from './quota-worker-transport';
import { createUbuntuNativeCredentialStore } from './native-credential-transport';
import { createUbuntuAntigravityDriver } from './ubuntu-driver';
import { createUbuntuRuntimeBridge } from './ubuntu-runtime-bridge';
import { AntigravityProfileRegistry } from './registry';
import { AntigravityError } from './errors';
import type { AntigravitySwitchDriver } from './types';

/** A native gate receipt must be reviewed and committed before this changes. */
export const ANTIGRAVITY_NATIVE_RELEASED = true;

/** A CLI executable may be large; hashing stays bounded and uses one owned fd. */
export const MAX_ANTIGRAVITY_NATIVE_BINARY_BYTES = 256 * 1024 * 1024;
type NativePinReader = (
  fd: number,
  buffer: Buffer,
  offset: number,
  length: number,
  position: number
) => number;

function sameNativeFile(left: fs.BigIntStats, right: fs.BigIntStats): boolean {
  return (
    left.dev === right.dev &&
    left.ino === right.ino &&
    left.mtimeNs === right.mtimeNs &&
    left.ctimeNs === right.ctimeNs &&
    left.size === right.size &&
    left.mode === right.mode &&
    left.nlink === right.nlink &&
    left.uid === right.uid &&
    left.gid === right.gid
  );
}

function safeNativeFile(value: fs.BigIntStats, uid: number): boolean {
  return (
    value.isFile() &&
    value.uid === BigInt(uid) &&
    value.nlink === 1n &&
    (value.mode & 0o100n) !== 0n &&
    (value.mode & 0o022n) === 0n &&
    value.size > 0n &&
    value.size <= BigInt(MAX_ANTIGRAVITY_NATIVE_BINARY_BYTES)
  );
}

/** Explicit read-only native pin proof; importing this module reads no binary.
 * The installed factory supplies its already-validated fixed native path.
 * The private reader seam permits disposable mutation fixtures only.
 */
export function verifyOwnedAntigravityNativePin(
  nativeBinary: string,
  expectedSha256: string,
  reader: NativePinReader = (fd, buffer, offset, length, position) =>
    fs.readSync(fd, buffer, offset, length, position)
): boolean {
  const uid = process.getuid?.();
  if (!Number.isSafeInteger(uid) || (uid as number) < 0 || !/^[a-f0-9]{64}$/.test(expectedSha256))
    return false;
  let descriptor: number | null = null;
  let verified = false;
  try {
    const before = fs.lstatSync(nativeBinary, { bigint: true });
    if (!safeNativeFile(before, uid as number)) return false;
    descriptor = fs.openSync(
      nativeBinary,
      fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK
    );
    const opened = fs.fstatSync(descriptor, { bigint: true });
    if (!safeNativeFile(opened, uid as number) || !sameNativeFile(before, opened)) return false;
    const size = Number(opened.size);
    const buffer = Buffer.alloc(Math.min(size, 64 * 1024));
    const hash = createHash('sha256');
    let position = 0;
    while (position < size) {
      const length = Math.min(buffer.length, size - position);
      const count = reader(descriptor, buffer, 0, length, position);
      if (!Number.isSafeInteger(count) || count < 1 || count > length) return false;
      hash.update(buffer.subarray(0, count));
      position += count;
    }
    if (reader(descriptor, buffer, 0, 1, size) !== 0) return false;
    const finalDescriptor = fs.fstatSync(descriptor, { bigint: true });
    const finalLeaf = fs.lstatSync(nativeBinary, { bigint: true });
    if (
      !safeNativeFile(finalDescriptor, uid as number) ||
      !safeNativeFile(finalLeaf, uid as number) ||
      !sameNativeFile(before, finalDescriptor) ||
      !sameNativeFile(before, finalLeaf)
    )
      return false;
    verified = hash.digest('hex') === expectedSha256;
  } catch {
    return false;
  } finally {
    if (descriptor !== null) {
      try {
        fs.closeSync(descriptor);
      } catch {
        verified = false;
      }
    }
  }
  return verified;
}

interface InstalledRuntime {
  schemaVersion: 1;
  bundleDirectory: string;
  nativeBinary: string;
  nativeSha256: string;
  socketPath: string;
}

export function readInstalledAntigravityRuntime(
  ccsDir: string,
  home: string
): InstalledRuntime | null {
  const directory = path.join(ccsDir, 'antigravity-switching');
  const file = path.join(directory, 'runtime-installation.json');
  try {
    const parent = fs.lstatSync(directory);
    const before = fs.lstatSync(file);
    const uid = process.getuid?.();
    if (
      !parent.isDirectory() ||
      parent.isSymbolicLink() ||
      (parent.mode & 0o777) !== 0o700 ||
      !before.isFile() ||
      before.isSymbolicLink() ||
      before.nlink !== 1 ||
      (before.mode & 0o777) !== 0o600 ||
      parent.uid !== uid ||
      before.uid !== uid ||
      before.size > 8192
    )
      return null;
    const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
    let raw: Buffer;
    try {
      const opened = fs.fstatSync(fd);
      if (opened.dev !== before.dev || opened.ino !== before.ino) return null;
      raw = fs.readFileSync(fd);
    } finally {
      fs.closeSync(fd);
    }
    const value = JSON.parse(raw.toString('utf8')) as InstalledRuntime;
    const after = fs.lstatSync(file);
    const runtimeRoot = path.join(
      home,
      '.local/share/ai-account-center/antigravity-runtime/bundles'
    );
    if (
      after.ino !== before.ino ||
      after.dev !== before.dev ||
      raw.length > 8192 ||
      value.schemaVersion !== 1 ||
      typeof value.bundleDirectory !== 'string' ||
      path.dirname(value.bundleDirectory) !== runtimeRoot ||
      !/^[a-f0-9]{64}$/.test(path.basename(value.bundleDirectory)) ||
      value.nativeBinary !== path.join(home, '.local/bin/agy') ||
      typeof value.nativeSha256 !== 'string' ||
      !/^[a-f0-9]{64}$/.test(value.nativeSha256) ||
      value.socketPath !== path.join(ccsDir, 'antigravity-runtime/control.sock')
    )
      return null;
    return value;
  } catch {
    return null;
  }
}

/** No implicit setup, daemon launch, account import or private-file read on import. */
export function createInstalledAntigravityRuntimeFactory(
  options: {
    home?: string;
    /** Explicit private composition seams; never dashboard settings or native setup. */
    collectQuota?: AntigravityRuntimeDependencies['collectQuota'];
    now?: AntigravityRuntimeDependencies['now'];
    setTimer?: AntigravityRuntimeDependencies['setTimer'];
    clearTimer?: AntigravityRuntimeDependencies['clearTimer'];
  } = {}
): AntigravityRuntimeFactory {
  const home = path.resolve(options.home ?? os.homedir());
  return (ccsDir) => {
    if (process.platform !== 'linux') return null;
    const directory = path.resolve(ccsDir);
    const components = createInstalledAntigravityComponents(directory, home);
    if (components) {
      const { quotaWorker, driver, bridge } = components;
      return createPersistentAntigravityRuntime(directory, {
        driver,
        collectQuota: quotaWorker.collectQuota,
        observeHost: () => bridge.readHostCensus(),
      });
    }
    // Saved-profile usage does not require a native runtime installation or release.
    if (!fs.existsSync(path.join(directory, 'antigravity-profiles'))) return null;
    const registry = new AntigravityProfileRegistry(directory);
    if (!registry.listProfiles().length) return null;
    const unsupported = async (): Promise<never> => {
      throw new AntigravityError('Antigravity native activation is unavailable.');
    };
    const driver: AntigravitySwitchDriver = {
      hostId: 'ubuntu',
      canProveRuntimeIdentity: async () => false,
      readCurrentCredential: unsupported,
      validateCredential: unsupported,
      inspectProcesses: unsupported,
      stopProcesses: unsupported,
      installCredential: unsupported,
      readStoredIdentity: unsupported,
      restartProcesses: unsupported,
      proveRuntimeIdentity: unsupported,
      rollbackCredential: unsupported,
      stopOwnedRestarts: unsupported,
    };
    const now = options.now ?? Date.now;
    const runtime = createPersistentAntigravityRuntime(directory, {
      driver,
      collectQuota: options.collectQuota ?? createAntigravityQuotaWorker().collectQuota,
      observeHost: async () => ({
        hostId: 'ubuntu',
        available: false,
        complete: false,
        busy: true,
        manualActivationInProgress: true,
        sampledAt: new Date(now()).toISOString(),
      }),
      now,
      setTimer: options.setTimer,
      clearTimer: options.clearTimer,
    });
    return {
      ...runtime,
      activate: async (request) => {
        if (request.mode !== 'manual' || request.hostId !== 'ubuntu')
          throw new AntigravityError('Invalid manual activation request.');
        return {
          status: 'unsupported-runtime-probe',
          profileId: request.profileId,
          hostId: 'ubuntu',
        };
      },
    };
  };
}

/** Private composition shared by the dashboard and explicit managed updater. */
export function createInstalledAntigravityComponents(ccsDir: string, home: string) {
  if (process.platform !== 'linux') return null;
  const installed = readInstalledAntigravityRuntime(path.resolve(ccsDir), home);
  if (!installed) return null;
  const quotaWorker = createAntigravityQuotaWorker();
  const nativeStore = createUbuntuNativeCredentialStore({ home });
  const bridge = createUbuntuRuntimeBridge({ socketPath: installed.socketPath });
  const driver = createUbuntuAntigravityDriver({
    nativeStore,
    quotaWorker,
    bridge,
    releaseGate: async () => {
      // Saved settings or a dashboard PUT cannot enable native activation.
      if (!ANTIGRAVITY_NATIVE_RELEASED) return false;
      const current = readInstalledAntigravityRuntime(path.resolve(ccsDir), home);
      if (!current || current.nativeSha256 !== installed.nativeSha256) return false;
      return verifyOwnedAntigravityNativePin(current.nativeBinary, current.nativeSha256);
    },
  });
  return { installed, quotaWorker, nativeStore, bridge, driver };
}

let defaultFactory: AntigravityRuntimeFactory | null = null;
export function getInstalledAntigravityRuntimeFactory(): AntigravityRuntimeFactory {
  defaultFactory ??= createInstalledAntigravityRuntimeFactory();
  return defaultFactory;
}
