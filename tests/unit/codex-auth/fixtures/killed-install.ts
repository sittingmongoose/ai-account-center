/**
 * A Codex CLI update whose helper is killed while the updater runs: after it has put 2.0.0 in place,
 * before the bridge writes anything else. Run as its own process with a temporary HOME: the real
 * bridge and the real runtime, synthetic processes only, with a daemon still running the 1.0.0
 * binary. Whatever is on disk then is what the next click finds. The marker is read and written here
 * exactly as the bridge's own helpers do.
 */
import * as fs from 'fs';
import * as path from 'path';
import {
  createCodexActivationRuntime,
  type CodexActivationRuntimeDependencies,
} from '../../../../src/codex-auth/codex-activation-runtime';

const bridge = require('../../../../scripts/app-updates/app_update_codex.cjs');
const home = process.env.HOME as string;
const codexHome = path.join(home, '.codex');
const markerFile = path.join(home, '.ccs/app-updates/codex-cli-pending-restart.json');

const processes: CodexActivationRuntimeDependencies = {
  platform: 'linux',
  scan: async () => [
    {
      pid: 10,
      ppid: 1,
      startTime: '10',
      state: 'S',
      args: ['/fixture/releases/1.0.0/codex', 'app-server', '--listen', 'unix://'],
      exe: '/fixture/releases/1.0.0/codex',
      cwd: '/fixture',
      env: { HOME: home, CODEX_HOME: codexHome },
    },
  ],
  acquireStartupLock: async () => async () => {},
  acquireNativeStartupLock: async () => {},
  releaseNativeStartupLock: async () => {},
  assertIdle: async () => {},
  signal: async () => {},
  launch: async () => {},
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

let installed = '1.0.0';
void bridge.execute({
  now: () => 0,
  sleep: async () => {},
  operation: 'cli',
  readVersion: async () => installed,
  // The updater has replaced the binary when the helper dies.
  update: async () => {
    installed = '2.0.0';
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
