/**
 * A Codex CLI update whose helper is killed during stop()'s own recovery. Run as its own process
 * with a temporary HOME: the real bridge and the real runtime, synthetic processes only. The daemon
 * is retired, a new writer appears so stop() must recover, and the recovery's readiness check
 * SIGKILLs this process, as a parent timeout would. Whatever is on disk then is what the next click
 * finds. The marker is read and written here exactly as the bridge's own helpers do.
 */
import * as fs from 'fs';
import * as path from 'path';
import {
  createCodexActivationRuntime,
  type CodexActivationRuntimeDependencies,
  type CodexProcessSnapshot,
} from '../../../../src/codex-auth/codex-activation-runtime';

const bridge = require('../../../../scripts/app-updates/app_update_codex.cjs');
const home = process.env.HOME as string;
const codexHome = path.join(home, '.codex');
const markerFile = path.join(home, '.ccs/app-updates/codex-cli-pending-restart.json');

function processFixture(pid: number, args: string[]): CodexProcessSnapshot {
  return {
    pid,
    ppid: 1,
    startTime: String(pid),
    state: 'S',
    args,
    exe: args[0],
    cwd: '/fixture',
    env: { HOME: home, CODEX_HOME: codexHome },
  };
}

let current = [processFixture(10, ['/fixture/bin/codex', 'app-server', '--listen', 'unix://'])];
let nextPid = 100;
const processes: CodexActivationRuntimeDependencies = {
  platform: 'linux',
  scan: async () => [...current],
  acquireStartupLock: async () => async () => {},
  acquireNativeStartupLock: async () => {},
  releaseNativeStartupLock: async () => {},
  assertIdle: async () => {},
  signal: async (target) => {
    current = current.filter((entry) => entry.pid !== target.pid);
    // A new writer appears once the daemon is gone, so stop() must recover.
    current.push(processFixture(30, ['/fixture/bin/codex', 'exec']));
  },
  launch: async (target) => {
    current.push({ ...target, pid: nextPid, startTime: String(nextPid++) });
  },
  prepareCli: async () => {},
  launchCli: async () => {},
  // The recovery's readiness check: the helper dies here.
  verifyDaemon: async () => {
    process.kill(process.pid, 'SIGKILL');
    await new Promise(() => {});
  },
  removeControlSocket: () => {},
  sleep: async () => {},
  now: () => 0,
};

function write(text: string): void {
  fs.mkdirSync(path.dirname(markerFile), { recursive: true, mode: 0o700 });
  const temporary = `${markerFile}.${process.pid}.tmp`;
  fs.writeFileSync(temporary, text, { mode: 0o600 });
  fs.renameSync(temporary, markerFile);
}
const read = (): Record<string, unknown> | null => {
  try {
    return JSON.parse(fs.readFileSync(markerFile, 'utf8'));
  } catch {
    return null;
  }
};

let installed = '1.0.0';
void bridge.execute({
  now: () => 0,
  sleep: async () => {},
  operation: 'cli',
  readVersion: async () => installed,
  update: async () => {
    installed = '2.0.0';
  },
  marked: read,
  pending: (expected: string) => read()?.version === expected,
  mark: (closed: Record<string, unknown> = {}) =>
    write(JSON.stringify({ version: bridge.output.version, ...closed })),
  markerSnapshot: () => (fs.existsSync(markerFile) ? fs.readFileSync(markerFile, 'utf8') : null),
  restoreMarker: (text: string) => write(text),
  clear: () => fs.rmSync(markerFile, { force: true }),
  hasOldProxies: async () => false,
  runtime: (hooks: Partial<CodexActivationRuntimeDependencies> = {}) =>
    createCodexActivationRuntime(codexHome, {
      ...processes,
      ...hooks,
      launch: (target, desktop) =>
        bridge.relaunch(target, desktop, processes.launch, { inside: () => true }),
    }),
});
