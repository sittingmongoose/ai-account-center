/** Fixed private updater entry point; importing never runs a command. */
import fs from 'fs';
import os from 'os';
import path from 'path';
import { createHash } from 'crypto';
import { execFile } from 'child_process';
import { AntigravityProfileRegistry } from './registry';
import { createInstalledAntigravityComponents } from './production-runtime';
import { updateManagedAntigravity, type ManagedNativeUpdate } from './managed-updater';
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

type ManagedCoordinatorResult = Awaited<ReturnType<typeof updateManagedAntigravity>>;

export interface ManagedUpdateCommandOptions extends ManagedNativeUpdateOptions {
  /** Explicit coordinator injection; importing or constructing options performs no work. */
  execute(native: ManagedNativeUpdate): Promise<ManagedCoordinatorResult>;
}

export interface ManagedUpdateCommandResult {
  appId: 'antigravity-cli';
  platform: 'ubuntu';
  manager: 'native';
  status: ManagedCoordinatorResult['status'];
  messageCode: ManagedCoordinatorResult['status'] | 'update_failed';
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
      const value = (await invoke(['--version'], true)).match(
        /\b(\d+\.\d+\.\d+(?:-[\w.-]+)?)\b/
      )?.[1];
      if (!value || value.length > 128) throw new AntigravityError('update-version-unavailable');
      return value;
    },
    backup: async () => {
      original = nativeBytes(binary);
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

function failedCommandResult(updateAttempted: boolean): ManagedUpdateCommandResult {
  return {
    appId: 'antigravity-cli',
    platform: 'ubuntu',
    manager: 'native',
    status: 'failed',
    messageCode: 'update_failed',
    updateAttempted,
    restartedProcesses: 0,
  };
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
      !['current', 'updated', 'failed', 'restart_failed'].includes(value.status) ||
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
      status: value.status,
      messageCode: value.status,
      previousVersion: value.previousVersion,
      version: value.version,
      updateAttempted: attempted,
      restartedProcesses: value.restartedProcesses,
    };
  } catch {
    return failedCommandResult(attempted);
  }
}

export async function runInstalledManagedAntigravityUpdate(home = os.homedir()) {
  try {
    const ccs = path.join(home, '.ccs');
    const components = createInstalledAntigravityComponents(ccs, home);
    if (!components) throw new AntigravityError('update-runtime-unavailable');
    return await runManagedAntigravityUpdateCommand({
      nativeBinary: components.installed.nativeBinary,
      ccsDirectory: ccs,
      execute: (native) =>
        updateManagedAntigravity({
          registry: new AntigravityProfileRegistry(ccs),
          driver: components.driver,
          native,
        }),
    });
  } catch {
    // Discovery/construction failed before any tracked native update invocation.
    return failedCommandResult(false);
  }
}

if (require.main === module) {
  void runInstalledManagedAntigravityUpdate()
    .then((result) => process.stdout.write(JSON.stringify(result) + '\n'))
    .catch(() => {
      process.stdout.write(JSON.stringify(failedCommandResult(false)) + '\n');
    });
}
