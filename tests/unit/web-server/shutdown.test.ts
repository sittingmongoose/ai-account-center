import { describe, expect, it } from 'bun:test';
import { spawn } from 'child_process';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { withDeadline } from '../../../src/utils/settle-deadline';

const repo = path.resolve(import.meta.dir, '../../..');
const brokerScript = path.join(repo, 'scripts/process-broker.cjs');
const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

function processState(pid: number): string | null {
  try {
    return fs.readFileSync(`/proc/${pid}/stat`, 'utf8').split(') ')[1].split(' ')[0];
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw error;
  }
}

async function waitUntilGone(pid: number): Promise<void> {
  for (let attempt = 0; attempt < 100 && processState(pid) !== null; attempt++) await pause(10);
  expect(processState(pid)).toBeNull();
}

async function signalFixture(signal: 'SIGINT' | 'SIGTERM', hungStore = false): Promise<void> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-signal-shutdown-'));
  const fixture = path.join(root, 'fixture.ts');
  const events = path.join(root, 'events');
  const brokerPidFile = path.join(root, 'broker.pid');
  const commandPidFile = path.join(root, 'command.pid');
  fs.writeFileSync(
    fixture,
    `
import { mock } from 'bun:test';
import fs from 'fs';
import path from 'path';
const root = process.argv[2];
const event = (name) => fs.appendFileSync(path.join(root, 'events'), name + '\\n');
mock.module(${JSON.stringify(path.join(repo, 'src/web-server/middleware/auth-middleware.ts'))}, () => ({
  closeDashboardSessions: async () => { event('sessions'); },
}));
mock.module(${JSON.stringify(path.join(repo, 'src/store/store-service.ts'))}, () => ({
  closeAacStore: async () => {
    event('store');
    if (process.argv[3] === 'hung') await new Promise(() => {});
  },
}));
const { startProcessBroker, runManagedCommand, processBrokerStats, stopProcessBroker } =
  await import(${JSON.stringify(path.join(repo, 'src/utils/managed-process.ts'))});
const { setupGracefulShutdown } = await import(${JSON.stringify(path.join(repo, 'src/web-server/shutdown.ts'))});
const broker = await startProcessBroker();
fs.writeFileSync(path.join(root, 'broker.pid'), String(broker.pid));
const marker = path.join(root, 'command.pid');
const source = 'import os,time;open(' + JSON.stringify(marker) + ',"w").write(str(os.getpid()));time.sleep(30)';
const work = runManagedCommand('/usr/bin/python3', ['-I', '-c', source], {
  timeoutMs: 5000, maxBytes: 65536, env: { PATH: '/usr/bin:/bin' },
});
for (let attempt = 0; attempt < 200 && !fs.existsSync(marker); attempt++)
  await new Promise(resolve => setTimeout(resolve, 10));
if (!fs.existsSync(marker)) throw new Error('Fixture command did not start');
broker.kill('SIGSTOP');
broker.disconnect();
if (!(await work).failed || processBrokerStats().retired !== 1)
  throw new Error('Fixture broker was not retired');
setupGracefulShutdown({ close() { event('server'); } }, {}, () => {
  event('cleanup');
  // The real server starts this same idempotent teardown without awaiting it.
  void stopProcessBroker();
});
setInterval(() => {}, 250);
console.log('SIGNAL_READY');
`
  );
  const child = spawn(process.execPath, [fixture, root, hungStore ? 'hung' : 'ready'], {
    env: {
      ...process.env,
      CCS_HOME: root,
      CCS_DIR: root,
      CODEX_HOME: path.join(root, 'codex'),
      CLAUDE_CONFIG_DIR: path.join(root, 'claude'),
      AAC_AGENT_REPORTS_DIR: path.join(root, 'reports'),
      NO_COLOR: '1',
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '';
  let stderr = '';
  const closed = new Promise<number | null>((resolve, reject) => {
    child.once('error', reject);
    child.once('close', resolve);
  });
  void closed.catch(() => {});
  child.stderr.on('data', (chunk) => {
    stderr += chunk.toString();
  });
  const ready = new Promise<void>((resolve, reject) => {
    child.stdout.on('data', (chunk) => {
      stdout += chunk.toString();
      if (stdout.includes('SIGNAL_READY')) resolve();
    });
    child.once('exit', () => reject(new Error(`Signal fixture exited before ready: ${stderr}`)));
  });
  try {
    await withDeadline(ready, 5000);
    const brokerPid = Number(fs.readFileSync(brokerPidFile, 'utf8'));
    expect(processState(brokerPid)).toBe('T');
    const sentAt = Date.now();
    expect(child.kill(signal)).toBe(true);
    await pause(10);
    // A second signal must share the ongoing shutdown, not exit early or run cleanup twice.
    if (child.exitCode === null && child.signalCode === null)
      child.kill(signal === 'SIGINT' ? 'SIGTERM' : 'SIGINT');
    expect(await withDeadline(closed, 5000)).toBe(0);
    const elapsed = Date.now() - sentAt;
    await waitUntilGone(brokerPid);
    const recorded = fs.readFileSync(events, 'utf8').trim().split('\n');
    for (const event of ['cleanup', 'server', 'sessions', 'store'])
      expect(recorded.filter((name) => name === event)).toHaveLength(1);
    if (hungStore) {
      expect(elapsed).toBeGreaterThanOrEqual(2500);
      expect(elapsed).toBeLessThan(5000);
    }
  } finally {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
    await withDeadline(closed, 1000).catch(() => {});
    // Failure cleanup only signals PIDs recorded by this fixture, after checking their argv.
    for (const [pidFile, ownedArg, group] of [
      [brokerPidFile, brokerScript, false],
      [commandPidFile, root, true],
    ] as const) {
      if (!fs.existsSync(pidFile)) continue;
      const pid = Number(fs.readFileSync(pidFile, 'utf8'));
      try {
        if (fs.readFileSync(`/proc/${pid}/cmdline`, 'utf8').includes(ownedArg))
          process.kill(group ? -pid : pid, 'SIGKILL');
      } catch (error) {
        if (!['ENOENT', 'ESRCH'].includes((error as NodeJS.ErrnoException).code ?? '')) throw error;
      }
    }
    fs.rmSync(root, { recursive: true, force: true });
  }
}

describe.skipIf(process.platform !== 'linux')('real dashboard signal shutdown', () => {
  it.each(['SIGINT', 'SIGTERM'] as const)(
    'awaits retired broker exit before %s ends the parent',
    async (signal) => {
      await signalFixture(signal);
    },
    10000
  );
  it('keeps the existing three-second deadline when another closer never settles', async () => {
    await signalFixture('SIGTERM', true);
  }, 10000);
});
