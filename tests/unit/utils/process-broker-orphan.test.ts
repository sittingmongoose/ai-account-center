import { describe, expect, it } from 'bun:test';
import { spawn } from 'child_process';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { withDeadline } from '../../../src/utils/settle-deadline';
import { waitForProcessState, waitUntil } from '../../shared/fixtures/process-state';

const brokerScript = path.resolve(import.meta.dir, '../../../scripts/process-broker.cjs');

describe.skipIf(process.platform !== 'linux')('broker owner death', () => {
  it.each(['disconnect', 'watchdog'] as const)(
    'kills its command group and exits after parent SIGKILL via %s',
    async (mode) => {
      const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-broker-orphan-'));
      const brokerPidFile = path.join(root, 'broker.pid');
      const commandPidFile = path.join(root, 'command-pids.json');
      const wrapper = path.join(root, 'broker.cjs');
      // Exercise the independent watchdog even if the IPC notification never runs.
      fs.writeFileSync(
        wrapper,
        `require(${JSON.stringify(brokerScript)});\n` +
          (mode === 'watchdog' ? `process.removeAllListeners('disconnect');\n` : '')
      );
      const source = [
        'import os,signal,time,json',
        'signal.signal(signal.SIGTERM,signal.SIG_IGN)',
        'child=os.fork()',
        `if child: open(${JSON.stringify(commandPidFile)},'w').write(json.dumps([os.getpid(),child]))`,
        'time.sleep(60)',
      ].join('\n');
      const fixture = path.join(root, 'parent.cjs');
      fs.writeFileSync(
        fixture,
        `
const fs = require('fs');
const { fork } = require('child_process');
const broker = fork(${JSON.stringify(wrapper)}, [String(process.pid)], {
  stdio: ['ignore', 'ignore', 'ignore', 'ipc'],
  env: { PATH: '/usr/bin:/bin' },
});
fs.writeFileSync(${JSON.stringify(brokerPidFile)}, String(broker.pid));
broker.on('message', packet => {
  if (packet.type !== 'ready') return;
  broker.send({type: 'run', id: 1, file: '/usr/bin/python3', args: ['-I','-c',${JSON.stringify(source)}],
    deadlineAt: Date.now() + 60000, hardDeadlineAt: Date.now() + 60000,
    options: {input: '', timeoutMs: 60000, graceMs: 1000, maxBytes: 65536, env: {PATH: '/usr/bin:/bin'}}});
});
setInterval(() => {}, 1000);
`
      );
      const parent = spawn(process.execPath, [fixture], {
        env: { PATH: process.env.PATH, CCS_HOME: root, NO_COLOR: '1' },
        stdio: ['ignore', 'ignore', 'pipe'],
      });
      const exited = new Promise<void>((resolve, reject) => {
        parent.once('error', reject);
        parent.once('exit', () => resolve());
      });
      let brokerPid: number | undefined;
      let commandPids: number[] = [];
      try {
        await waitUntil(
          () => {
            if (!fs.existsSync(brokerPidFile) || !fs.existsSync(commandPidFile)) return false;
            brokerPid = Number(fs.readFileSync(brokerPidFile, 'utf8'));
            try {
              commandPids = JSON.parse(fs.readFileSync(commandPidFile, 'utf8'));
              return commandPids.length === 2;
            } catch {
              return false;
            }
          },
          'Real broker and command group startup',
          5000
        );
        for (const pid of [brokerPid!, ...commandPids])
          expect(() => process.kill(pid, 0)).not.toThrow();
        expect(parent.kill('SIGKILL')).toBe(true);
        await withDeadline(exited, 2000);
        // Poll actual disappearance, not just signal delivery or a zombie state.
        await waitForProcessState(brokerPid!, [null]);
        for (const pid of commandPids) await waitForProcessState(pid, [null]);
      } finally {
        parent.kill('SIGKILL');
        await withDeadline(exited, 2000);
        // Only fixture-owned identities may be signalled during failure cleanup.
        for (const [pid, marker, group] of [
          [brokerPid, wrapper, false],
          [commandPids[0], commandPidFile, true],
        ] as const) {
          if (!pid) continue;
          try {
            if (fs.readFileSync(`/proc/${pid}/cmdline`, 'utf8').includes(marker))
              process.kill(group ? -pid : pid, 'SIGKILL');
          } catch (error) {
            if (!['ENOENT', 'ESRCH'].includes((error as NodeJS.ErrnoException).code ?? ''))
              throw error;
          }
        }
        fs.rmSync(root, { recursive: true, force: true });
      }
    },
    15000
  );
});
