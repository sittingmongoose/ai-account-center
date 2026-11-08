/**
 * A Codex CLI update during which a daemon arrives: at first only a proxy runs, and it starts a 1.0.0
 * daemon while the updater puts 2.0.0 in place. Run as its own process with a temporary HOME: the real
 * bridge and the real runtime, synthetic processes only. Prints the result as JSON, with how many
 * signals it sent and the marker it left.
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

const codex = (pid: number, args: string[], release = '1.0.0'): CodexProcessSnapshot => ({
  pid,
  ppid: 1,
  startTime: String(pid),
  state: 'S',
  args: [`/fixture/releases/${release}/codex`, ...args],
  exe: `/fixture/releases/${release}/codex`,
  cwd: '/fixture',
  env: { HOME: home, CODEX_HOME: codexHome },
});
const proxy = codex(20, ['app-server', 'proxy']);
const daemon = codex(10, ['app-server', '--listen', 'unix://']);
let current: CodexProcessSnapshot[] = [proxy];
let signals = 0;

const processes: CodexActivationRuntimeDependencies = {
  platform: 'linux',
  scan: async () => [...current],
  acquireStartupLock: async () => async () => {},
  acquireNativeStartupLock: async () => {},
  releaseNativeStartupLock: async () => {},
  assertIdle: async () => {},
  signal: async (target) => {
    signals++;
    current = current.filter((entry) => entry.pid !== target.pid);
  },
  launch: async () => {
    const started = codex(100, ['app-server', '--listen', 'unix://'], '2.0.0');
    current.push(started);
    return { pid: started.pid, startTime: started.startTime };
  },
  prepareCli: async () => {},
  launchCli: async () => {},
  verifyDaemon: async () => {},
  removeControlSocket: () => {},
  sleep: async () => {},
  now: () => 0,
};

const read = (): Record<string, unknown> | null => {
  try {
    return JSON.parse(fs.readFileSync(markerFile, 'utf8'));
  } catch {
    return null;
  }
};

let installed = '1.0.0';
void (async () => {
  const result = await bridge.execute({
    now: () => 0,
    sleep: async () => {},
    operation: 'cli',
    readVersion: async () => installed,
    update: async () => {
      // The proxy starts a daemon on the old binary while the updater runs.
      current.push(daemon);
      installed = '2.0.0';
    },
    marked: read,
    pending: () => false,
    mark: () => {
      throw new Error('a CLI update keeps no marker');
    },
    clear: () => fs.rmSync(markerFile, { force: true }),
    hasOldProxies: async () => false,
    runtime: (hooks: Partial<CodexActivationRuntimeDependencies> = {}) =>
      createCodexActivationRuntime(codexHome, { ...processes, ...hooks }),
  });
  console.log(JSON.stringify({ status: result.status, signals, marker: read() }));
})();
