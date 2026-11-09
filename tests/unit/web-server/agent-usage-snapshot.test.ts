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
  downgradeCrossedSignInRows,
  readSnapshotSettings,
  renderSnapshotMarkdown,
  SNAPSHOT_HISTORY,
  SNAPSHOT_JSON,
  SNAPSHOT_LOCATIONS,
  SNAPSHOT_MARKDOWN,
  snapshotSettingsPath,
  writeSnapshotSettings,
} from '../../../src/web-server/agent-usage/agent-usage-snapshot';
import { handleUsageSnapshotCommand } from '../../../src/commands/usage-snapshot-command';
import { writePrivateJsonFile } from '../../../src/web-server/services/private-json-store';
import {
  CLIENT_FILES,
  RUNNER_FILES,
  RUNNER_FOLDER,
  runnerSourceFolder,
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

  it('screens the plan name while counting code points', () => {
    const planOf = (plan: unknown) =>
      projectAgentUsage([account({ plan: plan as never })], { now: NOW }).accounts[0].plan;
    expect(planOf('Ultra')).toBe('Ultra');
    // 80 astral characters are 80 code points (160 UTF-16 units): they pass.
    expect(planOf('🚀'.repeat(80))).toBe('🚀'.repeat(80));
    expect(planOf('🚀'.repeat(81))).toBeNull();
    // Control characters and coerced non-strings never publish.
    expect(planOf('Pro\nMax')).toBeNull();
    expect(planOf('Pro\u0085Max')).toBeNull();
    // Bidi marks and controls never publish either.
    expect(planOf('Pro\u200eMax')).toBeNull();
    expect(planOf('Pro\u202eMax')).toBeNull();
    expect(planOf('sk-abcdefgh')).toBeNull();
    expect(planOf(42)).toBeNull();
    expect(planOf({ tier: 'max' })).toBeNull();
    expect(planOf('')).toBeNull();
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

  it('publishes dynamic keys, clears and the session release into usage.json windows', () => {
    const row = account({
      id: 'claude:party',
      provider: 'claude',
      providerLabel: 'Claude',
      windows: [
        window({
          key: 'seven_day_future_model',
          label: 'Weekly Future usage',
          windowMinutes: 10080,
          kind: 'rate_limit',
        }),
        window({
          key: 'reset_credit_used_grant_1',
          label: 'Used rate-limit reset grant 1',
          usedPercent: null,
          remainingPercent: null,
          windowMinutes: null,
          resetAt: null,
          kind: 'balance',
          used: 0,
          limit: 1,
          remaining: 1,
          unit: 'resets',
          clears: ['five_hour', 'seven_day'],
        }),
        window({
          key: 'session_reset_allowance',
          label: 'Weekly session reset allowance',
          usedPercent: null,
          remainingPercent: null,
          windowMinutes: null,
          kind: 'balance',
          used: 1,
          limit: 3,
          remaining: 2,
          unit: 'resets',
        }),
      ],
    });
    const answer = projectAgentUsage([row], { now: NOW, includeHidden: true });
    const windows = answer.accounts[0].windows;
    expect(windows.map((item) => item.key)).toEqual([
      'seven_day_future_model',
      'reset_credit_used_grant_1',
      'session_reset_allowance',
    ]);
    expect(windows[0]).toMatchObject({ label: 'Weekly Future usage', windowMinutes: 10080 });
    expect(windows[0].clears).toBeNull();
    expect(windows[1].clears).toEqual(['five_hour', 'seven_day']);
    expect(windows[2].clears).toBeNull();
    expect(windows[2]).toMatchObject({ remaining: 2, unit: 'resets' });
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
    // Each point carries its reset time, so a reader never averages across a reset.
    expect(file.series[0].points.every((point) => point[3] === '2026-10-15T14:00:00.000Z')).toBe(
      true
    );
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

  it('says a window already used up is at its limit, not full in now', () => {
    const row = account({ windows: [window({ usedPercent: 100, remainingPercent: 0 })] });
    const { snapshot } = buildSnapshotFiles(
      {
        accounts: [row],
        history: history([record(row, 70, 100), record(row, 30, 100)]),
        sampledAt: NOW,
      },
      60
    );
    const words = renderSnapshotMarkdown(snapshot);
    expect(words).toContain('at its limit until the reset (0%/h over the last 6 h)');
    expect(words).not.toContain('full in now');
  });

  it('publishes no pace for an old cached reading or an account that needs a sign-in', () => {
    const old = new Date(NOW - 3 * 60 * 60 * 1000).toISOString();
    const cached = account({
      status: 'cached',
      sampledAt: old,
      windows: [window({ usedPercent: 60, remainingPercent: 40 })],
    });
    const signIn = account({
      id: 'codex:party',
      status: 'needs_sign_in',
      windows: [window({ usedPercent: 60, remainingPercent: 40 })],
    });
    const { snapshot } = buildSnapshotFiles(
      {
        accounts: [cached, signIn],
        history: history([
          record(cached, 200, 10),
          record(cached, 185, 60),
          record(signIn, 30, 10),
          record(signIn, 15, 60),
        ]),
        sampledAt: NOW,
      },
      60
    );
    expect(snapshot.accounts.map((row) => row.windows[0].burn)).toEqual([null, null]);
    expect(renderSnapshotMarkdown(snapshot)).not.toContain('at its limit');
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

  it('never starts the sign-in probes while no folder is configured', async () => {
    let signInCalls = 0;
    const publish = createSnapshotPublisher({
      settings: async () => null,
      signIn: async () => {
        signInCalls += 1;
        return [];
      },
    });
    expect(await publish({ accounts: [account()], history: null, sampledAt: NOW })).toBe(false);
    expect(signInCalls).toBe(0);
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
        SNAPSHOT_LOCATIONS,
        SNAPSHOT_MARKDOWN,
        ...CLIENT_FILES.map((file) => file.name),
        RUNNER_FOLDER,
      ].sort()
    );
    // The sign-in renewal runner is kept current beside the reader, byte for byte.
    expect(fs.readdirSync(path.join(folder, RUNNER_FOLDER)).sort()).toEqual(
      RUNNER_FILES.map((file) => file.name).sort()
    );
    expect(fs.readFileSync(path.join(folder, RUNNER_FOLDER, 'claude_signin.py'), 'utf8')).toBe(
      fs.readFileSync(path.join(runnerSourceFolder(), 'claude_signin.py'), 'utf8')
    );
    expect(JSON.parse(fs.readFileSync(path.join(folder, SNAPSHOT_LOCATIONS), 'utf8'))).toEqual({
      version: 1,
      locations: [],
    });
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

  it('publishes the saved locations and follows a change without a restart', async () => {
    const folder = path.join(home, 'share');
    fs.mkdirSync(folder);
    const mac = { where: 'the Mac', path: '/Volumes/share/aac-usage' };
    const windows = { where: 'Windows', path: '\\\\nas\\share\\aac-usage' };
    await writeSnapshotSettings(folder, undefined, [mac, windows]);
    expect((await readSnapshotSettings())?.locations).toEqual([mac, windows]);
    const writes: string[] = [];
    const publish = createSnapshotPublisher({
      refreshSeconds: () => 60,
      syncClient: async () => {},
      write: async (directory, name, body) => {
        writes.push(name);
        await writeAtomically(directory, name, body);
      },
    });
    const event = { accounts: [account()], history: null, sampledAt: NOW };
    expect(await publish(event)).toBe(true);
    const read = () => JSON.parse(fs.readFileSync(path.join(folder, SNAPSHOT_LOCATIONS), 'utf8'));
    expect(read()).toEqual({ version: 1, locations: [mac, windows] });
    expect(await publish({ ...event, sampledAt: NOW + MINUTE })).toBe(true);
    expect(writes.filter((name) => name === SNAPSHOT_LOCATIONS)).toHaveLength(1);
    await writeSnapshotSettings(folder, undefined, [mac]);
    expect(await publish({ ...event, sampledAt: NOW + 2 * MINUTE })).toBe(true);
    expect(read()).toEqual({ version: 1, locations: [mac] });
  });

  it('reads back the largest setting it accepts', async () => {
    // Three UTF-8 bytes per character and a backslash JSON doubles: the most bytes per character.
    const folder = path.join(home, 's'.repeat(Math.max(1, 1000 - home.length)));
    const longest = Array.from({ length: 8 }, (_, index) => ({
      where: `${index}${'界'.repeat(59)}`,
      path: `\\${'界'.repeat(1023)}`,
    }));
    await writeSnapshotSettings(folder, undefined, longest);
    expect(await readSnapshotSettings()).toEqual({
      version: 1,
      directory: folder,
      locations: longest,
    });
  });

  it('refuses bad locations and drops a damaged list without losing the folder', async () => {
    const folder = path.join(home, 'share');
    await expect(
      writeSnapshotSettings(folder, undefined, [{ where: 'x', path: 'bad\npath' }])
    ).rejects.toThrow();
    await expect(
      writeSnapshotSettings(
        folder,
        undefined,
        Array.from({ length: 9 }, (_, index) => ({ where: `w${index}`, path: '/p' }))
      )
    ).rejects.toThrow();
    // Written the way the setting is (private), so only the damaged list differs.
    await writePrivateJsonFile(snapshotSettingsPath(), {
      version: 1,
      directory: folder,
      locations: [{ where: 1 }],
    });
    expect(await readSnapshotSettings()).toEqual({ version: 1, directory: folder });
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
    expect(
      await handleUsageSnapshotCommand(
        [
          'set',
          folder,
          '--location',
          'the Mac=/Volumes/share/aac-usage',
          '--location=Linux=/mnt/s=x',
        ],
        io
      )
    ).toBe(0);
    const saved = [
      { where: 'the Mac', path: '/Volumes/share/aac-usage' },
      { where: 'Linux', path: '/mnt/s=x' },
    ];
    expect((await readSnapshotSettings())?.locations).toEqual(saved);
    // set without --location keeps the saved ones
    expect(await handleUsageSnapshotCommand(['set', folder], io)).toBe(0);
    expect((await readSnapshotSettings())?.locations).toEqual(saved);
    expect(await handleUsageSnapshotCommand(['status'], io)).toBe(0);
    expect(out.join('')).toContain('  the Mac: /Volumes/share/aac-usage\n');
    expect(await handleUsageSnapshotCommand(['set', folder, '--location', 'no-equals'], io)).toBe(
      1
    );
    expect(await handleUsageSnapshotCommand(['set', folder, '--location'], io)).toBe(1);
    expect(await handleUsageSnapshotCommand(['set', folder, folder], io)).toBe(1);
    expect(await handleUsageSnapshotCommand(['off'], io)).toBe(0);
    expect(await readSnapshotSettings()).toBeNull();
    expect(await handleUsageSnapshotCommand(['bogus'], io)).toBe(1);
  });
});

describe('sign-in health section', () => {
  const row = (overrides: Record<string, unknown> = {}) => ({
    name: 'me',
    provider: 'claude',
    platform: 'ubuntu',
    machine: 'ubuntu',
    state: 'ok',
    fixCommand: "CLAUDE_CONFIG_DIR='~/.claude-t3/me' claude auth login",
    ...overrides,
  });
  const snapshotWith = (profiles: Record<string, unknown>[]) =>
    ({
      accounts: [],
      generatedAt: new Date(NOW).toISOString(),
      refreshSeconds: 60,
      staleAfterSeconds: 180,
      signIn: profiles,
    }) as unknown as AgentUsageSnapshot;

  it('publishes the section worst-first with end time and fix command', () => {
    const markdown = renderSnapshotMarkdown(
      snapshotWith([
        row({ state: 'ok', loggedOutAt: '2026-11-30T23:04:00Z' }),
        row({
          name: 'party',
          machine: 'windows',
          platform: 'windows',
          state: 'critical',
          loggedOutAt: '2026-11-04T09:43:00Z',
        }),
      ])
    );
    const party = markdown.indexOf('claude · windows/party');
    const me = markdown.indexOf('claude · ubuntu/me');
    expect(party).toBeGreaterThanOrEqual(0);
    expect(party).toBeLessThan(me);
    // The word carries the actual end, never a threshold: this critical has
    // days left, so it says when it ends instead of "within 4 hours".
    expect(markdown).toContain('ends 2026-11-04 09:43 UTC');
    expect(markdown).not.toContain('within 4 hours');
    expect(markdown).toContain('re-login before then');
  });

  it('says logged out for empty tokens and unavailable for a failed host', () => {
    const markdown = renderSnapshotMarkdown(
      snapshotWith([
        row({ state: 'logged_out', loggedOutAt: '2026-11-04T09:43:00Z' }),
        {
          name: 'mac',
          provider: 'claude',
          platform: 'mac',
          machine: 'mac',
          state: 'unavailable',
          fixCommand: '',
          error: 'connection refused',
        },
      ])
    );
    expect(markdown).toContain('already logged out');
    expect(markdown).toContain('re-login: `');
    expect(markdown).not.toContain('re-login before then');
    expect(markdown).not.toContain('sign-in ends');
    expect(markdown).toContain('not readable on this machine. (connection refused)');
  });

  it('reads a live row rendered past its deadline as awaiting recheck, never healthy', () => {
    const ended = renderSnapshotMarkdown(
      snapshotWith([row({ state: 'ok', loggedOutAt: '2026-10-15T11:00:00Z' })])
    );
    expect(ended).toContain('past its deadline; awaiting recheck');
    expect(ended).not.toContain('sign-in ends');
    expect(ended).toContain('re-login: `');
    const refreshGone = renderSnapshotMarkdown(
      snapshotWith([
        row({ state: 'ok', loggedOutAt: null, refreshExpiresAt: '2026-10-15T11:00:00Z' }),
      ])
    );
    expect(refreshGone).toContain('refresh expired');
    expect(refreshGone).toContain('awaiting recheck');
    expect(refreshGone).not.toContain('healthy');
  });

  it('downgrades crossed rows on copies, never the collected rows', () => {
    const rows = [
      row({ state: 'critical', loggedOutAt: '2026-10-15T12:30:00Z' }),
      row({ name: 'later', state: 'critical', loggedOutAt: '2026-10-16T12:00:00Z' }),
      // A stamp already past before the window is a fact the collected
      // state already accounts for, not a new crossing.
      row({
        name: 'renewing',
        state: 'warn',
        loggedOutAt: null,
        accessExpiresAt: '2026-10-15T11:00:00Z',
        refreshExpiresAt: '2026-11-30T00:00:00Z',
      }),
    ] as never[];
    // A known login end already past at `now` (a behind-clock remote)
    // is crossed even when it predates the window; only the login end
    // gets that treatment, never the renewable access stamp above.
    const skewed = downgradeCrossedSignInRows(
      [row({ state: 'critical', loggedOutAt: '2026-10-15T11:00:00Z' })] as never[],
      NOW,
      NOW
    );
    expect(skewed[0].state).toBe('unavailable');
    // A past refresh end with no named login end is the same skew
    // shape through the other stamp: no single-clock collector emits
    // it live, so it counts crossed here too.
    const refreshSkewed = downgradeCrossedSignInRows(
      [
        row({
          state: 'warn',
          loggedOutAt: null,
          accessExpiresAt: '2026-10-15T11:00:00Z',
          refreshExpiresAt: '2026-10-15T11:30:00Z',
        }),
      ] as never[],
      NOW,
      NOW
    );
    expect(refreshSkewed[0].state).toBe('unavailable');
    const out = downgradeCrossedSignInRows(rows, NOW, NOW + 60 * MINUTE);
    expect(out[0].state).toBe('unavailable');
    expect(out[0].error).toBe('sign-in state changed during collection; recheck pending');
    expect(out[0].loggedOutAt).toBeNull();
    expect(out[1].state).toBe('critical');
    expect(out[2].state).toBe('warn');
    // The collected (and cached) rows are untouched.
    expect((rows[0] as { state: string }).state).toBe('critical');
  });

  it('rechecks sign-in deadlines at publication time, not the sample time', async () => {
    const writes = new Map<string, string>();
    const liveRow = {
      name: 'soon',
      provider: 'claude',
      platform: 'ubuntu',
      machine: 'ubuntu',
      state: 'critical' as const,
      loggedOutAt: new Date(Date.now() + 40).toISOString(),
      fixCommand: 'claude auth login',
    };
    const publish = createSnapshotPublisher({
      settings: async () => ({ version: 1, directory: '/share/aac-usage' }),
      // A slow folder check holds publication past the row's deadline;
      // the published reading must not present it as a future end.
      isDirectory: async () => {
        await new Promise((resolve) => setTimeout(resolve, 120));
        return true;
      },
      refreshSeconds: () => 60,
      signIn: async () => [liveRow],
      write: async (_directory, name, body) => {
        writes.set(name, body);
      },
      syncClient: async () => {},
    });
    expect(await publish({ accounts: [account()], history: null, sampledAt: Date.now() })).toBe(
      true
    );
    const published = JSON.parse(writes.get(SNAPSHOT_JSON) as string);
    expect(published.signIn[0].state).toBe('unavailable');
    expect(writes.get(SNAPSHOT_MARKDOWN)).not.toContain('re-login before then');
    // The health service's own row is never mutated by publication.
    expect(liveRow.state).toBe('critical');
  });

  it('rechecks again after a slow markdown write, before the JSON lands', async () => {
    const writes: Array<[string, string]> = [];
    const liveRow = {
      name: 'soon',
      provider: 'claude',
      platform: 'ubuntu',
      machine: 'ubuntu',
      state: 'critical' as const,
      loggedOutAt: new Date(Date.now() + 80).toISOString(),
      fixCommand: 'claude auth login',
    };
    const publish = createSnapshotPublisher({
      settings: async () => ({ version: 1, directory: '/share/aac-usage' }),
      isDirectory: async () => true,
      refreshSeconds: () => 60,
      signIn: async () => [liveRow],
      // The markdown write itself is the slow stage (a network share):
      // the deadline crosses while it is in flight.
      write: async (_directory, name, body) => {
        if (name === SNAPSHOT_MARKDOWN) await new Promise((resolve) => setTimeout(resolve, 200));
        writes.push([name, body]);
      },
      syncClient: async () => {},
    });
    expect(await publish({ accounts: [account()], history: null, sampledAt: Date.now() })).toBe(
      true
    );
    const json = writes.find(([name]) => name === SNAPSHOT_JSON)?.[1] as string;
    expect(JSON.parse(json).signIn[0].state).toBe('unavailable');
    const markdownWrites = writes.filter(([name]) => name === SNAPSHOT_MARKDOWN);
    expect(markdownWrites[markdownWrites.length - 1][1]).not.toContain('re-login before then');
    expect(liveRow.state).toBe('critical');
  });

  it('rechecks after the JSON write too, converging both files', async () => {
    const writes: Array<[string, string]> = [];
    const liveRow = {
      name: 'soon',
      provider: 'claude',
      platform: 'ubuntu',
      machine: 'ubuntu',
      state: 'critical' as const,
      loggedOutAt: new Date(Date.now() + 80).toISOString(),
      fixCommand: 'claude auth login',
    };
    const publish = createSnapshotPublisher({
      settings: async () => ({ version: 1, directory: '/share/aac-usage' }),
      isDirectory: async () => true,
      refreshSeconds: () => 60,
      signIn: async () => [liveRow],
      // This time the JSON write is the slow stage: the deadline
      // crosses while usage.json itself is in flight, after every
      // earlier recheck has passed.
      write: async (_directory, name, body) => {
        if (name === SNAPSHOT_JSON) await new Promise((resolve) => setTimeout(resolve, 200));
        writes.push([name, body]);
      },
      syncClient: async () => {},
    });
    expect(await publish({ accounts: [account()], history: null, sampledAt: Date.now() })).toBe(
      true
    );
    const jsonWrites = writes.filter(([name]) => name === SNAPSHOT_JSON);
    const finalJson = jsonWrites[jsonWrites.length - 1][1];
    expect(JSON.parse(finalJson).signIn[0].state).toBe('unavailable');
    const markdownWrites = writes.filter(([name]) => name === SNAPSHOT_MARKDOWN);
    expect(markdownWrites[markdownWrites.length - 1][1]).not.toContain('re-login before then');
    expect(liveRow.state).toBe('critical');
  });

  it('corrects the JSON when the markdown correction itself fails', async () => {
    const writes: Array<[string, string]> = [];
    let markdownWrites = 0;
    const liveRow = {
      name: 'soon',
      provider: 'claude',
      platform: 'ubuntu',
      machine: 'ubuntu',
      state: 'critical' as const,
      loggedOutAt: new Date(Date.now() + 80).toISOString(),
      fixCommand: 'claude auth login',
    };
    const publish = createSnapshotPublisher({
      settings: async () => ({ version: 1, directory: '/share/aac-usage' }),
      isDirectory: async () => true,
      refreshSeconds: () => 60,
      signIn: async () => [liveRow],
      // The deadline crosses during the JSON write; the markdown
      // correction then fails (dead share). usage.json is the file
      // machines read: it must not keep the expired row as live —
      // a final JSON correction carries the downgraded rows.
      write: async (_directory, name, body) => {
        if (name === SNAPSHOT_JSON) await new Promise((resolve) => setTimeout(resolve, 200));
        if (name === SNAPSHOT_MARKDOWN) {
          markdownWrites += 1;
          if (markdownWrites > 1) throw new Error('share gone');
        }
        writes.push([name, body]);
      },
      syncClient: async () => {},
    });
    expect(await publish({ accounts: [account()], history: null, sampledAt: Date.now() })).toBe(
      false
    );
    const jsonWrites = writes.filter(([name]) => name === SNAPSHOT_JSON);
    const finalJson = jsonWrites[jsonWrites.length - 1][1];
    expect(JSON.parse(finalJson).signIn[0].state).toBe('unavailable');
  });

  it('marks the sign-in section when the fetch itself failed', async () => {
    const writes = new Map<string, string>();
    const publish = createSnapshotPublisher({
      settings: async () => ({ version: 1, directory: '/share/aac-usage' }),
      isDirectory: async () => true,
      refreshSeconds: () => 60,
      // A rejected (or timed-out) fetch must not read as "no sign-in
      // problems": the section stays, naming the failed collection.
      signIn: async () => {
        throw new Error('collector exploded');
      },
      write: async (_directory, name, body) => {
        writes.set(name, body);
      },
      syncClient: async () => {},
    });
    expect(await publish({ accounts: [account()], history: null, sampledAt: Date.now() })).toBe(
      true
    );
    const published = JSON.parse(writes.get(SNAPSHOT_JSON) as string);
    expect(published.signIn).toHaveLength(1);
    expect(published.signIn[0].state).toBe('unavailable');
    expect(published.signIn[0].error).toBe('sign-in collection failed');
    expect(writes.get(SNAPSHOT_MARKDOWN)).toContain('sign-in collection failed');
  });

  it('advises before a known end even when no command is published', () => {
    const markdown = renderSnapshotMarkdown(
      snapshotWith([
        row({ state: 'critical', fixCommand: '', loggedOutAt: '2026-10-16T12:00:00Z' }),
        row({
          name: 'refreshable',
          state: 'ok',
          loggedOutAt: null,
          refreshExpiresAt: '2026-10-20T12:00:00Z',
        }),
      ])
    );
    expect(markdown).toContain('re-login before then');
    expect(markdown).toContain('refresh expires 2026-10-20 12:00 UTC');
  });

  it('never puts an unpaired surrogate from a repair command into the markdown', () => {
    // Production rows are screened upstream; the renderer is the last
    // line before UTF-8 bytes, so it refuses the command itself too.
    const markdown = renderSnapshotMarkdown(
      snapshotWith([
        row({
          state: 'critical',
          fixCommand: 'CLAUDE_CONFIG_DIR="$HOME/.claude-t3/bad\udcff" claude auth login',
        }),
      ])
    );
    expect(markdown).not.toContain('\udcff');
    expect(Buffer.from(markdown, 'utf8').toString('utf8')).not.toContain('bad�');
  });

  it('names no invented end when the collector could not derive one', () => {
    const markdown = renderSnapshotMarkdown(
      snapshotWith([row({ state: 'ok', loggedOutAt: null })])
    );
    expect(markdown).toContain('no exact end published');
    expect(markdown).not.toContain('sign-in ends');
    expect(markdown).not.toContain('closes soon');
  });

  it('carries the entitlement class into the published snapshot accounts', () => {
    const accountWithClass = {
      id: 'claude:one',
      provider: 'claude',
      providerLabel: 'Claude',
      label: 'One',
      email: null,
      profile: null,
      source: null,
      plan: 'Ultra',
      planClass: 'max',
      platform: 'ubuntu',
      host: 'ubuntu',
      active: false,
      status: 'ok',
      sampledAt: NOW,
      ageSeconds: 0,
      hidden: false,
      windows: [],
    } as unknown as Parameters<typeof buildSnapshotFiles>[0]['accounts'][number];
    const { snapshot } = buildSnapshotFiles(
      { accounts: [accountWithClass], history: null, sampledAt: NOW },
      60
    );
    expect(snapshot.accounts[0]?.plan).toBe('Ultra');
    expect(snapshot.accounts[0]?.planClass).toBe('max');
  });

  it('omits the section when no machine reported profiles', () => {
    const { snapshot } = buildSnapshotFiles({ accounts: [], history: null, sampledAt: NOW }, 60);
    expect(snapshot.signIn).toBeUndefined();
    expect(renderSnapshotMarkdown(snapshot)).not.toContain('Sign-in health');
  });
});

describe('sign-in renewal in usage.md', () => {
  it('names what needs a person and a stopped runner, and renewed rows say they renew themselves', () => {
    const base = {
      version: 1 as const,
      source: 'AI Account Center' as const,
      generatedAt: new Date(NOW).toISOString(),
      refreshSeconds: 60,
      staleAfterSeconds: 300,
      accounts: [],
      signIn: [
        {
          name: 'gmail',
          provider: 'claude',
          platform: 'mac',
          machine: 'mac',
          email: 'a@example.com',
          refreshExpiresAt: new Date(NOW + 40 * 86400_000).toISOString(),
          loggedOutAt: new Date(NOW + 40 * 86400_000).toISOString(),
          state: 'ok' as const,
          fixCommand: '',
          autoRenew: true,
          renewFrom: new Date(NOW + 36 * 86400_000).toISOString(),
          lastResult: 'renewed',
          lastRenewAt: new Date(NOW - 3600_000).toISOString(),
        },
      ],
    };
    const fresh = renderSnapshotMarkdown({
      ...base,
      signInRenewal: {
        runner: 'mac',
        generatedAt: new Date(NOW - 60_000).toISOString(),
        stale: false,
        needsYou: [
          { email: 'a@example.com', reason: 'captcha', how: 'Solve the CAPTCHA.', since: null },
        ],
      },
    });
    expect(fresh).toContain('Claude Code sign-ins renew by themselves (runner on mac');
    expect(fresh).toContain('- **NEEDS YOU · a@example.com**: Solve the CAPTCHA.');
    expect(fresh).toContain('(a@example.com): healthy. sign-in ends');
    expect(fresh).toContain('renews by itself from');
    const stale = renderSnapshotMarkdown({
      ...base,
      signInRenewal: {
        runner: 'mac',
        generatedAt: new Date(NOW - 7200_000).toISOString(),
        stale: true,
        needsYou: [],
      },
    });
    expect(stale).toContain('renewals are not happening until it runs again');
  });
});
