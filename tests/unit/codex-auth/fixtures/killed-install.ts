/**
 * A Codex CLI update whose helper is killed while the updater runs: after it has put 2.0.0 in place,
 * before the bridge writes anything else. Run as its own process with a temporary HOME: the real
 * bridge and the real runtime, synthetic processes only, with a daemon still running the 1.0.0
 * binary. Whatever is on disk then is what the next click finds. The marker is read and written here
 * exactly as the bridge's own helpers do.
 *
 * AAC_FIXTURE_ARRIVAL=1: no daemon runs at first, only a proxy that can start one, and a 1.0.0 daemon
 * arrives while the updater runs. AAC_FIXTURE_NO_KILL=1 lets that update finish instead. With the
 * argument `retry`, the next click: 2.0.0 installed, the proxy and the 1.0.0 daemon running, and the
 * result printed as JSON with how many signals it sent.
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

const arrival = process.env.AAC_FIXTURE_ARRIVAL === '1';
const retry = process.argv[2] === 'retry';
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
let current: CodexProcessSnapshot[] = retry ? [proxy, daemon] : arrival ? [proxy] : [daemon];
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

let installed = retry ? '2.0.0' : '1.0.0';
void (async () => {
  const result = await bridge.execute({
    now: () => 0,
    sleep: async () => {},
    operation: 'cli',
    readVersion: async () => installed,
    // The updater has replaced the binary when the helper dies; with an arrival, the proxy has
    // started a daemon on the old binary first.
    update: async () => {
      if (retry) return;
      if (arrival) current.push(daemon);
      installed = '2.0.0';
      if (process.env.AAC_FIXTURE_NO_KILL === '1') return;
      process.kill(process.pid, 'SIGKILL');
      await new Promise(() => {});
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
      createCodexActivationRuntime(codexHome, { ...processes, ...hooks }),
  });
  console.log(
    JSON.stringify({
      status: result.status,
      messageCode: result.messageCode,
      signals,
      marker: read(),
    })
  );
})();
