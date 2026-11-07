/** Fixed private updater entry point; importing never runs a command. */
import fs from 'fs';
import os from 'os';
import path from 'path';
import { createHash } from 'crypto';
import { execFile } from 'child_process';
import { AntigravityProfileRegistry } from './registry';
import {
  createInstalledAntigravityComponents,
  hashOwnedAntigravityNative,
} from './production-runtime';
import {
  routeManagedAntigravityUpdate,
  type ManagedNativeUpdate,
  type ManagedUpdateResult,
  type NativeReviewState,
} from './managed-updater';
import { defaultNativeReleaseFile, readNativeRelease } from './native-version';
import { probeRuntimeSocket } from './runtime-socket-probe';
import { AntigravityError } from './errors';

const MAX_COMMAND_BYTES = 65_536;

export type ManagedNativeCommand = (
  binary: string,
  args: string[],
  capture: boolean
) => Promise<string>;

export interface ManagedNativeUpdateOptions {
  nativeBinary: string;
  ccsDirectory: string;
  /** Owned fixture injection only; production uses the fixed native execFile command below. */
  runCommand?: ManagedNativeCommand;
}

type ManagedCoordinatorResult = ManagedUpdateResult;

/**
 * Why a managed update failed before it could finish, beyond update_failed:
 * runtime_unavailable - the resident runtime did not answer (nothing ran);
 * runtime_paused - switching is paused on an unreviewed build, the runtime is
 * stopped, and `agy update` ran without it but did not finish;
 * runtime_paused_blocked - the same, but it refused before `agy update` ran.
 */
export type ManagedFailureCode =
  | 'update_failed'
  | 'runtime_unavailable'
  | 'runtime_paused'
  | 'runtime_paused_blocked';

export interface ManagedUpdateCommandOptions extends ManagedNativeUpdateOptions {
  /** Explicit coordinator injection; importing or constructing options performs no work. */
  execute(native: ManagedNativeUpdate): Promise<ManagedCoordinatorResult>;
}

export interface ManagedUpdateCommandResult {
  appId: 'antigravity-cli';
  platform: 'ubuntu';
  manager: 'native';
  status: Exclude<ManagedCoordinatorResult['status'], 'in_use'> | 'action_required';
  messageCode: ManagedCoordinatorResult['status'] | ManagedFailureCode;
  updateAttempted: boolean;
  restartedProcesses: number;
  previousVersion?: string;
  version?: string;
}

function nativeBytes(binary: string) {
  const before = fs.lstatSync(binary);
  if (
    !before.isFile() ||
    before.isSymbolicLink() ||
    before.uid !== process.getuid?.() ||
    before.nlink !== 1 ||
    before.size > 512 * 1024 * 1024 ||
    before.mode & 0o022
  )
    throw new AntigravityError('update-native-unsafe');
  const fd = fs.openSync(binary, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  let bytes: Buffer;
  try {
    const opened = fs.fstatSync(fd);
    if (opened.dev !== before.dev || opened.ino !== before.ino)
      throw new AntigravityError('update-native-changed');
    bytes = fs.readFileSync(fd);
  } finally {
    fs.closeSync(fd);
  }
  const after = fs.lstatSync(binary);
  if (
    after.dev !== before.dev ||
    after.ino !== before.ino ||
    after.mtimeMs !== before.mtimeMs ||
    bytes.length !== before.size
  )
    throw new AntigravityError('update-native-changed');
  return { bytes, stat: before, sha: createHash('sha256').update(bytes).digest('hex') };
}

function command(binary: string, args: string[], capture = false): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(
      binary,
      args,
      {
        timeout: args[0] === 'update' ? 180000 : 10000,
        maxBuffer: MAX_COMMAND_BYTES,
        env: { ...process.env, AGY_CLI_DISABLE_AUTO_UPDATE: 'true', NO_UPDATE_NOTIFIER: '1' },
      },
      (error, stdout) =>
        error
          ? reject(new AntigravityError('update-native-failed'))
          : resolve(capture ? stdout : '')
    );
  });
}

function privateBackupDirectory(ccsDirectory: string): string {
  let ancestor = ccsDirectory;
  for (;;) {
    const info = fs.lstatSync(ancestor);
    if (!info.isDirectory() || info.isSymbolicLink())
      throw new AntigravityError('update-backup-unsafe');
    const parent = path.dirname(ancestor);
    if (parent === ancestor) break;
    ancestor = parent;
  }
  const directories = [
    ccsDirectory,
    path.join(ccsDirectory, 'antigravity-switching'),
    path.join(ccsDirectory, 'antigravity-switching/native-backups'),
  ];
  for (const directory of directories) {
    try {
      fs.mkdirSync(directory, { mode: 0o700 });
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EEXIST')
        throw new AntigravityError('update-backup-unsafe');
    }
    const info = fs.lstatSync(directory);
    if (
      !info.isDirectory() ||
      info.isSymbolicLink() ||
      info.uid !== process.getuid?.() ||
      (info.mode & 0o777) !== 0o700
    )
      throw new AntigravityError('update-backup-unsafe');
  }
  return directories[directories.length - 1];
}

function unchangedNative(
  left: ReturnType<typeof nativeBytes>,
  right: ReturnType<typeof nativeBytes>
): boolean {
  return (
    left.sha === right.sha &&
    left.stat.dev === right.stat.dev &&
    left.stat.ino === right.stat.ino &&
    left.stat.mtimeMs === right.stat.mtimeMs &&
    left.stat.ctimeMs === right.stat.ctimeMs &&
    left.stat.mode === right.stat.mode &&
    left.stat.uid === right.stat.uid &&
    left.stat.nlink === right.stat.nlink
  );
}

/** Construction has no filesystem or command side effects. Only explicit methods perform work. */
export function createManagedNativeUpdate(
  options: ManagedNativeUpdateOptions
): ManagedNativeUpdate {
  const binary = options.nativeBinary;
  const ccs = options.ccsDirectory;
  if (!path.isAbsolute(binary) || !path.isAbsolute(ccs))
    throw new AntigravityError('update-native-unsafe');
  const run = options.runCommand ?? command;
  let selected: ReturnType<typeof nativeBytes> | undefined;
  let original: ReturnType<typeof nativeBytes> | undefined;
  let afterUpdate: ReturnType<typeof nativeBytes> | undefined;
  const invoke = async (args: string[], capture = false): Promise<string> => {
    let output: string;
    try {
      output = await run(binary, args, capture);
    } catch {
      throw new AntigravityError('update-native-failed');
    }
    if (typeof output !== 'string' || Buffer.byteLength(output) > MAX_COMMAND_BYTES)
      throw new AntigravityError('update-output-unsafe');
    return capture ? output : '';
  };
  return {
    version: async () => {
      selected ??= nativeBytes(binary);
      const value = (await invoke(['--version'], true)).match(
        /\b(\d+\.\d+\.\d+(?:-[\w.-]+)?)\b/
      )?.[1];
      if (!value || value.length > 128) throw new AntigravityError('update-version-unavailable');
      return value;
    },
    backup: async () => {
      original = nativeBytes(binary);
      if (selected && !unchangedNative(selected, original))
        throw new AntigravityError('update-native-changed');
      const directory = privateBackupDirectory(ccs);
      const backup = path.join(directory, original.sha);
      if (fs.existsSync(backup)) {
        const existing = nativeBytes(backup);
        if (existing.sha !== original.sha || (existing.stat.mode & 0o777) !== 0o600)
          throw new AntigravityError('update-backup-changed');
      } else {
        const fd = fs.openSync(
          backup,
          fs.constants.O_WRONLY |
            fs.constants.O_CREAT |
            fs.constants.O_EXCL |
            fs.constants.O_NOFOLLOW,
          0o600
        );
        try {
          fs.writeFileSync(fd, original.bytes);
          fs.fsyncSync(fd);
        } finally {
          fs.closeSync(fd);
        }
      }
    },
    unchangedSinceBackup: () => {
      if (!original) return false;
      return unchangedNative(original, nativeBytes(binary));
    },
    update: async () => {
      try {
        await invoke(['update']);
      } finally {
        afterUpdate = nativeBytes(binary);
      }
    },
    rollback: async () => {
      if (!original || !afterUpdate) return false;
      try {
        const current = nativeBytes(binary);
        // A command without an authenticated publication receipt cannot own a
        // changed or renamed artifact, including a foreign same-bytes rename.
        // An exact no-op needs no restoration and must not rewrite its inode.
        return unchangedNative(original, afterUpdate) && unchangedNative(afterUpdate, current);
      } catch {
        return false;
      }
    },
  };
}

function failedCommandResult(
  updateAttempted: boolean,
  messageCode: ManagedFailureCode = 'update_failed'
): ManagedUpdateCommandResult {
  return {
    appId: 'antigravity-cli',
    platform: 'ubuntu',
    manager: 'native',
    status: 'failed',
    messageCode,
    updateAttempted,
    restartedProcesses: 0,
  };
}

/** Paused-path failures say whether `agy update` itself ran. */
function pausedCode(attempted: boolean): ManagedFailureCode {
  return attempted ? 'runtime_paused' : 'runtime_paused_blocked';
}

function safeVersion(value: unknown): value is string {
  return (
    typeof value === 'string' && value.length <= 128 && /^\d+\.\d+\.\d+(?:-[\w.-]+)?$/.test(value)
  );
}

/** Track the fixed native invocation even if the coordinator's final lock cleanup throws. */
export async function runManagedAntigravityUpdateCommand(
  options: ManagedUpdateCommandOptions
): Promise<ManagedUpdateCommandResult> {
  let attempted = false;
  try {
    const run = options.runCommand ?? command;
    const native = createManagedNativeUpdate({
      nativeBinary: options.nativeBinary,
      ccsDirectory: options.ccsDirectory,
      runCommand: (binary, args, capture) => {
        if (args.length === 1 && args[0] === 'update' && !capture) attempted = true;
        return run(binary, args, capture);
      },
    });
    const value = await options.execute(native);
    if (
      !value ||
      !['current', 'updated', 'failed', 'restart_failed', 'in_use'].includes(value.status) ||
      !safeVersion(value.previousVersion) ||
      !safeVersion(value.version) ||
      !Number.isSafeInteger(value.restartedProcesses) ||
      value.restartedProcesses < 0 ||
      value.restartedProcesses > 128
    )
      throw new AntigravityError('update-result-unsafe');
    // Project a fresh public object. Errors, paths, credentials and extra private
    // coordinator properties can never be spread into the command response.
    return {
      appId: 'antigravity-cli',
      platform: 'ubuntu',
      manager: 'native',
      // A running Antigravity kept an idle-only update from starting: nothing changed.
      status: value.status === 'in_use' ? 'action_required' : value.status,
      messageCode:
        value.status === 'failed' && value.runtimePaused === true
          ? pausedCode(attempted)
          : value.status,
      previousVersion: value.previousVersion,
      version: value.version,
      updateAttempted: attempted,
      restartedProcesses: value.restartedProcesses,
    };
  } catch (error) {
    const message = error instanceof AntigravityError ? error.message : '';
    return failedCommandResult(
      attempted,
      // runtime_unavailable promises nothing ran; after a counted update it stays generic.
      message === 'antigravity-runtime-unavailable' && !attempted
        ? 'runtime_unavailable'
        : message === 'update-runtime-paused'
          ? pausedCode(attempted)
          : 'update_failed'
    );
  }
}

/**
 * Whether the installed native is a reviewed build, hashed once. Only a native
 * that was read in full and matched no reviewed hash is 'unreviewed'; a read
 * failure or an unusable review list is 'unknown' and never counts as paused.
 */
export function nativeReviewState(nativeBinary: string, releaseFile?: string): NativeReviewState {
  const release = readNativeRelease(releaseFile ?? defaultNativeReleaseFile());
  if (!release.reviewed.length) return 'unknown';
  const digest = hashOwnedAntigravityNative(nativeBinary);
  if (digest === null) return 'unknown';
  return release.reviewed.some((entry) => entry.sha256 === digest) ? 'reviewed' : 'unreviewed';
}

export async function runInstalledManagedAntigravityUpdate(
  home = os.homedir(),
  {
    idleOnly = true,
    releaseFile,
  }: {
    idleOnly?: boolean;
    /** Owned fixture injection only; production reads the packaged release file. */
    releaseFile?: string;
  } = {}
) {
  try {
    const ccs = path.join(home, '.ccs');
    const components = createInstalledAntigravityComponents(ccs, home);
    // The descriptor is present (the helper checked) but unusable: no runtime can run.
    if (!components) return failedCommandResult(false, 'runtime_unavailable');
    const { nativeBinary, socketPath } = components.installed;
    return await runManagedAntigravityUpdateCommand({
      nativeBinary,
      ccsDirectory: ccs,
      execute: (native) =>
        routeManagedAntigravityUpdate({
          registry: new AntigravityProfileRegistry(ccs),
          driver: components.driver,
          native,
          idleOnly,
          nativeReview: () => nativeReviewState(nativeBinary, releaseFile),
          runtimeSocket: () => probeRuntimeSocket(socketPath),
        }),
    });
  } catch {
    // Discovery/construction failed before any tracked native update invocation.
    return failedCommandResult(false);
  }
}

/**
 * The helper passes --idle-only unless the build `agy update` installs is reviewed.
 * Anything else is refused (null) before anything runs.
 */
export function managedUpdateArguments(args: string[]): { idleOnly: boolean } | null {
  if (args.length === 0) return { idleOnly: false };
  return args.length === 1 && args[0] === '--idle-only' ? { idleOnly: true } : null;
}

/**
 * The command-line entry point. Only idleOnly crosses the command line; the
 * releaseFile fixture seam never does, and refused arguments run nothing.
 */
export async function main(
  argv: string[],
  run: typeof runInstalledManagedAntigravityUpdate = runInstalledManagedAntigravityUpdate
): Promise<ManagedUpdateCommandResult> {
  const parsed = managedUpdateArguments(argv);
  if (!parsed) return failedCommandResult(false);
  try {
    return await run(os.homedir(), { idleOnly: parsed.idleOnly });
  } catch {
    return failedCommandResult(false);
  }
}

if (require.main === module) {
  void main(process.argv.slice(2)).then((result) =>
    process.stdout.write(JSON.stringify(result) + '\n')
  );
}
