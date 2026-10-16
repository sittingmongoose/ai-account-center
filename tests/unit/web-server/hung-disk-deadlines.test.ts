/**
 * A hard-mounted share that stops answering leaves file calls pending forever. These tests stand
 * in for it with calls that never settle, and with a FIFO that has no writer: opening it blocks
 * the calling thread in the kernel, which a worker's terminate() cannot interrupt either.
 */
import { afterEach, describe, expect, it, mock, spyOn } from 'bun:test';
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  AccountAnalyticsActivityService,
  runAnalyticsThread,
  type AccountAnalyticsActivityRequest,
} from '../../../src/web-server/services/account-analytics-activity';
import { AdditionalAccountService } from '../../../src/web-server/services/additional-account-service';
import { resolveAnalyticsRemoteHosts } from '../../../src/web-server/services/analytics-remote-transport';
import * as claudeProfiles from '../../../src/web-server/services/claude-desktop-profile-service';
import { parseAccountRegistry } from '../../../src/web-server/services/account-registry-v2';
import type { AccountRegistryRead } from '../../../src/web-server/services/account-registry-v2';
import { createUsageHubAccountSource } from '../../../src/web-server/usage-hub/usage-hub-accounts';
import type { AccountDashboard } from '../../../src/web-server/services/account-dashboard-types';
import {
  spawnLargeCodexReader,
  type UsageWorkerResult,
} from '../../../src/web-server/usage/worker-client';

const NOW = Date.parse('2026-10-16T18:00:00Z');
const never = <T>() => new Promise<T>(() => undefined);
const cleanups: Array<() => void> = [];
afterEach(() => {
  mock.restore();
  for (const cleanup of cleanups.splice(0)) cleanup();
});

/** A writer lets a stuck open of `fifo` return, as a share that answers again; never blocks. */
function answer(fifo: string): void {
  try {
    fs.closeSync(fs.openSync(fifo, fs.constants.O_WRONLY | fs.constants.O_NONBLOCK));
  } catch {
    /* No reader is waiting any more. */
  }
}

function usage(): UsageWorkerResult {
  return { daily: [], hourly: [], monthly: [], session: [], eventCount: 1 };
}

describe('analytics threads on a hung disk', () => {
  it.skipIf(process.platform === 'win32')(
    'settles a thread stuck in a disk call and reads its logs again only after it exits',
    async () => {
      const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-hung-thread-'));
      const fifo = path.join(dir, 'hung');
      execFileSync('mkfifo', [fifo]);
      let released = false;
      const release = () => {
        if (released) return;
        released = true;
        answer(fifo);
      };
      cleanups.push(() => {
        release();
        fs.rmSync(dir, { recursive: true, force: true });
      });
      const stuck = path.join(dir, 'stuck-worker.js');
      fs.writeFileSync(
        stuck,
        "const fs = require('fs');\n" +
          "const { workerData, parentPort } = require('worker_threads');\n" +
          'fs.openSync(workerData.fifo, "r");\n' +
          "parentPort.postMessage('late');\n"
      );
      const healthy = path.join(dir, 'healthy-worker.js');
      fs.writeFileSync(healthy, "require('worker_threads').parentPort.postMessage('answered');\n");

      const started = Date.now();
      await expect(runAnalyticsThread(stuck, { fifo }, 20, 'fixture-logs', 50)).rejects.toThrow(
        'did not stop'
      );
      expect(Date.now() - started).toBeLessThan(2_000);
      // While the stuck thread may still write, the same logs are not read again.
      await expect(runAnalyticsThread(healthy, {}, 1_000, 'fixture-logs')).rejects.toThrow(
        'has not stopped yet'
      );
      // Other logs are unaffected.
      await expect(runAnalyticsThread(healthy, {}, 1_000, 'other-logs')).resolves.toBe('answered');

      release();
      let reply: unknown;
      for (let attempt = 0; attempt < 100 && reply === undefined; attempt++) {
        reply = await runAnalyticsThread(healthy, {}, 1_000, 'fixture-logs').catch(() => {
          return new Promise((resolve) => setTimeout(() => resolve(undefined), 20));
        });
      }
      expect(reply).toBe('answered');
    },
    8_000
  );

  it.skipIf(process.platform === 'win32')(
    'settles a large Codex read stuck in a disk call without waiting for its thread',
    async () => {
      const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-hung-large-read-'));
      const fifo = path.join(dir, 'hung');
      execFileSync('mkfifo', [fifo]);
      cleanups.push(() => {
        answer(fifo);
        fs.rmSync(dir, { recursive: true, force: true });
      });
      const stuck = path.join(dir, 'stuck-worker.js');
      fs.writeFileSync(stuck, "require('fs').openSync(" + JSON.stringify(fifo) + ", 'r');\n");
      const spec = {
        file: fifo,
        checkpoint: path.join(dir, 'checkpoint'),
        revision: 'r',
        minDate: 0,
      };
      const started = Date.now();
      const result = await spawnLargeCodexReader(spec, new AbortController().signal, 20, stuck, 50);
      expect(result).toBeUndefined();
      expect(Date.now() - started).toBeLessThan(2_000);
    },
    8_000
  );

  it('reads the previous roots when discovery does not answer, without calling any tool missing', async () => {
    let hung = false;
    let pass = 0;
    const loaded: string[] = [];
    const claude: AccountAnalyticsActivityRequest = {
      provider: 'claude',
      request: { kind: 'claude', projectsDir: '/fixture/hung/projects' },
    };
    const omp: AccountAnalyticsActivityRequest = {
      provider: 'omp',
      request: { kind: 'omp', roots: ['/fixture/hung/omp'] },
    };
    const service = new AccountAnalyticsActivityService({
      now: () => NOW + pass * 1_000,
      scope: () => '/fixture-hung-discovery',
      persistSnapshot: false,
      remote: async () => ({ results: [], states: [] }),
      planThread: async (request) => {
        if (hung) return undefined;
        return request.part === 'ready'
          ? { ok: true, ready: { requests: [claude], scanned: [], experimentView: null } }
          : { ok: true, later: [omp] };
      },
      loadWorker: async (request) => {
        loaded.push(request.kind);
        return usage();
      },
    });
    const settled = async () => {
      for (let attempt = 0; attempt < 500; attempt++) {
        const answer = await service.get(
          { platform: 'mac', range: '24h', provider: 'all', account: 'all' },
          NOW - 86_400_000,
          NOW
        );
        if (answer.refreshing !== true) return answer;
        await new Promise((resolve) => setTimeout(resolve, 5));
      }
      throw new Error('background collection did not settle');
    };

    await settled();
    expect(loaded.sort()).toEqual(['claude', 'omp']);

    hung = true;
    loaded.length = 0;
    pass++;
    await service.get(
      { platform: 'mac', range: '24h', provider: 'all', account: 'all', refresh: true },
      NOW - 86_400_000,
      NOW
    );
    const answer = await settled();
    expect(loaded.sort()).toEqual(['claude', 'omp']);
    const ubuntu = answer.sources.filter((entry) => entry.host === 'ubuntu');
    expect(ubuntu.some((entry) => entry.state === 'not_installed')).toBe(false);
    await service.shutdown();
  });
});

describe('small private files on a hung disk', () => {
  it('keeps the last account list in force while its read does not answer', async () => {
    const parsed = parseAccountRegistry({
      version: 2,
      accounts: [
        {
          id: 'zai:usage',
          provider: 'zai',
          platform: 'ubuntu',
          sshHost: null,
          label: null,
          credential: { kind: 'discover' },
        },
      ],
    });
    if (!parsed) throw new TypeError('fixture registry is invalid');
    let hung = false;
    const service = new AdditionalAccountService({
      ccsDir: '/tmp/aac-hung-account-list-fixture',
      now: () => NOW,
      manifestDeadlineMs: 20,
      readRegistry: (): Promise<AccountRegistryRead> =>
        hung ? never() : Promise.resolve({ state: 'ok', registry: parsed }),
      runSource: async () =>
        JSON.stringify({
          status: 'ok',
          email: null,
          plan: 'Pro',
          fetchedAt: new Date(NOW).toISOString(),
          sampledAt: new Date(NOW).toISOString(),
          windows: [{ key: 'weekly', label: 'Weekly', usedPercent: 12, windowMinutes: 10080 }],
        }),
    });
    const first = await service.snapshot();
    expect(first.registry).toBe('v2');
    expect(first.accounts.map((account) => account.id)).toEqual(['zai:usage']);

    hung = true;
    const kept = await service.snapshot({ refresh: true });
    expect(kept.registry).toBe('v2');
    expect(kept.accounts.map((account) => [account.id, account.status])).toEqual([
      ['zai:usage', 'ok'],
    ]);
  });

  it('keeps the last remote host aliases while the launcher list read does not answer', async () => {
    const originalHome = process.env.CCS_HOME;
    const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-hung-launchers-'));
    process.env.CCS_HOME = home;
    cleanups.push(() => {
      if (originalHome === undefined) delete process.env.CCS_HOME;
      else process.env.CCS_HOME = originalHome;
      fs.rmSync(home, { recursive: true, force: true });
    });
    fs.mkdirSync(path.join(home, '.ccs'), { recursive: true });
    fs.writeFileSync(
      path.join(home, '.ccs', 'claude-desktop-profiles.json'),
      JSON.stringify({
        version: 1,
        profiles: [
          {
            id: 'one',
            email: 'one@example.test',
            mac: { launcherName: 'One', sshHost: 'mac-fixture' },
            windows: { launcherName: 'One', sshHost: 'win-fixture' },
          },
        ],
      })
    );
    const first = await resolveAnalyticsRemoteHosts(1_000);
    expect(first).toMatchObject({ mac: 'mac-fixture', windows: 'win-fixture' });

    // The backup reader and remote Analytics are not held by the read, nor lose their hosts.
    spyOn(claudeProfiles, 'listClaudeDesktopProfiles').mockImplementation(never);
    const started = Date.now();
    expect(await resolveAnalyticsRemoteHosts(20)).toEqual(first);
    expect(Date.now() - started).toBeLessThan(1_000);
  });

  it('gives T3 the last account list when neither dashboard read answers', async () => {
    let clock = NOW;
    let hung = false;
    const dashboard = { accounts: [] } as unknown as AccountDashboard;
    const reads: string[] = [];
    const source = createUsageHubAccountSource({
      now: () => clock,
      peekDeadlineMs: 20,
      peek: async (platform) => {
        reads.push(platform);
        return hung ? never() : dashboard;
      },
    });
    const first = await source.read();
    hung = true;
    clock += 60_000;
    const kept = await source.read();
    expect(kept).toBe(first);
    expect(reads).toEqual(['mac', 'windows', 'mac', 'windows']);
  });
});
