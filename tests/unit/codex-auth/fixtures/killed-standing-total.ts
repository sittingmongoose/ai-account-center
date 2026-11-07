/**
 * A Codex Desktop update over two desktops and a daemon. stop() retires the first desktop, fails on
 * the second and recovers; the first desktop's relaunch fails. The bridge's readiness check on the daemon then SIGKILLs this process, as a
 * parent timeout would. Run as its own process with a temporary HOME: the real bridge and runtime,
 * synthetic processes only, kept in a file so the next click sees what the killed one left.
 *
 *   bun run killed-standing-total.ts kill    the killed update
 *   bun run killed-standing-total.ts retry   the next update, printing its result as JSON
 */
import * as fs from 'fs';
import * as path from 'path';
import {
  CodexActivationRuntimeError,
  createCodexActivationRuntime,
  type CodexActivationRuntimeDependencies,
  type CodexProcessSnapshot,
} from '../../../../src/codex-auth/codex-activation-runtime';

const bridge = require('../../../../scripts/app-updates/app_update_codex.cjs');
const mode = process.argv[2];
const home = process.env.HOME as string;
const codexHome = path.join(home, '.codex');
const state = path.join(home, 'fixture-state');
const markerFile = path.join(home, '.ccs/app-updates/codex-desktop-pending-restart.json');
const EXE = '/usr/lib/chatgpt/ChatGPT';

const main = (pid: number, flag: string): CodexProcessSnapshot => ({
  pid,
  ppid: 1,
  startTime: String(pid),
  state: 'S',
  exe: EXE,
  args: [EXE, `--user-data-dir=/fixture/Profile${flag}`],
  cwd: '/fixture',
  env: { HOME: home, DISPLAY: ':8' },
});
const server = (ppid: number): CodexProcessSnapshot => ({
  pid: ppid + 1,
  ppid,
  startTime: String(ppid + 1),
  state: 'S',
  exe: '/usr/lib/chatgpt/resources/codex',
  args: ['/usr/lib/chatgpt/resources/codex', 'app-server'],
  cwd: '/fixture',
  env: { HOME: home, CODEX_HOME: codexHome, DISPLAY: ':8' },
});
const daemon: CodexProcessSnapshot = {
  pid: 30,
  ppid: 1,
  startTime: '30',
  state: 'S',
  exe: '/fixture/bin/codex',
  args: ['/fixture/bin/codex', 'app-server', '--listen', 'unix://'],
  cwd: '/fixture',
  env: { HOME: home, CODEX_HOME: codexHome },
};

const load = (name: string): any => JSON.parse(fs.readFileSync(path.join(state, name), 'utf8'));
const save = (name: string, value: unknown): void =>
  fs.writeFileSync(path.join(state, name), JSON.stringify(value));
const processes = (): CodexProcessSnapshot[] => load('processes.json');

if (mode === 'kill') {
  fs.mkdirSync(state, { recursive: true });
  save('processes.json', [main(10, '--one'), server(10), main(20, '--two'), server(20), daemon]);
  save('installed.json', '1.0.0');
  save('next-pid.json', 100);
}

// A fake clock that moves only when something waits, so a bounded wait ends.
let clock = 0;
let refused = false;
let checks = 0;
const dependencies: CodexActivationRuntimeDependencies = {
  platform: 'linux',
  scan: async () => processes(),
  acquireStartupLock: async () => async () => {},
  acquireNativeStartupLock: async () => {},
  releaseNativeStartupLock: async () => {},
  assertIdle: async () => {},
  prepareCli: async () => {},
  launchCli: async () => {},
  removeControlSocket: () => {},
  sleep: async (ms) => {
    clock += ms;
  },
  now: () => clock,
  signal: async (target) => {
    // The twin refuses its first signal, so stop() fails partway and recovers.
    if (mode === 'kill' && target.pid === 20 && !refused) {
      refused = true;
      throw new CodexActivationRuntimeError('fixture signal failed');
    }
    save(
      'processes.json',
      processes().filter((entry) => entry.pid !== target.pid && entry.ppid !== target.pid)
    );
  },
  launch: async (target, desktop) => {
    // The closed desktop's relaunch fails in the killed run.
    if (mode === 'kill' && desktop && target.pid === 10) {
      throw new CodexActivationRuntimeError('fixture launch failed');
    }
    const pid = load('next-pid.json');
    save('next-pid.json', pid + 10);
    const fresh = { ...target, pid, ppid: 1, startTime: `fresh-${pid}` };
    save('processes.json', [...processes(), fresh, ...(desktop ? [server(pid)] : [])]);
    return { pid };
  },
  verifyDaemon: async () => {
    // The first check is stop()'s recovery; the second is the bridge's own, after a failed stop().
    if (mode === 'kill' && ++checks === 2) {
      process.kill(process.pid, 'SIGKILL');
      await new Promise(() => {});
    }
  },
};

const read = (): Record<string, unknown> | null => {
  try {
    return JSON.parse(fs.readFileSync(markerFile, 'utf8'));
  } catch {
    return null;
  }
};

void (async () => {
  const result = await bridge.execute({
    operation: 'desktop',
    expectedCandidate: '2.0.0',
    now: () => clock,
    sleep: async (ms: number) => {
      clock += ms;
    },
    readVersion: async () => load('installed.json'),
    update: async () => save('installed.json', '2.0.0'),
    hasOldProxies: async () => false,
    marked: read,
    pending: (expected: string) => read()?.version === expected,
    markerSnapshot: () => bridge.markerSnapshot(markerFile),
    restoreMarker: (text: string) => bridge.restoreMarker(text, markerFile),
    clear: () => fs.rmSync(markerFile, { force: true }),
    mark: (fields: Record<string, unknown> = {}) =>
      bridge.restoreMarker(
        JSON.stringify({ version: bridge.output.version, ...fields }),
        markerFile
      ),
    runtime: (hooks: Partial<CodexActivationRuntimeDependencies> = {}) =>
      createCodexActivationRuntime(codexHome, {
        ...dependencies,
        ...hooks,
        launch: (target, desktop) =>
          bridge.relaunch(target, desktop, dependencies.launch, { inside: () => true }),
      }),
  });
  const desktops = processes().filter((entry) => entry.exe === EXE && entry.ppid === 1).length;
  console.log(JSON.stringify({ status: result.status, messageCode: result.messageCode, desktops }));
})();
