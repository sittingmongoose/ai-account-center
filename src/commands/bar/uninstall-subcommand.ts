/** Reversible native-app uninstall. Account configuration and authentication are preserved. */
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { getCcsDir } from '../../config/config-loader-facade';
import { ConfigError } from '../../errors/error-types';
import {
  getMacAppPaths,
  inspectOwnedMacApp,
  MAC_APP_NAME,
  MAC_BUNDLE_ID,
  MAC_EXECUTABLE,
  MAC_LEGACY_APP_NAME,
  PRODUCT_NAME,
  readMacPlist,
  resolveOwnedMacApp,
  runNativeCommand,
} from './native-app-paths';
import type { NativeCommandRunner, NativeAppOptions } from './native-app-paths';

export interface NativeAppProcess {
  pid: number;
  executable: string;
}
export interface UninstallDeps {
  getCcsDir: () => string;
  getAppsDir: () => string;
  appName: string;
  platform: NodeJS.Platform;
  home: string;
  runner: NativeCommandRunner;
  readPlist: NonNullable<NativeAppOptions['readPlist']>;
  listAppProcesses: (executables: string[]) => NativeAppProcess[];
  readProcessIdentity: (pid: number) => string | null;
  disableLaunchAgent: (agent: string, executables: string[]) => boolean;
  restoreLaunchAgent: (agent: string) => void;
  movePath: (source: string, destination: string) => void;
}

interface PreservedTarget {
  source: string;
  name: string;
  stat: fs.Stats;
}

function statIfPresent(file: string): fs.Stats | null {
  try {
    return fs.lstatSync(file);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw error;
  }
}

function requireSafeFile(file: string): fs.Stats {
  const stat = statIfPresent(file);
  if (
    !stat ||
    !stat.isFile() ||
    stat.isSymbolicLink() ||
    (typeof process.getuid === 'function' && stat.uid !== process.getuid())
  ) {
    throw new ConfigError(`Refusing an unexpected uninstall target: ${file}`);
  }
  return stat;
}

function listProcesses(executables: string[], runner: NativeCommandRunner): NativeAppProcess[] {
  const result = runner('/bin/ps', ['-axww', '-o', 'pid=,comm=']);
  if (result.status !== 0)
    throw new ConfigError('Cannot verify whether the native app is running.');
  const processes: NativeAppProcess[] = [];
  for (const line of result.stdout.split('\n')) {
    const match = /^\s*(\d+)\s+(.+?)\s*$/.exec(line);
    if (!match) continue;
    if (match[2] === MAC_EXECUTABLE) {
      throw new ConfigError(
        'An unidentified native app process is running; quit it before uninstalling.'
      );
    }
    if (executables.includes(match[2])) {
      const pid = Number(match[1]);
      if (!Number.isSafeInteger(pid) || pid <= 0)
        throw new ConfigError('Cannot verify a native app process identity.');
      processes.push({ pid, executable: match[2] });
    }
  }
  return processes;
}

function processIdentity(pid: number, runner: NativeCommandRunner): string | null {
  const result = runner('/bin/ps', ['-p', String(pid), '-o', 'lstart=']);
  return result.status === 0 ? result.stdout.trim() || null : null;
}

function disableAgent(_agent: string, executables: string[], runner: NativeCommandRunner): boolean {
  if (typeof process.getuid !== 'function')
    throw new ConfigError('Cannot identify the graphical user.');
  const target = `gui/${process.getuid()}/${MAC_BUNDLE_ID}`;
  const current = runner('/bin/launchctl', ['print', target]);
  if (current.status !== 0) {
    if (/could not find (?:specified )?service|service not found/i.test(current.stderr))
      return false;
    throw new ConfigError('Cannot safely verify the native app launch agent.');
  }
  const program = /(?:^|\n)\s*program = ([^\n]+)/.exec(current.stdout)?.[1].trim();
  if (!program || !executables.includes(program))
    throw new ConfigError('Refusing to stop a foreign launch agent.');
  if (/(?:^|\n)\s*pid = [1-9]\d*/.test(current.stdout)) {
    throw new ConfigError(
      `Quit ${PRODUCT_NAME} before uninstalling; the launch agent has a running process.`
    );
  }
  const result = runner('/bin/launchctl', ['bootout', target]);
  if (result.status !== 0) throw new ConfigError('Unable to disable the native app launch agent.');
  return true;
}

function preflightParents(base: string, home: string): void {
  let current = base;
  while (current !== home) {
    const stat = statIfPresent(current);
    if (stat && (!stat.isDirectory() || stat.isSymbolicLink())) {
      throw new ConfigError(`Refusing an unexpected native app parent path: ${current}`);
    }
    const parent = path.dirname(current);
    if (parent === current) throw new ConfigError('Invalid native app parent location.');
    current = parent;
  }
}

function sameFile(source: string, expected: fs.Stats): boolean {
  const current = statIfPresent(source);
  return (
    current !== null &&
    current.dev === expected.dev &&
    current.ino === expected.ino &&
    current.mode === expected.mode &&
    current.size === expected.size &&
    current.mtimeMs === expected.mtimeMs
  );
}

export async function handleBarUninstall(
  _args: string[],
  deps: Partial<UninstallDeps> = {}
): Promise<void> {
  // Existing path DI permits isolated fixtures on Linux; production remains macOS-only.
  const platform =
    deps.platform ?? (deps.getAppsDir && deps.getCcsDir ? 'darwin' : process.platform);
  if (platform !== 'darwin') {
    console.error(`[X] ${PRODUCT_NAME} native uninstall is supported on macOS only.`);
    process.exitCode = 1;
    return;
  }
  try {
    const appsDir = path.resolve(
      (deps.getAppsDir ?? (() => path.join(os.homedir(), 'Applications')))()
    );
    const home = path.resolve(
      deps.home ?? (deps.getAppsDir ? path.dirname(appsDir) : os.homedir())
    );
    const ccsDir = path.resolve((deps.getCcsDir ?? getCcsDir)());
    const runner = deps.runner ?? runNativeCommand;
    const options: NativeAppOptions = {
      runner,
      ...(deps.readPlist ? { readPlist: deps.readPlist } : {}),
    };
    const paths = getMacAppPaths(home, appsDir);
    preflightParents(appsDir, home);
    resolveOwnedMacApp(appsDir, options); // Check every occupied name before changing anything.
    if (deps.appName && ![MAC_APP_NAME, MAC_LEGACY_APP_NAME].includes(deps.appName)) {
      throw new ConfigError('Refusing an unexpected native app name.');
    }
    const candidates =
      deps.appName === MAC_LEGACY_APP_NAME
        ? [paths.legacy, paths.canonical]
        : [paths.canonical, paths.legacy];
    const targets: PreservedTarget[] = [];
    for (const source of candidates) {
      const stat = statIfPresent(source);
      if (stat) targets.push({ source, name: path.basename(source), stat });
    }
    const executables = [paths.canonical, paths.legacy].map((app) =>
      path.join(app, 'Contents', 'MacOS', MAC_EXECUTABLE)
    );
    const agentDir = path.join(home, 'Library', 'LaunchAgents');
    preflightParents(agentDir, home);
    const agent = path.join(agentDir, `${MAC_BUNDLE_ID}.plist`);
    const agentStat = statIfPresent(agent);
    if (agentStat) {
      requireSafeFile(agent);
      const document = deps.readPlist?.(agent) ?? readMacPlist(agent, runner);
      const programArgs = document.ProgramArguments;
      if (
        document.Label !== MAC_BUNDLE_ID ||
        !Array.isArray(programArgs) ||
        typeof programArgs[0] !== 'string' ||
        !executables.includes(programArgs[0]) ||
        (document.Program !== undefined && document.Program !== programArgs[0])
      ) {
        throw new ConfigError('Refusing a foreign native app launch agent.');
      }
      targets.push({ source: agent, name: path.basename(agent), stat: agentStat });
    }
    const barDir = path.join(ccsDir, 'bar');
    const barDirStat = statIfPresent(barDir);
    if (barDirStat && (!barDirStat.isDirectory() || barDirStat.isSymbolicLink())) {
      throw new ConfigError('Refusing an unexpected version pin directory.');
    }
    const pin = path.join(barDir, '.version');
    const pinStat = statIfPresent(pin);
    if (pinStat) targets.push({ source: pin, name: 'version-pin', stat: requireSafeFile(pin) });
    if (targets.length === 0) {
      console.log(`[i] ${PRODUCT_NAME} is not installed — nothing to remove.`);
      return;
    }
    const inspectProcesses = deps.listAppProcesses ?? ((values) => listProcesses(values, runner));
    const readIdentity = deps.readProcessIdentity ?? ((pid) => processIdentity(pid, runner));
    const ensureNotRunning = () => {
      for (const process of inspectProcesses(executables)) {
        if (
          !Number.isSafeInteger(process.pid) ||
          process.pid <= 0 ||
          !executables.includes(process.executable)
        ) {
          throw new ConfigError('Cannot verify a reported native app process.');
        }
        // No signal is sent, even when a PID's birth identity cannot be verified.
        const identity = readIdentity(process.pid);
        throw new ConfigError(
          identity
            ? `Quit ${PRODUCT_NAME} (PID ${process.pid}) before uninstalling.`
            : 'Cannot verify that the native app has stopped; no uninstall changes were made.'
        );
      }
    };
    ensureNotRunning();
    const base = path.join(home, 'Library', 'Application Support', 'CCS Bar', 'Backups');
    preflightParents(base, home);
    fs.mkdirSync(base, { recursive: true, mode: 0o700 });
    fs.chmodSync(base, 0o700);
    const backup = fs.mkdtempSync(path.join(base, 'uninstall-'));
    fs.chmodSync(backup, 0o700);
    const move = deps.movePath ?? fs.renameSync;
    const moved: { source: string; saved: string }[] = [];
    let unloaded = false;
    try {
      for (const target of targets) {
        if (!sameFile(target.source, target.stat)) {
          throw new ConfigError(
            'Native uninstall targets changed during preflight; refusing to continue.'
          );
        }
      }
      resolveOwnedMacApp(appsDir, options);
      if (agentStat)
        unloaded = (
          deps.disableLaunchAgent ?? ((file, values) => disableAgent(file, values, runner))
        )(agent, executables);
      ensureNotRunning();
      for (const target of targets) {
        if (!sameFile(target.source, target.stat))
          throw new ConfigError(
            'Native uninstall targets changed during preflight; refusing to continue.'
          );
        if (
          target.source === paths.canonical ||
          (!target.stat.isSymbolicLink() && target.source === paths.legacy)
        ) {
          inspectOwnedMacApp(target.source, options);
        }
        const saved = path.join(backup, target.name);
        // rename preserves the verified symlink itself; it never follows its target.
        move(target.source, saved);
        moved.push({ source: target.source, saved });
      }
    } catch (error) {
      let restored = true;
      for (const target of moved.reverse()) {
        try {
          if (statIfPresent(target.source))
            throw new ConfigError('A replacement uninstall target appeared.');
          fs.renameSync(target.saved, target.source);
        } catch {
          restored = false;
        }
      }
      if (unloaded && restored) {
        try {
          if (deps.restoreLaunchAgent) deps.restoreLaunchAgent(agent);
          else {
            const result = runner('/bin/launchctl', [
              'bootstrap',
              `gui/${process.getuid?.()}`,
              agent,
            ]);
            if (result.status !== 0) restored = false;
          }
        } catch {
          restored = false;
        }
      }
      if (restored) {
        try {
          fs.rmdirSync(backup);
        } catch {
          restored = false;
        }
      }
      if (!restored)
        console.error(
          `[X] Uninstall rollback needs recovery from ${backup}; account data was preserved.`
        );
      throw error;
    }
    console.log(`[OK] ${PRODUCT_NAME} uninstalled. Preserved app backup: ${backup}`);
    console.log(
      '[i] Account configuration and authentication were preserved. Run `ai-account-center bar install` to reinstall.'
    );
  } catch (error) {
    console.error(`[X] ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 1;
  }
}
