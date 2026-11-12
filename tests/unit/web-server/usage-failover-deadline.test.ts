import { afterEach, describe, expect, it, spyOn } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  localUsageHost,
  runUsageHelper,
  usageHelperDeadlineMs,
} from '../../../src/web-server/services/usage-failover-transport';
import type { UsageHelperRequest } from '../../../src/web-server/services/usage-failover-contract';
import { stopProcessBroker } from '../../../src/utils/managed-process';
import { waitForProcessState } from '../../shared/fixtures/process-state';

const roots: string[] = [];
const delay = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
afterEach(async () => {
  await stopProcessBroker();
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

function running(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return (
      process.platform !== 'linux' ||
      !fs.readFileSync(`/proc/${pid}/stat`, 'utf8').split(') ')[1].startsWith('Z ')
    );
  } catch {
    return false;
  }
}

describe.skipIf(process.platform === 'win32')('usage helper end-to-end deadlines', () => {
  it('never launches after source loading outlives a shorter caller budget', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-usage-source-budget-'));
    roots.push(root);
    const ccsDir = path.join(root, 'ccs');
    const interpreter = path.join(ccsDir, 'claude-session-migration/venv/bin/python3');
    const marker = path.join(root, 'started');
    fs.mkdirSync(path.dirname(interpreter), { recursive: true });
    fs.writeFileSync(
      interpreter,
      `#!/usr/bin/python3\nopen(${JSON.stringify(marker)},'w').write('late')\n`,
      { mode: 0o700 }
    );
    const original = fs.promises.readFile.bind(fs.promises);
    let delayed = false;
    const read = spyOn(fs.promises, 'readFile').mockImplementation(async (...args) => {
      if (!delayed && String(args[0]).endsWith('/account-usage/failover_usage.py')) {
        delayed = true;
        await delay(150);
      }
      return original(...args);
    });
    const request: UsageHelperRequest = {
      version: 1,
      action: 'inventory',
      provider: 'codex',
      homeDir: root,
      ccsDir,
    };
    try {
      await expect(runUsageHelper(localUsageHost(), request, Date.now() - 1)).rejects.toThrow(
        'Usage source unavailable.'
      );
      expect(delayed).toBe(false);
      await expect(runUsageHelper(localUsageHost(), request, Date.now() + 20)).rejects.toThrow(
        'Usage source unavailable.'
      );
      expect(delayed).toBe(true);
      await delay(400);
      expect(fs.existsSync(marker)).toBe(false);
    } finally {
      read.mockRestore();
    }
  });

  for (const action of ['inventory', 'collect'] as const) {
    it(`kills a real ${action} helper at the outer deadline after slow source loading`, async () => {
      const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-usage-deadline-'));
      roots.push(root);
      const ccsDir = path.join(root, 'ccs');
      const interpreter = path.join(ccsDir, 'claude-session-migration/venv/bin/python3');
      const marker = path.join(root, 'helper.pid');
      const heartbeat = path.join(root, 'heartbeat');
      fs.mkdirSync(path.dirname(interpreter), { recursive: true });
      fs.writeFileSync(
        interpreter,
        [
          '#!/usr/bin/python3',
          'import sys,os,signal,time',
          'sys.stdin.read()',
          'signal.signal(signal.SIGTERM,signal.SIG_IGN)',
          `open(${JSON.stringify(marker)},'w').write(str(os.getpid()))`,
          'while True:',
          ` open(${JSON.stringify(heartbeat)},'w').write(str(time.monotonic()))`,
          ' time.sleep(.02)',
        ].join('\n'),
        { mode: 0o700 }
      );
      const context = { version: 1 as const, provider: 'codex' as const, homeDir: root, ccsDir };
      const request: UsageHelperRequest =
        action === 'inventory'
          ? { ...context, action }
          : {
              ...context,
              action,
              candidate: {
                locator: '1'.repeat(64),
                selectors: ['discover'],
                revision: '2'.repeat(64),
                identity: { kind: 'fixture', key: '3'.repeat(64) },
                email: null,
                plan: null,
              },
            };
      const original = fs.promises.readFile.bind(fs.promises);
      let delayed = false;
      const read = spyOn(fs.promises, 'readFile').mockImplementation(async (...args) => {
        if (!delayed && String(args[0]).endsWith('/account-usage/failover_usage.py')) {
          delayed = true;
          // Both review witnesses: original source bytes arrive two/three seconds late.
          await delay(action === 'inventory' ? 3000 : 2000);
        }
        return original(...args);
      });
      let pid: number | undefined;
      try {
        const began = performance.now();
        await expect(runUsageHelper(localUsageHost(), request)).rejects.toThrow(
          'Usage source unavailable.'
        );
        expect(delayed).toBe(true);
        expect(performance.now() - began).toBeLessThan(usageHelperDeadlineMs(action) + 1000);
        pid = Number(fs.readFileSync(marker, 'utf8'));
        expect(Number.isSafeInteger(pid) && pid > 0).toBe(true);
        // Keep the original 250 ms termination allowance; polling must not hide overruns.
        await waitForProcessState(pid, [null, 'Z', 'X'], 250);
        const beat = fs.readFileSync(heartbeat, 'utf8');
        await delay(250);
        expect(fs.readFileSync(heartbeat, 'utf8')).toBe(beat);
      } finally {
        read.mockRestore();
        // Only the process recorded by this synthetic private interpreter is signalled.
        if (pid === undefined && fs.existsSync(marker))
          pid = Number(fs.readFileSync(marker, 'utf8'));
        if (pid !== undefined && running(pid)) process.kill(pid, 'SIGKILL');
      }
    }, 45_000);
  }
});
