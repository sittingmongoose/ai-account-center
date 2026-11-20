import { SSH_ALIASES } from '../../../src/web-server/services/dashboard-hosts';
/**
 * A hard-mounted share that stops answering leaves file calls pending forever. These tests stand
 * in for it with calls that never settle, and with a FIFO that has no writer: opening it blocks
 * the calling thread in the kernel, which a worker's terminate() cannot interrupt either.
 */
import { afterEach,describe,expect,it,mock,spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { AdditionalAccountService } from '../../../src/web-server/services/additional-account-service';
import { resolveAnalyticsRemoteHosts } from '../../../src/web-server/services/analytics-remote-transport';
import * as claudeProfiles from '../../../src/web-server/services/claude-desktop-profile-service';
import { parseAccountRegistry } from '../../../src/web-server/services/account-registry-v2';
import type { AccountRegistryRead } from '../../../src/web-server/services/account-registry-v2';
import { createUsageHubAccountSource } from '../../../src/web-server/usage-hub/usage-hub-accounts';
import type { AccountDashboard } from '../../../src/web-server/services/account-dashboard-types';
import {
type UsageWorkerResult
} from '../../../src/web-server/usage/worker-client';

const NOW = Date.parse('2026-10-16T18:00:00Z');
const never = <T>() => new Promise<T>(() => undefined);
const cleanups: Array<() => void | Promise<void>> = [];
afterEach(async () => {
  mock.restore();
  for (const cleanup of cleanups.splice(0)) await cleanup();
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

  it('uses fixed computer aliases even when the desktop launcher list cannot answer', async () => {
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
    expect(first).toEqual(SSH_ALIASES);

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
