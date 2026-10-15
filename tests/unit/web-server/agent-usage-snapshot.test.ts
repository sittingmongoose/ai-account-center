import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import type {
  DashboardAccount,
  DashboardAccountWindow,
} from '../../../src/web-server/services/account-dashboard-types';
import {
  accountAnalyticsIdentity,
  type AccountAnalyticsHistoryData,
  type AccountAnalyticsObservation,
} from '../../../src/web-server/services/account-analytics-history';
import { projectAgentUsage } from '../../../src/web-server/agent-usage/agent-usage-projection';
import {
  burnRate,
  RecentReadings,
  usageHistory,
  type Reading,
} from '../../../src/web-server/agent-usage/agent-usage-history';
import {
  buildSnapshotFiles,
  createSnapshotPublisher,
  readSnapshotSettings,
  renderSnapshotMarkdown,
  SNAPSHOT_HISTORY,
  SNAPSHOT_JSON,
  SNAPSHOT_MARKDOWN,
  writeSnapshotSettings,
} from '../../../src/web-server/agent-usage/agent-usage-snapshot';
import { handleUsageSnapshotCommand } from '../../../src/commands/usage-snapshot-command';
import {
  CLIENT_FILES,
  clientSourceFolder,
} from '../../../src/web-server/agent-usage/agent-usage-snapshot';

const SENTINEL = 'sentinel-secret-value-7d1e';
const NOW = Date.parse('2026-10-15T12:00:00.000Z');
const MINUTE = 60_000;

function window(overrides: Partial<DashboardAccountWindow> = {}): DashboardAccountWindow {
  return {
    key: 'five_hour',
    label: '5-hour',
    usedPercent: 40,
    remainingPercent: 60,
    resetAt: '2026-10-15T14:00:00.000Z',
    windowMinutes: 300,
    used: null,
    limit: null,
    unit: null,
    ...overrides,
  };
}

function account(overrides: Partial<DashboardAccount> = {}): DashboardAccount {
  return {
    id: 'codex:gmail',
    provider: 'codex',
    providerLabel: 'Codex',
    label: 'Gmail',
    email: 'person@example.com',
    plan: 'Pro',
    platform: 'ubuntu',
    source: 'Codex usage on Ubuntu',
    status: 'ok',
    message: SENTINEL,
    fetchedAt: '2026-10-15T12:00:00.000Z',
    sampledAt: '2026-10-15T12:00:00.000Z',
    isActive: true,
    windows: [window()],
    capabilities: { codexProfile: 'gmail', claudeProfileId: null, claudePlatforms: [] },
    ...overrides,
  };
}

function record(
  row: DashboardAccount,
  minutesAgo: number,
  used: number,
  resetAt = '2026-10-15T14:00:00.000Z'
): AccountAnalyticsObservation {
  const at = new Date(NOW - minutesAgo * MINUTE).toISOString();
  return {
    identity: accountAnalyticsIdentity(row),
    accountId: row.id,
    sampledAt: at,
    observedAt: at,
    platform: row.platform,
    source: row.source,
    status: 'ok',
    isActive: true,
    windows: [window({ usedPercent: used, remainingPercent: 100 - used, resetAt })],
  };
}

function history(records: AccountAnalyticsObservation[]): AccountAnalyticsHistoryData {
  return { schemaVersion: 1, collectedSince: '2026-10-01T00:00:00.000Z', records };
}

function reading(minutesAgo: number, used: number, resetAt: number | null = null): Reading {
  return { at: NOW - minutesAgo * MINUTE, used, remaining: null, resetAt };
}

describe('agent usage projection', () => {
  it('keeps the account identity and leaves out messages, paths and private capabilities', () => {
    const answer = projectAgentUsage(
      [
        account({
          capabilities: {
            codexProfile: 'gmail',
            claudeProfileId: null,
            claudePlatforms: [],
            antigravityProfileId: `/home/${SENTINEL}`,
          },
        }),
        account({
          id: 'claude:party',
          provider: 'claude',
          providerLabel: 'Claude',
          label: 'Party',
          source: `/home/user/.claude/${SENTINEL}`,
          capabilities: { codexProfile: null, claudeProfileId: 'party', claudePlatforms: ['mac'] },
        }),
      ],
      { now: NOW }
    );
    const text = JSON.stringify(answer);
    expect(text).not.toContain(SENTINEL);
    expect(answer.accounts.map((entry) => entry.provider)).toEqual(['codex', 'claude']);
    expect(answer.accounts[0]).toMatchObject({
      id: 'codex:gmail',
      label: 'Gmail',
      email: 'person@example.com',
      profile: 'gmail',
      source: 'Codex usage on Ubuntu',
      host: 'ubuntu',
      active: true,
      ageSeconds: 0,
    });
    expect(answer.accounts[1]).toMatchObject({ profile: 'party', source: null });
  });

  it('lists an account seen in both dashboard views once, with its newest reading', () => {
    const older = account({
      sampledAt: '2026-10-15T11:00:00.000Z',
      windows: [window({ usedPercent: 10 })],
    });
    const newer = account({ windows: [window({ usedPercent: 50 })] });
    const answer = projectAgentUsage([older, newer, older], { now: NOW });
    expect(answer.accounts).toHaveLength(1);
    expect(answer.accounts[0].windows[0].usedPercent).toBe(50);
  });

  it('marks a reset that passed after the reading, and filters hidden accounts unless asked', () => {
    const passed = account({
      sampledAt: '2026-10-15T09:00:00.000Z',
      windows: [window({ resetAt: '2026-10-15T10:00:00.000Z' })],
    });
    expect(projectAgentUsage([passed], { now: NOW }).accounts[0].windows[0].resetPassed).toBe(true);
    const hidden = account({ hidden: true });
    expect(projectAgentUsage([hidden], { now: NOW }).accounts).toHaveLength(0);
    expect(projectAgentUsage([hidden], { now: NOW, includeHidden: true }).accounts).toHaveLength(1);
  });
});

describe('agent usage burn rate', () => {
  it('measures the last five minutes first, so a burst shows at once', () => {
    const readings = [reading(60, 10), reading(30, 12), reading(5, 15), reading(0, 25)];
    const burn = burnRate(readings, '2026-10-15T14:00:00.000Z');
    expect(burn.last5Minutes).toBe(120);
    expect(burn.pace).toBe(120);
    expect(burn.paceMinutes).toBe(5);
    expect(burn.lastHour).toBe(15);
    // 75 points left at 120 %/h is 37.5 minutes, before the 14:00 reset.
    expect(burn.projectedFullAt).toBe('2026-10-15T12:37:30.000Z');
    expect(burn.runsOutBeforeReset).toBe(true);
  });

  it('never measures across a reset', () => {
    const readings = [reading(50, 90), reading(40, 95), reading(10, 2), reading(0, 4)];
    const burn = burnRate(readings, '2026-10-15T17:00:00.000Z');
    expect(burn.periodStartedAt).toBe(new Date(NOW - 10 * MINUTE).toISOString());
    expect(burn.lastHour).toBeNull();
    expect(burn.last15Minutes).toBe(12);
  });

  it('treats a moved reset time as a new period, and a flat window as not running out', () => {
    const moved = [
      reading(30, 50, NOW + 60 * MINUTE),
      reading(5, 50, NOW + 300 * MINUTE),
      reading(0, 51, NOW + 300 * MINUTE),
    ];
    expect(burnRate(moved, null).periodStartedAt).toBe(new Date(NOW - 5 * MINUTE).toISOString());
    const flat = burnRate([reading(10, 30), reading(0, 30)], null);
    expect(flat.pace).toBe(0);
    expect(flat.projectedFullAt).toBeNull();
    expect(flat.runsOutBeforeReset).toBeNull();
  });

  it('gives no rate without two readings far enough apart', () => {
    const burn = burnRate([reading(1, 30), reading(0, 31)], null);
    expect(burn.pace).toBeNull();
    expect(burn.last5Minutes).toBeNull();
  });

  it('keeps every one-minute reading for an hour, so flat minutes before a burst count', () => {
    const recent = new RecentReadings();
    const row = account({ windows: [window({ usedPercent: 40 })] });
    for (let minute = 70; minute >= 0; minute--)
      recent.add(
        [
          account({
            sampledAt: new Date(NOW - minute * MINUTE).toISOString(),
            windows: [window({ usedPercent: minute === 0 ? 45 : 40 })],
          }),
        ],
        NOW - minute * MINUTE
      );
    const readings = recent.get(row.id, row.windows[0]);
    expect(readings[0].at).toBeGreaterThanOrEqual(NOW - 65 * MINUTE);
    expect(readings.at(-1)?.used).toBe(45);
    // Stored history has only the jump; the ring adds the flat minute before it.
    const { rates } = usageHistory(
      [account({ windows: [window({ usedPercent: 45 })] })],
      projectAgentUsage([account({ windows: [window({ usedPercent: 45 })] })], { now: NOW })
        .accounts,
      history([record(row, 30, 40)]),
      NOW,
      recent
    );
    expect(rates.get('codex:gmail')?.get('five_hour')?.last5Minutes).toBe(60);
  });
});

describe('agent usage snapshot files', () => {
  it('writes identity, readings, burn rates and the rolling history', () => {
    const row = account({ windows: [window({ usedPercent: 30, remainingPercent: 70 })] });
    const { snapshot, history: file } = buildSnapshotFiles(
      {
        accounts: [row],
        history: history([record(row, 60, 10), record(row, 30, 20), record(row, 10, 25)]),
        sampledAt: NOW,
      },
      60
    );
    expect(snapshot.refreshSeconds).toBe(60);
    expect(snapshot.staleAfterSeconds).toBe(300);
    expect(snapshot.accounts[0].windows[0].burn?.last15Minutes).toBe(30);
    expect(file.series).toHaveLength(1);
    expect(file.series[0]).toMatchObject({
      accountId: 'codex:gmail',
      email: 'person@example.com',
      window: 'five_hour',
    });
    expect(file.series[0].points.map((point) => point[1])).toEqual([10, 20, 25, 30]);
    expect(JSON.stringify(snapshot) + JSON.stringify(file)).not.toContain(SENTINEL);
    const words = renderSnapshotMarkdown(snapshot);
    expect(words).toContain(
      '**Gmail** (person@example.com, profile gmail), Pro, Ubuntu VM, active'
    );
    expect(words).toContain('sampled just now');
    expect(words).toContain('30% used, 70% left');
    expect(words).toContain('burning 30%/h over the last 15 min');
    expect(words).toContain('full in 2h 20m (after the reset)');
  });

  it('says a missing number was not reported and skips windows of an account without a reading', () => {
    const { snapshot } = buildSnapshotFiles(
      {
        accounts: [
          account({ status: 'needs_sign_in' }),
          account({
            id: 'qwen:default',
            provider: 'qwen',
            providerLabel: 'Qwen',
            label: 'Qwen',
            windows: [
              window({
                key: 'subscription',
                label: 'Plan subscription',
                usedPercent: null,
                remainingPercent: null,
                resetAt: null,
                expiresAt: '2026-11-03T00:00:00.000Z',
              }),
            ],
          }),
        ],
        history: null,
        sampledAt: NOW,
      },
      60
    );
    const words = renderSnapshotMarkdown(snapshot);
    expect(words).toContain('needs sign-in');
    expect(words).not.toContain('40% used');
    expect(words).toContain('Plan subscription: expires 2026-11-03 00:00 UTC');
  });
});

describe('agent usage snapshot publisher', () => {
  let home = '';
  let previous: Record<'CCS_HOME' | 'CCS_DIR', string | undefined> = {
    CCS_HOME: undefined,
    CCS_DIR: undefined,
  };

  beforeEach(() => {
    home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agent-usage-'));
    previous = { CCS_HOME: process.env.CCS_HOME, CCS_DIR: process.env.CCS_DIR };
    delete process.env.CCS_DIR;
    process.env.CCS_HOME = home;
  });

  afterEach(() => {
    for (const [name, value] of Object.entries(previous)) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
    fs.rmSync(home, { recursive: true, force: true });
  });

  it('writes nothing until a folder is set, then all three files after each sample', async () => {
    const folder = path.join(home, 'share');
    fs.mkdirSync(folder);
    const publish = createSnapshotPublisher({ refreshSeconds: () => 60 });
    const event = { accounts: [account()], history: null, sampledAt: NOW };
    expect(await publish(event)).toBe(false);
    expect(fs.readdirSync(folder)).toEqual([]);
    await writeSnapshotSettings(folder);
    expect((await readSnapshotSettings())?.directory).toBe(folder);
    expect(await publish(event)).toBe(true);
    expect(fs.readdirSync(folder).sort()).toEqual(
      [
        SNAPSHOT_HISTORY,
        SNAPSHOT_JSON,
        SNAPSHOT_MARKDOWN,
        ...CLIENT_FILES.map((file) => file.name),
      ].sort()
    );
    // The reader matches the package byte for byte; the Windows wrapper keeps CRLF.
    expect(fs.readFileSync(path.join(folder, 'aac-usage'), 'utf8')).toBe(
      fs.readFileSync(path.join(clientSourceFolder(), 'aac-usage'), 'utf8').replace(/\r\n/g, '\n')
    );
    expect(fs.readFileSync(path.join(folder, 'aac-usage.cmd'), 'utf8')).toContain('\r\n');
    const readerTime = fs.statSync(path.join(folder, 'aac-usage')).mtimeMs;
    expect(await publish({ ...event, sampledAt: NOW + 60_000 })).toBe(true);
    expect(fs.statSync(path.join(folder, 'aac-usage')).mtimeMs).toBe(readerTime);
    const written = JSON.parse(fs.readFileSync(path.join(folder, SNAPSHOT_JSON), 'utf8'));
    expect(written.accounts[0].email).toBe('person@example.com');
    expect(fs.readFileSync(path.join(folder, SNAPSHOT_JSON), 'utf8')).not.toContain(SENTINEL);
    expect(await publish({ ...event, accounts: [] })).toBe(false);
  });

  it('runs one write at a time and survives a failing folder', async () => {
    let release: () => void = () => {};
    const writes: string[] = [];
    const publish = createSnapshotPublisher({
      settings: async () => ({ version: 1, directory: '/share/aac-usage' }),
      isDirectory: async () => true,
      refreshSeconds: () => 60,
      write: (_directory, name) => {
        writes.push(name);
        return writes.length === 1
          ? new Promise<void>((resolve) => (release = resolve))
          : Promise.resolve();
      },
    });
    const event = { accounts: [account()], history: null, sampledAt: NOW };
    const first = publish(event);
    expect(await publish(event)).toBe(false);
    while (writes.length === 0) await new Promise((resolve) => setTimeout(resolve, 1));
    release();
    expect(await first).toBe(true);
    const failing = createSnapshotPublisher({
      settings: async () => ({ version: 1, directory: '/share/aac-usage' }),
      isDirectory: async () => {
        throw new Error('stale file handle');
      },
    });
    expect(await failing(event)).toBe(false);
  });

  it('refuses a relative or climbing folder', async () => {
    await expect(writeSnapshotSettings('share/aac-usage')).rejects.toThrow();
    await expect(writeSnapshotSettings(`${home}/../x`)).rejects.toThrow();
  });

  it('sets, reports and turns off the folder from the command line', async () => {
    const out: string[] = [];
    const err: string[] = [];
    const io = { out: (text: string) => out.push(text), err: (text: string) => err.push(text) };
    const folder = path.join(home, 'aac-usage');
    expect(await handleUsageSnapshotCommand(['set', folder], io)).toBe(0);
    expect(fs.statSync(folder).isDirectory()).toBe(true);
    expect(await handleUsageSnapshotCommand(['status'], io)).toBe(0);
    expect(out.join('')).toContain(`Usage snapshot: on, folder ${folder}`);
    expect(await handleUsageSnapshotCommand(['set', 'relative/folder'], io)).toBe(1);
    expect(await handleUsageSnapshotCommand(['off'], io)).toBe(0);
    expect(await readSnapshotSettings()).toBeNull();
    expect(await handleUsageSnapshotCommand(['bogus'], io)).toBe(1);
  });
});
