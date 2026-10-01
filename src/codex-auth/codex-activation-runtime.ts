import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { execFile, spawn } from 'child_process';
import WebSocket from 'ws';
import {
  codexProcessFingerprint,
  type CodexActivationStopPlan,
} from './codex-activation-confirmation';

export interface CodexActivationRuntime {
  stop(approval?: CodexActivationStopPlan): Promise<void>;
  start(): Promise<void>;
  dispose?(): Promise<void>;
}

export class CodexActivationRuntimeError extends Error {
  constructor(
    message: string,
    public readonly code: 'busy' | 'restart_failed' | 'confirmation_stale' = 'restart_failed',
    public readonly stopPlan?: CodexActivationStopPlan
  ) {
    super(message);
    this.name = 'CodexActivationRuntimeError';
  }
}

/** Captured credentials in env remain in memory; never serialize this object. */
export interface CodexProcessSnapshot {
  pid: number;
  ppid: number;
  startTime: string;
  state: string;
  args: string[];
  exe: string;
  cwd: string;
  env: NodeJS.ProcessEnv;
}

export interface CodexActivationRuntimeDependencies {
  platform: string;
  scan(): Promise<CodexProcessSnapshot[]>;
  acquireStartupLock(): Promise<() => Promise<void>>;
  acquireNativeStartupLock(): Promise<void>;
  releaseNativeStartupLock(): Promise<void>;
  assertIdle(processes: CodexProcessSnapshot[]): Promise<void>;
  signal(process: CodexProcessSnapshot, signal: NodeJS.Signals): Promise<void>;
  launch(process: CodexProcessSnapshot, desktop: boolean): Promise<void>;
  prepareCli(process: CodexProcessSnapshot): Promise<void>;
  launchCli(process: CodexProcessSnapshot): Promise<void>;
  verifyDaemon(process: CodexProcessSnapshot): Promise<void>;
  removeControlSocket(): void;
  sleep(milliseconds: number): Promise<void>;
  now(): number;
}

const STOP_TIMEOUT = 10000;
const KILL_TIMEOUT = 5000;
const START_TIMEOUT = 30000;

function isAlive(process: CodexProcessSnapshot): boolean {
  return process.state !== 'Z' && process.state !== 'X';
}

function isCodex(process: CodexProcessSnapshot): boolean {
  return (
    path.basename((process.exe || process.args[0] || '').replace(/ \(deleted\)$/, '')) === 'codex'
  );
}

/** Electron and its zygotes rewrite argv into one space-joined string. */
function argWords(process: CodexProcessSnapshot): string[] {
  return process.args.flatMap((arg) => arg.split(/\s+/)).filter(Boolean);
}

/**
 * Chromium zygotes rewrite their cmdline into one space-joined string, so a
 * `--type=` flag can sit inside a single argv element rather than start one.
 */
export function isDesktop(process: CodexProcessSnapshot): boolean {
  return (
    process.exe.replace(/ \(deleted\)$/, '') === '/usr/lib/chatgpt/ChatGPT' &&
    !argWords(process).some((word) => word.startsWith('--type='))
  );
}

function usesHome(process: CodexProcessSnapshot, home: string): boolean {
  const configured = process.env.CODEX_HOME;
  const defaultHome = path.join(process.env.HOME || os.homedir(), '.codex');
  return path.resolve(configured || defaultHome) === path.resolve(home);
}

function isProxy(process: CodexProcessSnapshot): boolean {
  const index = process.args.indexOf('app-server');
  return index >= 0 && process.args[index + 1] === 'proxy';
}

function isDaemon(process: CodexProcessSnapshot): boolean {
  const index = process.args.indexOf('--listen');
  return (
    isCodex(process) &&
    process.args.includes('app-server') &&
    index >= 0 &&
    process.args[index + 1]?.startsWith('unix://') === true
  );
}

function descendants(
  root: CodexProcessSnapshot,
  processes: CodexProcessSnapshot[]
): CodexProcessSnapshot[] {
  const pids = new Set([root.pid]);
  let added = true;
  while (added) {
    added = false;
    for (const process of processes) {
      if (pids.has(process.ppid) && !pids.has(process.pid)) {
        pids.add(process.pid);
        added = true;
      }
    }
  }
  return processes.filter((process) => pids.has(process.pid));
}

function sameProcess(left: CodexProcessSnapshot, right: CodexProcessSnapshot): boolean {
  return left.pid === right.pid && left.startTime === right.startTime;
}

/** The native browser helper must be owned by a restartable Codex daemon. */
function isOwnedBrowserHelper(
  process: CodexProcessSnapshot,
  processes: CodexProcessSnapshot[],
  daemons: CodexProcessSnapshot[]
): boolean {
  const parent = processes.find((entry) => entry.pid === process.ppid);
  return Boolean(
    process.args.includes('app-server') &&
      process.args.includes('stdio://') &&
      parent?.exe === '/usr/lib/chatgpt/resources/cua_node/bin/node_repl' &&
      daemons.some((daemon) => descendants(daemon, processes).includes(process))
  );
}

function stopPlan(
  roots: CodexProcessSnapshot[],
  all: CodexProcessSnapshot[],
  browserHelpers: CodexProcessSnapshot[]
): CodexActivationStopPlan {
  const targets = new Map<number, CodexProcessSnapshot>();
  for (const root of roots) {
    for (const child of descendants(root, all)) targets.set(child.pid, child);
  }
  return {
    identities: [...targets.values()]
      .sort((left, right) => left.pid - right.pid)
      .map((entry) => ({
        pid: entry.pid,
        ppid: entry.ppid,
        startTime: entry.startTime,
        fingerprint: codexProcessFingerprint(entry),
      })),
    roots: roots.map((entry) => entry.pid).sort((left, right) => left - right),
    processes: [
      ...roots.map((entry) => ({
        pid: entry.pid,
        label: isDesktop(entry)
          ? 'Codex desktop'
          : isDaemon(entry)
            ? 'Shared Codex server'
            : 'Codex CLI',
        role: isDesktop(entry)
          ? ('desktop' as const)
          : isDaemon(entry)
            ? ('daemon' as const)
            : ('cli' as const),
      })),
      ...browserHelpers.map((entry) => ({
        pid: entry.pid,
        label: 'Codex browser automation helper',
        role: 'automation' as const,
      })),
    ],
  };
}

function matchesStopPlan(
  current: CodexActivationStopPlan,
  approved: CodexActivationStopPlan
): boolean {
  return (
    JSON.stringify(current.identities) === JSON.stringify(approved.identities) &&
    JSON.stringify(current.roots) === JSON.stringify(approved.roots)
  );
}

/**
 * All writer shutdown precedes auth.json replacement. The startup flock remains
 * held until start succeeds (or the caller retries after restoring old auth).
 */
export function createCodexActivationRuntime(
  codexHome: string,
  overrides: Partial<CodexActivationRuntimeDependencies> = {}
): CodexActivationRuntime {
  const dependencies = { ...createDependencies(codexHome), ...overrides };
  let releaseLock: (() => Promise<void>) | undefined;
  let launchers: CodexProcessSnapshot[] = [];
  let cliLaunchers: CodexProcessSnapshot[] = [];
  let confirmed = false;
  const restartedClis: CodexProcessSnapshot[] = [];
  let stopped = false;

  const scan = async (): Promise<CodexProcessSnapshot[]> =>
    (await dependencies.scan()).filter(isAlive);

  const release = async (): Promise<void> => {
    const unlock = releaseLock;
    releaseLock = undefined;
    try {
      await dependencies.releaseNativeStartupLock();
    } finally {
      if (unlock) await unlock();
    }
  };

  const waitForExit = async (
    targets: CodexProcessSnapshot[],
    timeout: number
  ): Promise<boolean> => {
    const deadline = dependencies.now() + timeout;
    do {
      const processes = await scan();
      if (!targets.some((target) => processes.some((process) => sameProcess(target, process)))) {
        return true;
      }
      await dependencies.sleep(100);
    } while (dependencies.now() < deadline);
    return false;
  };

  const retire = async (
    roots: CodexProcessSnapshot[],
    all: CodexProcessSnapshot[]
  ): Promise<void> => {
    const targets = roots.flatMap((root) => descendants(root, all));
    for (const root of roots) await dependencies.signal(root, 'SIGTERM');
    if (await waitForExit(targets, STOP_TIMEOUT)) return;
    const remaining = await scan();
    for (const target of targets) {
      if (remaining.some((process) => sameProcess(target, process))) {
        await dependencies.signal(target, 'SIGTERM');
      }
    }
    if (await waitForExit(targets, 2000)) return;
    const survivors = await scan();
    for (const target of targets) {
      if (survivors.some((process) => sameProcess(target, process))) {
        await dependencies.signal(target, 'SIGKILL');
      }
    }
    if (!(await waitForExit(targets, KILL_TIMEOUT))) {
      throw new CodexActivationRuntimeError(
        'A Codex auth writer did not exit; no account was changed.'
      );
    }
  };

  const start = async (): Promise<void> => {
    if (!releaseLock) return;
    stopped = false;
    try {
      // SSH proxies from the Mac/Windows apps respawn the daemon as soon as it
      // stops; those copies wait on the startup locks and may predate the swap.
      const respawned = (await scan()).filter(
        (entry) => isDaemon(entry) && usesHome(entry, codexHome)
      );
      if (respawned.length > 0) await retire(respawned, await scan());
      if (!(await scan()).some((entry) => isDaemon(entry) && usesHome(entry, codexHome))) {
        dependencies.removeControlSocket();
      }
      // A daemon cannot bind its socket while either startup lock is held.
      await release();
      for (const launcher of launchers.filter(isDaemon)) {
        if (!(await scan()).some((entry) => isDaemon(entry) && usesHome(entry, codexHome))) {
          await dependencies.launch(launcher, false);
        }
        await dependencies.verifyDaemon(launcher);
      }
      for (const launcher of launchers.filter(isDesktop)) {
        const matchesDesktop = (
          main: CodexProcessSnapshot,
          processes: CodexProcessSnapshot[]
        ): boolean =>
          isDesktop(main) &&
          desktopUserData(main, processes) === desktopUserData(launcher, [launcher]);
        const existing = await scan();
        if (!existing.some((entry) => matchesDesktop(entry, existing))) {
          await dependencies.launch(launcher, true);
        }
        const deadline = dependencies.now() + START_TIMEOUT;
        let ready = false;
        do {
          const processes = await scan();
          const main = processes.find((entry) => matchesDesktop(entry, processes));
          ready = Boolean(
            main &&
              descendants(main, processes).some(
                (process) =>
                  isCodex(process) &&
                  usesHome(process, codexHome) &&
                  process.args.includes('app-server') &&
                  !isProxy(process)
              )
          );
          if (ready) break;
          await dependencies.sleep(250);
        } while (dependencies.now() < deadline);
        if (!ready) {
          throw new CodexActivationRuntimeError(
            'The VM Codex desktop app did not start its app server on the original display.'
          );
        }
      }
      for (const launcher of cliLaunchers) {
        const before = await scan();
        let launchFailed = false;
        try {
          await dependencies.launchCli(launcher);
        } catch {
          launchFailed = true;
        }
        const started = (await scan()).filter(
          (entry) =>
            isCodex(entry) &&
            usesHome(entry, codexHome) &&
            entry.exe === launcher.exe &&
            entry.cwd === launcher.cwd &&
            !before.some((previous) => sameProcess(previous, entry))
        );
        // Even a terminal readiness failure can leave a fresh CLI alive. Retain
        // its start identity so rollback can stop only our partial relaunch.
        restartedClis.push(...started);
        if (launchFailed || started.length === 0) {
          throw new CodexActivationRuntimeError('The confirmed Codex CLI did not restart.');
        }
      }
      stopped = false;
      await release();
    } catch {
      // Keep the lock and launch snapshot for rollback; never leak subprocess output/env.
      throw new CodexActivationRuntimeError(
        'Codex restart failed; the original account must be restored.'
      );
    }
  };

  return {
    async stop(approval?: CodexActivationStopPlan): Promise<void> {
      if (dependencies.platform !== 'linux') {
        throw new CodexActivationRuntimeError(
          'In-place Codex activation currently requires Linux.'
        );
      }
      if (stopped) return;
      if (!releaseLock) releaseLock = await dependencies.acquireStartupLock();
      else await dependencies.acquireNativeStartupLock();
      let mutationStarted = false;
      try {
        const processes = await scan();
        const writers = processes.filter(
          (process) => isCodex(process) && usesHome(process, codexHome)
        );
        const desktops = processes
          .filter(isDesktop)
          .filter((desktop) =>
            descendants(desktop, processes).some((process) => writers.includes(process))
          );
        const desktopChildren = desktops.flatMap((desktop) => descendants(desktop, processes));
        const daemons = writers.filter(isDaemon);
        const unmanaged = writers.filter(
          (process) => !isProxy(process) && !isDaemon(process) && !desktopChildren.includes(process)
        );
        const browserHelpers = unmanaged.filter((entry) =>
          isOwnedBrowserHelper(entry, processes, daemons)
        );
        const cliRoots = unmanaged.filter(
          (entry) =>
            !browserHelpers.includes(entry) &&
            !unmanaged
              .filter((other) => other !== entry)
              .some((root) => descendants(root, processes).includes(entry))
        );
        const unsupported = cliRoots.some((entry) => entry.args.includes('app-server'));
        const roots = [...desktops, ...daemons, ...cliRoots];
        const plan = stopPlan(roots, processes, browserHelpers);
        if (approval && !matchesStopPlan(plan, approval)) {
          throw new CodexActivationRuntimeError(
            'The running Codex programs changed. Review a new activation warning.',
            'confirmation_stale'
          );
        }
        if (unsupported) {
          throw new CodexActivationRuntimeError(
            'A Codex stdio server has no restartable owner. Close its owning program and try again.',
            'busy'
          );
        }
        if (approval) {
          // Preflight every fresh CLI terminal before any approved process is stopped.
          for (const cli of cliRoots) await dependencies.prepareCli(cli);
          const rechecked = await scan();
          if (
            !matchesStopPlan(stopPlan(roots, rechecked, browserHelpers), approval) ||
            rechecked.some(
              (entry) =>
                isCodex(entry) &&
                usesHome(entry, codexHome) &&
                !isProxy(entry) &&
                !approval.identities.some(
                  (identity) => identity.pid === entry.pid && identity.startTime === entry.startTime
                )
            )
          ) {
            throw new CodexActivationRuntimeError(
              'The running Codex programs changed. Review a new activation warning.',
              'confirmation_stale'
            );
          }
          confirmed = true;
          cliLaunchers = cliRoots;
        } else if (!confirmed) {
          if (unmanaged.some((entry) => !browserHelpers.includes(entry))) {
            // Ensure the offer only contains programs that can actually be restarted.
            for (const cli of cliRoots) await dependencies.prepareCli(cli);
            throw new CodexActivationRuntimeError(
              'Another Codex program is running. Review it before stopping and switching.',
              'busy',
              plan
            );
          }
          try {
            await dependencies.assertIdle(processes);
          } catch (error) {
            if (error instanceof CodexActivationRuntimeError && error.code === 'busy') {
              throw new CodexActivationRuntimeError(error.message, 'busy', plan);
            }
            throw error;
          }
        } else {
          // Rollback may only stop fresh CLI instances started by this transaction.
          if (
            cliRoots.some((entry) => !restartedClis.some((started) => sameProcess(entry, started)))
          ) {
            throw new CodexActivationRuntimeError('Another Codex CLI started during recovery.');
          }
        }
        if (launchers.length === 0) {
          launchers = [
            ...daemons,
            ...desktops.map((desktop) => desktopLauncher(desktop, processes)),
          ];
        }
        mutationStarted = true;
        await retire(desktops, processes);
        await retire(daemons, processes);
        await retire(cliRoots, processes);
        // A daemon respawned by an SSH proxy is retired again in start(), after the swap.
        const survivors = (await scan()).filter(
          (process) =>
            isCodex(process) &&
            usesHome(process, codexHome) &&
            !isProxy(process) &&
            !isDaemon(process)
        );
        if (survivors.length > 0) {
          throw new CodexActivationRuntimeError(
            'A new Codex auth writer started; no account was changed.'
          );
        }
        if (!(await scan()).some((entry) => isDaemon(entry) && usesHome(entry, codexHome))) {
          dependencies.removeControlSocket();
        }
        stopped = true;
      } catch (error) {
        if (mutationStarted) {
          try {
            await start();
          } catch (recoveryError) {
            await release();
            throw recoveryError;
          }
        } else {
          await release();
        }
        if (error instanceof CodexActivationRuntimeError) throw error;
        throw new CodexActivationRuntimeError(
          'Could not safely stop Codex; no account was changed.'
        );
      }
    },
    start,
    dispose: release,
  };
}

function desktopUserData(desktop: CodexProcessSnapshot, processes: CodexProcessSnapshot[]): string {
  const args = descendants(desktop, processes).flatMap(argWords);
  const inline = args.find((arg) => arg.startsWith('--user-data-dir='));
  if (inline) return inline.slice('--user-data-dir='.length);
  const index = args.indexOf('--user-data-dir');
  return index >= 0 ? args[index + 1] || '' : '';
}

function desktopLauncher(
  desktop: CodexProcessSnapshot,
  processes: CodexProcessSnapshot[]
): CodexProcessSnapshot {
  const children = descendants(desktop, processes);
  const session = children.find((process) => process.env.DISPLAY || process.env.WAYLAND_DISPLAY);
  const env = { ...(session?.env || desktop.env) };
  // Electron clears main's environ in-place. Recover the desktop session from
  // its child, excluding app-server-specific pipe and originator overrides.
  for (const name of Object.keys(env)) {
    if (
      name.startsWith('CODEX_') ||
      name.startsWith('BROWSER_USE_') ||
      name === 'RUST_LOG' ||
      name === 'LOG_FORMAT'
    ) {
      delete env[name];
    }
  }
  if (!env.DISPLAY && !env.WAYLAND_DISPLAY) {
    throw new CodexActivationRuntimeError(
      'Cannot recover the VM Codex desktop display; no account was changed.'
    );
  }
  const args = argWords(desktop);
  if (!args.some((arg) => arg.startsWith('--user-data-dir'))) {
    const userData = children.flatMap(argWords).find((arg) => arg.startsWith('--user-data-dir='));
    args.push(userData || `--user-data-dir=${path.join(os.homedir(), '.config', 'Codex')}`);
  }
  return { ...desktop, args, env };
}

function readProcesses(): CodexProcessSnapshot[] {
  const processes: CodexProcessSnapshot[] = [];
  for (const name of fs.readdirSync('/proc')) {
    if (!/^\d+$/.test(name)) continue;
    const directory = path.join('/proc', name);
    try {
      if (fs.statSync(directory).uid !== process.getuid?.()) continue;
      const stat = fs.readFileSync(path.join(directory, 'stat'), 'utf8');
      const fields = stat.slice(stat.lastIndexOf(')') + 2).split(' ');
      const args = fs
        .readFileSync(path.join(directory, 'cmdline'), 'utf8')
        .split('\0')
        .filter(Boolean);
      if (args.length === 0) continue;
      let exe = args[0];
      try {
        exe = fs.readlinkSync(path.join(directory, 'exe')).replace(/ \(deleted\)$/, '');
      } catch (error) {
        // Non-dumpable sshd/dbus helpers can share the user's uid. Their readable
        // argv still supplies ancestry; no auth-writer metadata is needed.
        const candidate = path.basename(exe) === 'codex' || exe === '/usr/lib/chatgpt/ChatGPT';
        if ((error as NodeJS.ErrnoException).code !== 'EACCES' || candidate) throw error;
      }
      const relevant = path.basename(exe) === 'codex' || exe === '/usr/lib/chatgpt/ChatGPT';
      const env: NodeJS.ProcessEnv = {};
      if (relevant) {
        for (const item of fs.readFileSync(path.join(directory, 'environ'), 'utf8').split('\0')) {
          const separator = item.indexOf('=');
          if (separator > 0) env[item.slice(0, separator)] = item.slice(separator + 1);
        }
      }
      processes.push({
        pid: Number(name),
        ppid: Number(fields[1]),
        state: fields[0],
        startTime: fields[19],
        args,
        exe,
        cwd: relevant ? fs.readlinkSync(path.join(directory, 'cwd')) : '',
        env,
      });
    } catch (error) {
      const code = (error as NodeJS.ErrnoException).code;
      if (code === 'ENOENT' || code === 'ESRCH') continue;
      throw new CodexActivationRuntimeError('Cannot inspect all local Codex processes safely.');
    }
  }
  return processes;
}

async function acquireLock(lockFile: string): Promise<() => Promise<void>> {
  const child = spawn(
    'flock',
    ['-x', '-w', '10', lockFile, 'sh', '-c', 'printf "locked\\n"; cat >/dev/null'],
    { stdio: ['pipe', 'pipe', 'ignore'] }
  );
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => {
      child.stdin.end();
      reject(new CodexActivationRuntimeError('Timed out waiting for the Codex startup lock.'));
    }, 12000);
    child.once('error', () => {
      clearTimeout(timer);
      reject(new CodexActivationRuntimeError('Could not acquire the Codex startup lock.'));
    });
    child.once('exit', () => {
      clearTimeout(timer);
      reject(new CodexActivationRuntimeError('The Codex startup lock is busy.', 'busy'));
    });
    child.stdout.once('data', () => {
      clearTimeout(timer);
      resolve();
    });
  });
  return async (): Promise<void> => {
    await new Promise<void>((resolve) => {
      if (child.exitCode !== null) {
        resolve();
        return;
      }
      child.once('exit', () => resolve());
      child.stdin.end();
    });
  };
}

function runCommand(
  command: string,
  args: string[],
  env: NodeJS.ProcessEnv,
  cwd: string
): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(
      command,
      args,
      { env, cwd, timeout: 10000, maxBuffer: 1024 * 1024 },
      (error, stdout) => {
        if (error) reject(new CodexActivationRuntimeError('A Codex status check failed.'));
        else resolve(stdout);
      }
    );
  });
}

function runCliRestartHelper(target: CodexProcessSnapshot, check: boolean): Promise<void> {
  const helper = path.resolve(__dirname, '../../scripts/app-updates/app_update_confirmed_codex.py');
  return new Promise((resolve, reject) => {
    const child = spawn('/usr/bin/python3', [helper, ...(check ? ['--check'] : [])], {
      stdio: ['pipe', 'pipe', 'ignore'],
    });
    let output = '';
    const timer = setTimeout(() => {
      child.kill('SIGTERM');
      reject(new CodexActivationRuntimeError('The Codex CLI terminal restart timed out.'));
    }, 40_000);
    child.once('error', () => {
      clearTimeout(timer);
      reject(new CodexActivationRuntimeError('Could not prepare the Codex CLI terminal.'));
    });
    child.stdout.on('data', (data: Buffer) => {
      output += data.toString('utf8');
      if (output.length > 16_384) child.kill('SIGTERM');
    });
    child.once('exit', (code) => {
      clearTimeout(timer);
      try {
        if (code !== 0 || JSON.parse(output).success !== true) {
          throw new CodexActivationRuntimeError('Could not reopen Codex in a fresh terminal.');
        }
        resolve();
      } catch {
        reject(new CodexActivationRuntimeError('Could not reopen Codex in a fresh terminal.'));
      }
    });
    child.stdin.on('error', () => {
      // Closed private stdin is reported by the sanitized exit handler.
    });
    // Never serialize this private packet into a file, argv, HTTP, or a log.
    child.stdin.end(JSON.stringify({ exe: target.exe, cwd: target.cwd, env: target.env }));
  });
}

function createDependencies(codexHome: string): CodexActivationRuntimeDependencies {
  let nativeRelease: (() => Promise<void>) | undefined;
  let nativeLockFile: string | undefined;
  const acquireNativeStartupLock = async (): Promise<void> => {
    if (nativeRelease) return;
    const socket = path.join(codexHome, 'app-server-control', 'app-server-control.sock');
    if (!nativeLockFile) {
      try {
        if (fs.lstatSync(socket).isSymbolicLink()) {
          nativeLockFile = `${fs.realpathSync(socket)}.lock`;
        }
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
      }
    }
    if (nativeLockFile) nativeRelease = await acquireLock(nativeLockFile);
  };
  const releaseNativeStartupLock = async (): Promise<void> => {
    const unlock = nativeRelease;
    nativeRelease = undefined;
    if (unlock) await unlock();
  };
  return {
    platform: process.platform,
    scan: async () => readProcesses(),
    acquireStartupLock: async () => {
      const directory = path.join(codexHome, 'app-server-control');
      fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
      const legacyRelease = await acquireLock(path.join(directory, 'app-server-startup.lock'));
      try {
        // Modern Codex hashes CODEX_HOME into a /tmp socket and locks next to it.
        // Resolve its existing symlink rather than guessing that private hash.
        await acquireNativeStartupLock();
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT') {
          await legacyRelease();
          throw new CodexActivationRuntimeError('Could not acquire the native Codex startup lock.');
        }
      }
      return legacyRelease;
    },
    acquireNativeStartupLock,
    releaseNativeStartupLock,
    assertIdle: async (processes) => {
      for (const daemon of processes.filter(
        (entry) => isDaemon(entry) && usesHome(entry, codexHome)
      )) {
        await assertDaemonIdle(daemon, codexHome);
      }
      for (const desktop of processes
        .filter(isDesktop)
        .filter((entry) =>
          descendants(entry, processes).some(
            (child) => isCodex(child) && usesHome(child, codexHome) && !isProxy(child)
          )
        )) {
        assertDesktopIdle(desktop, processes, codexHome);
      }
    },
    signal: async (target, signal) => {
      const current = readProcesses().find((entry) => sameProcess(target, entry));
      if (current && isAlive(current)) {
        if (codexProcessFingerprint(current) !== codexProcessFingerprint(target)) {
          throw new CodexActivationRuntimeError(
            'A Codex process changed executable during shutdown; no account was changed.'
          );
        }
        try {
          process.kill(target.pid, signal);
        } catch (error) {
          if ((error as NodeJS.ErrnoException).code !== 'ESRCH')
            throw new CodexActivationRuntimeError('Could not stop a Codex auth writer.');
        }
      }
    },
    launch: async (target, desktop) => {
      let stream: number | undefined;
      try {
        stream = fs.openSync(
          desktop ? '/dev/null' : path.join(codexHome, 'app-server-control', 'app-server.log'),
          'a',
          0o600
        );
        const child = spawn(target.exe, target.args.slice(1), {
          cwd: target.cwd,
          env: target.env,
          detached: true,
          argv0: target.args[0],
          stdio: ['ignore', stream, stream],
        });
        await new Promise<void>((resolve, reject) => {
          child.once('spawn', () => resolve());
          child.once('error', () =>
            reject(new CodexActivationRuntimeError('Could not launch Codex.'))
          );
        });
        child.unref();
      } finally {
        if (stream !== undefined) fs.closeSync(stream);
      }
    },
    prepareCli: (target) => runCliRestartHelper(target, true),
    launchCli: (target) => runCliRestartHelper(target, false),
    verifyDaemon: async (target) => {
      const deadline = Date.now() + START_TIMEOUT;
      do {
        try {
          const status = JSON.parse(
            await runCommand(
              target.exe,
              ['app-server', 'daemon', 'version'],
              target.env,
              target.cwd
            )
          ) as { status?: string };
          if (status.status === 'running') return;
        } catch {
          /* Retry readiness without exposing stderr. */
        }
        await new Promise((resolve) => setTimeout(resolve, 250));
      } while (Date.now() < deadline);
      throw new CodexActivationRuntimeError('The shared Codex app server did not become healthy.');
    },
    removeControlSocket: () => {
      const socket = path.join(codexHome, 'app-server-control', 'app-server-control.sock');
      try {
        const target = fs.lstatSync(socket).isSymbolicLink() ? fs.readlinkSync(socket) : socket;
        if (fs.lstatSync(target).isSocket()) fs.unlinkSync(target);
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT')
          throw new CodexActivationRuntimeError(
            'Could not remove the retired Codex control socket.'
          );
      }
    },
    sleep: (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
    now: Date.now,
  };
}

async function assertDaemonIdle(daemon: CodexProcessSnapshot, codexHome: string): Promise<void> {
  const index = daemon.args.indexOf('--listen');
  const endpoint = daemon.args[index + 1];
  const socketPath =
    endpoint === 'unix://'
      ? path.join(codexHome, 'app-server-control', 'app-server-control.sock')
      : endpoint.slice('unix://'.length);
  const socket = new WebSocket(`ws+unix://${socketPath}:/`);
  const rpc = createRpc(socket);
  try {
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new CodexActivationRuntimeError('Codex status connection timed out.', 'busy')),
        5000
      );
      socket.once('open', () => {
        clearTimeout(timer);
        resolve();
      });
      socket.once('error', () => {
        clearTimeout(timer);
        reject(new CodexActivationRuntimeError('Cannot inspect running Codex tasks.', 'busy'));
      });
    });
    await rpc.request('initialize', {
      clientInfo: { name: 'ccs-activation', version: '1.0.0' },
      capabilities: { experimentalApi: true },
    });
    socket.send(JSON.stringify({ method: 'initialized', params: {} }));
    let cursor: string | undefined;
    const cursors = new Set<string>();
    do {
      const loaded = (await rpc.request('thread/loaded/list', {
        limit: 100,
        ...(cursor ? { cursor } : {}),
      })) as { data?: string[]; nextCursor?: string | null };
      if (!Array.isArray(loaded.data))
        throw new CodexActivationRuntimeError(
          'Cannot determine whether shared Codex work is idle.',
          'busy'
        );
      for (const threadId of loaded.data) {
        const result = (await rpc.request('thread/read', { threadId, includeTurns: false })) as {
          thread?: { status?: { type?: string } };
        };
        const type = result.thread?.status?.type;
        if (type !== 'idle' && type !== 'notLoaded' && type !== 'systemError') {
          throw new CodexActivationRuntimeError(
            'Codex work is running. Wait for all Codex tasks to finish, then activate the account again.',
            'busy'
          );
        }
      }
      cursor = loaded.nextCursor || undefined;
      if (cursor && cursors.has(cursor))
        throw new CodexActivationRuntimeError(
          'Codex task status pagination did not finish.',
          'busy'
        );
      if (cursor) cursors.add(cursor);
    } while (cursor);
  } catch (error) {
    if (error instanceof CodexActivationRuntimeError) throw error;
    throw new CodexActivationRuntimeError(
      'Cannot determine whether shared Codex work is idle.',
      'busy'
    );
  } finally {
    rpc.close();
    socket.terminate();
  }
}

/** Only inspect lifecycle event types; never expose rollout prompts or token data. */
export function readRolloutLifecycle(file: string): 'idle' | 'busy' | 'unknown' {
  const descriptor = fs.openSync(file, 'r');
  try {
    const size = fs.fstatSync(descriptor).size;
    let position = size;
    let carry = '';
    // A missing end marker is not evidence of idleness, even for very large rollouts.
    while (position > 0) {
      const length = Math.min(position, 65536);
      position -= length;
      const chunk = Buffer.alloc(length);
      fs.readSync(descriptor, chunk, 0, length, position);
      const lines = (chunk.toString('utf8') + carry).split('\n');
      carry = position > 0 ? lines.shift() || '' : '';
      for (let index = lines.length - 1; index >= 0; index--) {
        try {
          const entry = JSON.parse(lines[index]) as {
            type?: string;
            payload?: { type?: string };
          };
          if (entry.type !== 'event_msg') continue;
          const event = entry.payload?.type;
          if (event === 'task_complete' || event === 'task_aborted' || event === 'turn_aborted')
            return 'idle';
          if (event === 'task_started') return 'busy';
        } catch {
          /* Ignore truncated lines, never print their content. */
        }
      }
      if (carry.length > 1024 * 1024) return 'unknown';
    }
    return 'unknown';
  } finally {
    fs.closeSync(descriptor);
  }
}

function assertDesktopIdle(
  desktop: CodexProcessSnapshot,
  processes: CodexProcessSnapshot[],
  codexHome: string
): void {
  const servers = descendants(desktop, processes).filter(
    (entry) => isCodex(entry) && entry.args.includes('app-server') && !isProxy(entry)
  );
  if (servers.length === 0) {
    throw new CodexActivationRuntimeError(
      'Cannot inspect the VM Codex desktop app server safely.',
      'busy'
    );
  }
  const sessionRoots = ['sessions', 'archived_sessions'].map((name) => {
    try {
      return `${fs.realpathSync(path.join(codexHome, name))}${path.sep}`;
    } catch {
      return `${path.join(codexHome, name)}${path.sep}`;
    }
  });
  try {
    for (const server of servers) {
      const directory = `/proc/${server.pid}/fd`;
      for (const name of fs.readdirSync(directory)) {
        let file: string;
        try {
          file = fs.readlinkSync(path.join(directory, name)).replace(/ \(deleted\)$/, '');
        } catch (error) {
          if ((error as NodeJS.ErrnoException).code === 'ENOENT') continue;
          throw error;
        }
        if (!sessionRoots.some((root) => file.startsWith(root)) || !file.endsWith('.jsonl'))
          continue;
        if (readRolloutLifecycle(path.join(directory, name)) !== 'idle') {
          throw new CodexActivationRuntimeError(
            'The VM Codex desktop has an active task. Wait for it to finish, then activate the account again.',
            'busy'
          );
        }
      }
    }
  } catch (error) {
    if (error instanceof CodexActivationRuntimeError) throw error;
    throw new CodexActivationRuntimeError(
      'Cannot confirm that VM Codex desktop tasks are idle.',
      'busy'
    );
  }
}

function createRpc(socket: WebSocket): {
  request(method: string, params: unknown): Promise<unknown>;
  close(): void;
} {
  let nextId = 0;
  const pending = new Map<
    number,
    { resolve(value: unknown): void; reject(error: Error): void; timer: NodeJS.Timeout }
  >();
  const fail = (): void => {
    for (const entry of pending.values()) {
      clearTimeout(entry.timer);
      entry.reject(new CodexActivationRuntimeError('Codex status RPC was unavailable.', 'busy'));
    }
    pending.clear();
  };
  socket.on('error', fail);
  socket.on('close', fail);
  socket.on('message', (message: WebSocket.RawData) => {
    try {
      const response = JSON.parse(message.toString()) as {
        id?: number;
        result?: unknown;
        error?: unknown;
      };
      const entry = response.id === undefined ? undefined : pending.get(response.id);
      if (!entry || response.id === undefined) return;
      clearTimeout(entry.timer);
      pending.delete(response.id);
      if (response.error)
        entry.reject(new CodexActivationRuntimeError('Codex status RPC was rejected.', 'busy'));
      else entry.resolve(response.result);
    } catch {
      /* Protocol notifications are never printed or persisted. */
    }
  });
  return {
    request: (method, params) =>
      new Promise((resolve, reject) => {
        const id = ++nextId;
        const timer = setTimeout(() => {
          pending.delete(id);
          reject(new CodexActivationRuntimeError('Codex status RPC timed out.', 'busy'));
        }, 5000);
        pending.set(id, { resolve, reject, timer });
        socket.send(JSON.stringify({ id, method, params }));
      }),
    close: fail,
  };
}
