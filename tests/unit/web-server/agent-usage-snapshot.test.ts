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
  burnKey,
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
  writeAtomically,
} from '../../../src/web-server/agent-usage/agent-usage-snapshot';
import { windowReadings } from '../../../src/web-server/agent-usage/agent-usage-history';

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

  it('names the computer the quota came from, not the Claude launcher', () => {
    const claude = account({
      id: 'claude:party',
      provider: 'claude',
      providerLabel: 'Claude',
      platform: 'windows',
      source: 'Verified quota on Ubuntu (fallback)',
    });
    const answer = projectAgentUsage([claude], { now: NOW });
    expect(answer.accounts[0]).toMatchObject({ platform: 'windows', host: 'ubuntu' });
    const nas1 = account({ host: 'nas1', source: 'Kimi Code usage' });
    expect(projectAgentUsage([nas1], { now: NOW }).accounts[0].host).toBe('nas1');
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
    // Flat for ten minutes: too little movement and time for a pace.
    expect(flat.pace).toBeNull();
    expect(flat.projectedFullAt).toBeNull();
    expect(flat.runsOutBeforeReset).toBeNull();
  });

  it('ends a period on any fall when no reset time is reported, so a 1-point reset is not crossed', () => {
    // 1% before an unreported reset, 0% after it, then 2%: only the new period counts.
    const burn = burnRate([reading(5, 1), reading(4, 0), reading(0, 2)], null);
    expect(burn.periodStartedAt).toBe(new Date(NOW - 4 * MINUTE).toISOString());
    expect(burn.last5Minutes).toBe(30);
    // With the same reset time on both readings, a one-point ease of a rolling window is not a reset.
    const reset = NOW + 120 * MINUTE;
    const eased = burnRate(
      [reading(5, 20, reset), reading(4, 19, reset), reading(0, 25, reset)],
      null
    );
    expect(eased.periodStartedAt).toBe(new Date(NOW - 5 * MINUTE).toISOString());
  });

  it('keeps one-minute readings apart for two logins that reuse one dashboard id', () => {
    const recent = new RecentReadings();
    const first = account({
      email: 'first@example.com',
      sampledAt: new Date(NOW - 5 * MINUTE).toISOString(),
      windows: [window({ usedPercent: 10 })],
    });
    const second = account({ email: 'second@example.com', windows: [window({ usedPercent: 90 })] });
    recent.add([first], NOW - 5 * MINUTE);
    recent.add([second], NOW);
    expect(recent.get(second, second.windows[0]).map((entry) => entry.used)).toEqual([90]);
    const { rates } = usageHistory(
      [second],
      projectAgentUsage([second], { now: NOW }).accounts,
      null,
      NOW,
      recent
    );
    expect(rates.get('codex:gmail')?.get(burnKey('five_hour', null, null))?.pace).toBeNull();
  });

  it('keeps a retained optional window at its own sample time, never as a fresh reading', () => {
    const balance = window({
      key: 'prepaid',
      label: 'Prepaid balance',
      kind: 'balance',
      usedPercent: null,
      remainingPercent: null,
      resetAt: null,
      remaining: 12,
      unit: 'USD',
      status: 'cached',
      sampledAt: '2026-10-15T11:00:00.000Z',
    });
    const row = account({ windows: [window(), balance] });
    const records = [10, 5, 0].map((minutesAgo) => ({
      ...record(row, minutesAgo, 40),
      windows: [window(), balance],
    }));
    expect(
      windowReadings(records, balance, 0).map((entry) => new Date(entry.at).toISOString())
    ).toEqual(['2026-10-15T11:00:00.000Z']);
    const published = projectAgentUsage([row], { now: NOW }).accounts[0].windows[1];
    expect(published).toMatchObject({ cached: true, sampledAt: '2026-10-15T11:00:00.000Z' });
    expect(projectAgentUsage([row], { now: NOW }).accounts[0].windows[0]).toMatchObject({
      cached: false,
      sampledAt: '2026-10-15T12:00:00.000Z',
    });
    const { snapshot } = buildSnapshotFiles({ accounts: [row], history: null, sampledAt: NOW }, 60);
    expect(renderSnapshotMarkdown(snapshot)).toContain(
      'Prepaid balance: 12 USD left, earlier reading from 1h 0m ago'
    );
  });

  it("starts a new period when the earlier reading's reset passed before the next reading", () => {
    // The later source omits its reset time; the earlier one said 11:55, between the two readings.
    const burn = burnRate([reading(10, 50, NOW - 5 * MINUTE), reading(0, 60)], null);
    expect(burn.periodStartedAt).toBe(new Date(NOW).toISOString());
    expect(burn.pace).toBeNull();
  });

  it('remembers a known reset across readings that omit it, and keeps pace after a past reset', () => {
    // 11:45 40% (reset 11:55), 11:50 45% (no reset time), 12:00 60% (no reset time).
    const crossed = burnRate(
      [reading(15, 40, NOW - 5 * MINUTE), reading(10, 45), reading(0, 60)],
      null
    );
    expect(crossed.periodStartedAt).toBe(new Date(NOW).toISOString());
    expect(crossed.pace).toBeNull();
    // 11:56 10% and 12:00 20%, both still reporting the 11:55 reset they came after.
    const after = burnRate(
      [reading(4, 10, NOW - 5 * MINUTE), reading(0, 20, NOW - 5 * MINUTE)],
      null
    );
    expect(after.pace).toBe(150);
    expect(after.paceMinutes).toBe(5);
  });

  it('splits only at reported resets, from any reading, and keeps post-reset pace', () => {
    const at = (minutesAgo: number) => NOW - minutesAgo * MINUTE;
    // 11:45 40% (reset 11:55), 11:56 10%, 12:00 20% (next reset 16:55): pace from 11:56.
    const late = burnRate(
      [reading(15, 40, at(5)), reading(4, 10), reading(0, 20, NOW + 295 * MINUTE)],
      null
    );
    expect(late.periodStartedAt).toBe(new Date(at(4)).toISOString());
    expect(late.pace).toBe(150);
    // An expired reset (06:56) does not make a one-point fall noise: 11:55 1%, 11:56 0%, 12:00 2%.
    const expired = burnRate([reading(5, 1, at(304)), reading(4, 0), reading(0, 2)], null);
    expect(expired.periodStartedAt).toBe(new Date(at(4)).toISOString());
    expect(expired.last5Minutes).toBe(30);
    // A boundary only the later reading reports: 11:50 45%, 12:00 60% (reset 11:55).
    const reportedLater = burnRate([reading(10, 45), reading(0, 60, at(5))], null);
    expect(reportedLater.pace).toBeNull();
    // A reading without a percentage still carries its reset: 11:45 40%, 11:50 (reset 11:55), 12:00 60%.
    const silent: Reading = { at: at(10), used: null, remaining: 55, resetAt: at(5) };
    const withSilent = burnRate([reading(15, 40), silent, reading(0, 60)], null);
    expect(withSilent.pace).toBeNull();
  });

  it('splits at a reset reported by a reading with no numbers, from history and from the ring', () => {
    const row = account({
      windows: [window({ usedPercent: 60, remainingPercent: 40, resetAt: null })],
    });
    const resetOnly = window({
      usedPercent: null,
      remainingPercent: null,
      resetAt: '2026-10-15T11:55:00.000Z',
    });
    const stored = [
      {
        ...record(row, 15, 40),
        windows: [window({ usedPercent: 40, remainingPercent: 60, resetAt: null })],
      },
      { ...record(row, 10, 0), windows: [resetOnly] },
    ];
    const projected = projectAgentUsage([row], { now: NOW }).accounts;
    const fromHistory = usageHistory([row], projected, history(stored), NOW);
    expect(
      fromHistory.rates.get('codex:gmail')?.get(burnKey('five_hour', null, null))?.pace
    ).toBeNull();
    expect(fromHistory.series[0].points.map((point) => point[1])).toEqual([40, 60]);
    const recent = new RecentReadings();
    recent.add(
      [
        account({
          sampledAt: new Date(NOW - 15 * MINUTE).toISOString(),
          windows: [window({ usedPercent: 40, resetAt: null })],
        }),
      ],
      NOW - 15 * MINUTE
    );
    recent.add(
      [account({ sampledAt: new Date(NOW - 10 * MINUTE).toISOString(), windows: [resetOnly] })],
      NOW - 10 * MINUTE
    );
    const fromRing = usageHistory([row], projected, null, NOW, recent);
    expect(
      fromRing.rates.get('codex:gmail')?.get(burnKey('five_hour', null, null))?.pace
    ).toBeNull();
  });

  it('takes a fractional two-point move as a burst', () => {
    // Cursor reports fractions: 32.3 - 30.3 is 1.9999999999999964 in floating point.
    const burst = burnRate([reading(60, 20.3), reading(5, 30.3), reading(0, 32.3)], null);
    expect(burst.pace).toBe(24);
    expect(burst.paceMinutes).toBe(5);
    const below = burnRate([reading(60, 20.3), reading(5, 30.3), reading(0, 31.8)], null);
    expect(below.paceMinutes).toBe(60);
  });

  it('keeps a reset reported without the window unit as a boundary', () => {
    // Qwen drops the unit when its quota-config request fails; the reset still bounds the window.
    const unit = { unit: 'credits', resetAt: null };
    const row = account({ windows: [window({ usedPercent: 60, ...unit })] });
    const stored = [
      { ...record(row, 15, 40), windows: [window({ usedPercent: 40, ...unit })] },
      {
        ...record(row, 10, 0),
        windows: [
          window({
            usedPercent: null,
            remainingPercent: null,
            unit: null,
            resetAt: '2026-10-15T11:55:00.000Z',
          }),
        ],
      },
    ];
    const projected = projectAgentUsage([row], { now: NOW }).accounts;
    const key = burnKey('five_hour', 'credits', null);
    expect(
      usageHistory([row], projected, history(stored), NOW).rates.get('codex:gmail')?.get(key)?.pace
    ).toBeNull();
    const recent = new RecentReadings();
    recent.add(
      [
        account({
          sampledAt: new Date(NOW - 15 * MINUTE).toISOString(),
          windows: [window({ usedPercent: 40, ...unit })],
        }),
      ],
      NOW - 15 * MINUTE
    );
    recent.add(
      [
        account({
          sampledAt: new Date(NOW - 10 * MINUTE).toISOString(),
          windows: [
            window({
              usedPercent: null,
              remainingPercent: null,
              unit: null,
              resetAt: '2026-10-15T11:55:00.000Z',
            }),
          ],
        }),
      ],
      NOW - 10 * MINUTE
    );
    expect(
      usageHistory([row], projected, null, NOW, recent).rates.get('codex:gmail')?.get(key)?.pace
    ).toBeNull();
  });

  it("keeps an account's history through an outage, without a reading or a pace", () => {
    const live = account();
    const down = account({ status: 'unavailable', windows: [] });
    const { rates, series } = usageHistory(
      [down],
      projectAgentUsage([down], { now: NOW }).accounts,
      history([record(live, 30, 20), record(live, 10, 30)]),
      NOW
    );
    expect(series).toHaveLength(1);
    expect(series[0].points.map((point) => point[1])).toEqual([20, 30]);
    expect(rates.get('codex:gmail')?.size).toBe(0);
  });

  it('does not take a single rounding step within minutes as the pace', () => {
    // Weekly window: 20% for two hours, then 21%.
    const readings = [
      ...Array.from({ length: 25 }, (_, step) => reading(125 - step * 5, 20)),
      reading(0, 21),
    ];
    // 79 points at 1%/h last 79 hours: past a reset two days away.
    const burn = burnRate(readings, new Date(NOW + 2 * 24 * 60 * MINUTE).toISOString());
    expect(burn.last5Minutes).toBe(12);
    expect(burn.pace).toBe(1);
    expect(burn.paceMinutes).toBe(60);
    expect(burn.runsOutBeforeReset).toBe(false);
    // Two points in five minutes is a burst and sets the pace at once.
    const burst = burnRate([reading(60, 20), reading(5, 20), reading(0, 22)], null);
    expect(burst.pace).toBe(24);
    expect(burst.paceMinutes).toBe(5);
  });

  it('publishes no burn for a window whose reset passed, and keeps burns of same-key pools apart', () => {
    const passed = account({
      sampledAt: '2026-10-15T09:00:00.000Z',
      windows: [window({ resetAt: '2026-10-15T10:00:00.000Z' })],
    });
    const stale = buildSnapshotFiles(
      {
        accounts: [passed],
        history: history([record(passed, 200, 10), record(passed, 180, 40)]),
        sampledAt: NOW,
      },
      60
    );
    expect(stale.snapshot.accounts[0].windows[0]).toMatchObject({ resetPassed: true, burn: null });
    const pools = account({
      windows: [
        window({ key: 'weekly', usedPercent: 50, poolId: 'pool-a', poolLabel: 'Pool A' }),
        window({ key: 'weekly', usedPercent: 10, poolId: 'pool-b', poolLabel: 'Pool B' }),
      ],
    });
    const recent = new RecentReadings();
    recent.add(
      [
        account({
          sampledAt: new Date(NOW - 5 * MINUTE).toISOString(),
          windows: [
            window({ key: 'weekly', usedPercent: 40, poolId: 'pool-a', poolLabel: 'Pool A' }),
            window({ key: 'weekly', usedPercent: 10, poolId: 'pool-b', poolLabel: 'Pool B' }),
          ],
        }),
      ],
      NOW - 5 * MINUTE
    );
    const { snapshot } = buildSnapshotFiles(
      { accounts: [pools], history: null, sampledAt: NOW },
      60,
      recent
    );
    expect(snapshot.accounts[0].windows.map((entry) => entry.burn?.pace)).toEqual([120, null]);
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
    const readings = recent.get(row, row.windows[0]);
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
    expect(rates.get('codex:gmail')?.get(burnKey('five_hour', null, null))?.last5Minutes).toBe(60);
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

  it('calls a falling window falling, never a negative burn', () => {
    const row = account({ windows: [window({ usedPercent: 27.6, remainingPercent: 72.4 })] });
    const { snapshot } = buildSnapshotFiles(
      {
        accounts: [row],
        history: history([record(row, 15, 30), record(row, 10, 29.2), record(row, 5, 28.4)]),
        sampledAt: NOW,
      },
      60
    );
    expect(snapshot.accounts[0].windows[0].burn?.pace).toBe(-9.6);
    const words = renderSnapshotMarkdown(snapshot);
    expect(words).toContain('usage falling -9.60%/h over the last 15 min');
    expect(words).not.toContain('burning');
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

  it('replaces a planted link at the target name without writing through it', async () => {
    const folder = path.join(home, 'share');
    fs.mkdirSync(folder);
    const victim = path.join(home, 'victim.txt');
    fs.writeFileSync(victim, 'keep me');
    try {
      fs.symlinkSync(victim, path.join(folder, SNAPSHOT_JSON));
    } catch {
      return; // This Windows session cannot create symbolic links.
    }
    await writeAtomically(folder, SNAPSHOT_JSON, '{}');
    expect(fs.readFileSync(victim, 'utf8')).toBe('keep me');
    expect(fs.lstatSync(path.join(folder, SNAPSHOT_JSON)).isSymbolicLink()).toBe(false);
    expect(fs.readFileSync(path.join(folder, SNAPSHOT_JSON), 'utf8')).toBe('{}');
    expect(fs.readdirSync(folder).filter((name) => name.endsWith('.tmp'))).toEqual([]);
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
