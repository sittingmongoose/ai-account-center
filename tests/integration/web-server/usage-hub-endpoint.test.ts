/**
 * The T3 usage hub end to end over HTTP, against the real dashboard service
 * with a fake AAC cache: synthetic Codex and Claude accounts, a real key in a
 * temporary CCS_HOME, and counters on every collector to prove that T3's
 * requests never start a provider quota read.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import fs from 'fs';
import type { Server } from 'http';
import os from 'os';
import path from 'path';
import type { CodexAuthProfilesSummary } from '../../../src/codex-auth/codex-auth-dashboard-service';
import type { BarSummaryRow } from '../../../src/web-server/routes/bar-routes';
import {
  AccountDashboardService,
  type AccountDashboardDeps,
} from '../../../src/web-server/services/account-dashboard-service';
import type { ClaudeDesktopProfile } from '../../../src/web-server/services/claude-desktop-profile-service';
import type { ClaudeDesktopLiveUsage } from '../../../src/web-server/services/claude-desktop-live-service';
import { createUsageHubAccountSource } from '../../../src/web-server/usage-hub/usage-hub-accounts';
import {
  CLAUDE_USAGE_URL,
  CODEX_CREDITS_URL,
  CODEX_USAGE_URL,
} from '../../../src/web-server/usage-hub/usage-hub-contract';
import { writeUsageHubKey } from '../../../src/web-server/usage-hub/usage-hub-key-store';
import { createUsageHubRouter } from '../../../src/web-server/usage-hub/usage-hub-router';

const NOW = Date.parse('2026-10-06T12:30:00Z');
const SENTINEL = 'private-credential-sentinel-77';

const collectorCalls = { codexRows: 0, claudeHistory: 0, claudeLive: 0 };

function summary(): CodexAuthProfilesSummary {
  return {
    active: { name: 'alpha', source: 'default', codexHome: `/private/${SENTINEL}` },
    activated: { name: 'alpha', email: 'alpha@example.com', plan: 'pro', codexHome: SENTINEL },
    default: 'alpha',
    profiles: ['alpha', 'bravo', 'charlie'].map((name) => ({
      name,
      email: `${name}@example.com`,
      plan: name === 'bravo' ? 'plus' : 'pro',
      accountId: SENTINEL,
      codexHome: `/private/${SENTINEL}/${name}`,
      lastUsed: null,
      authValid: name !== 'charlie',
    })),
  };
}

function codexRow(profile: string): BarSummaryRow {
  return {
    profile,
    provider: 'codex',
    account_id: SENTINEL,
    displayName: profile,
    tier: 'pro',
    paused: false,
    quota_percentage: 70,
    quotaStatus: 'ok',
    next_reset: null,
    is_default: profile === 'alpha',
    last_activity_at: null,
    today_cost: null,
    health: 'ok',
    cached: false,
    fetchedAt: '2026-10-06T12:00:00Z',
    needsReauth: false,
    quotaSource: 'network',
    quotaWindows: [
      {
        key: 'five_hour',
        label: '5h',
        usedPercent: profile === 'alpha' ? 30 : 5,
        remainingPercent: profile === 'alpha' ? 70 : 95,
        resetAt: '2026-10-06T15:00:00Z',
        windowMinutes: 300,
      },
      {
        key: 'seven_day',
        label: 'week',
        usedPercent: 100,
        remainingPercent: 0,
        resetAt: '2026-10-09T21:32:12Z',
        windowMinutes: 10080,
      },
    ],
  };
}

const claudeProfiles: ClaudeDesktopProfile[] = ['delta', 'echo'].map((id) => ({
  id,
  email: `${id}@example.com`,
  windows: { launcherName: 'Windows', profilePath: `C:\\${SENTINEL}` },
}));

function live(profileId: string): ClaudeDesktopLiveUsage {
  return {
    profileId,
    email: `${profileId}@example.com`,
    platform: 'windows',
    source: 'Claude Desktop live quota on Windows',
    plan: 'max',
    fetchedAt: '2026-10-06T12:10:00Z',
    windows: [
      {
        key: 'five_hour',
        label: 'Five-hour usage',
        kind: 'rate_limit',
        usedPercent: 12,
        remainingPercent: 88,
        resetAt: '2026-10-06T16:00:00Z',
        windowMinutes: 300,
        used: null,
        limit: null,
        unit: null,
      },
      {
        key: 'seven_day',
        label: 'Weekly usage',
        kind: 'rate_limit',
        usedPercent: 44,
        remainingPercent: 56,
        resetAt: '2026-10-11T08:00:00Z',
        windowMinutes: 10080,
        used: null,
        limit: null,
        unit: null,
      },
      {
        key: 'seven_day_fable',
        label: 'Weekly Fable usage',
        kind: 'rate_limit',
        usedPercent: 61,
        remainingPercent: 39,
        resetAt: '2026-10-11T08:00:00Z',
        windowMinutes: 10080,
        used: null,
        limit: null,
        unit: null,
      },
    ],
  };
}

function deps(): AccountDashboardDeps {
  return {
    getCodexSummary: async () => summary(),
    getCodexRows: async (names) => {
      collectorCalls.codexRows += 1;
      return names.map(codexRow);
    },
    getCachedCodexRows: (names) => names.map(codexRow),
    listClaudeProfiles: async () => claudeProfiles,
    getClaudeUsage: async (platform) => {
      collectorCalls.claudeHistory += 1;
      return { platform, fetchedAt: '2026-10-06T12:00:00Z', profiles: [] };
    },
    getLiveClaudeUsage: async (profileId) => {
      collectorCalls.claudeLive += 1;
      return profileId === 'delta' ? live(profileId) : null;
    },
    getAdditionalAccounts: async () => [],
    getAutoSwitchStatus: () => ({
      enabled: false,
      thresholdPercent: 5,
      pollIntervalSeconds: 60,
      outcome: 'idle',
      message: 'Idle',
      activationInProgress: false,
    }),
    invalidateClaudeCache: () => {},
    scope: () => 'usage-hub-fixture-scope',
    refreshIntervalSeconds: () => 60,
    now: () => NOW,
    readVisibility: async () => ({
      state: 'ok',
      visibility: {
        hiddenProviders: [],
        hiddenAccountIds: [],
        trayHiddenProviders: [],
        trayHiddenAccountIds: [],
      },
    }),
  };
}

let tempHome = '';
let previous: Record<'CCS_HOME' | 'CCS_DIR', string | undefined> = {
  CCS_HOME: undefined,
  CCS_DIR: undefined,
};
let server: Server | undefined;
let baseUrl = '';
let service: AccountDashboardService;

async function startHub(): Promise<void> {
  const app = express();
  app.use(express.json());
  app.use(
    '/v0/management',
    createUsageHubRouter({
      accounts: createUsageHubAccountSource({ peek: (platform) => service.peek(platform) }),
    })
  );
  server = await new Promise<Server>((resolve) => {
    const instance = app.listen(0, '127.0.0.1', () => resolve(instance));
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('No test port');
  baseUrl = `http://127.0.0.1:${address.port}`;
}

beforeEach(() => {
  tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-usage-hub-int-'));
  previous = { CCS_HOME: process.env.CCS_HOME, CCS_DIR: process.env.CCS_DIR };
  delete process.env.CCS_DIR;
  process.env.CCS_HOME = tempHome;
  collectorCalls.codexRows = 0;
  collectorCalls.claudeHistory = 0;
  collectorCalls.claudeLive = 0;
  service = new AccountDashboardService(deps());
});

afterEach(async () => {
  if (server) await new Promise<void>((resolve) => server?.close(() => resolve()));
  server = undefined;
  for (const [name, value] of Object.entries(previous)) {
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
  fs.rmSync(tempHome, { recursive: true, force: true });
});

function management(pathname: string, key: string, body?: unknown): Promise<Response> {
  return fetch(`${baseUrl}/v0/management/${pathname}`, {
    method: body === undefined ? 'GET' : 'POST',
    headers: {
      authorization: `Bearer ${key}`,
      ...(body === undefined ? {} : { 'content-type': 'application/json' }),
    },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
}

describe('usage hub over a fake AAC cache', () => {
  it('serves the dashboard readings to T3 without ever starting a collection', async () => {
    const key = await writeUsageHubKey({ replace: false });
    // The dashboard's own refresh fills the cache for both desktops.
    await service.get('mac', false);
    await service.get('windows', false);
    const before = { ...collectorCalls };
    expect(before.codexRows).toBeGreaterThan(0);
    await startHub();

    const files = (await (await management('auth-files', key)).json()).files;
    expect(files.map((file: { id: string }) => file.id)).toEqual([
      'codex:alpha',
      'codex:bravo',
      'codex:charlie',
      'claude:delta',
      'claude:echo',
    ]);
    const byId = Object.fromEntries(
      files.map((file: { id: string; auth_index: string }) => [file.id, file])
    );
    expect(byId['codex:charlie'].status).toBe('error');
    expect(byId['claude:delta'].email).toBe('delta@example.com');

    const codex = await (
      await management('api-call', key, {
        auth_index: byId['codex:alpha'].auth_index,
        method: 'GET',
        url: CODEX_USAGE_URL,
        header: { Authorization: 'Bearer $TOKEN$' },
      })
    ).json();
    expect(codex.status_code).toBe(200);
    expect(JSON.parse(codex.body)).toEqual({
      plan_type: 'pro',
      rate_limit: {
        primary_window: {
          used_percent: 30,
          reset_at: Date.parse('2026-10-06T15:00:00Z') / 1000,
          limit_window_seconds: 18000,
        },
        secondary_window: {
          used_percent: 100,
          reset_at: Date.parse('2026-10-09T21:32:12Z') / 1000,
          limit_window_seconds: 604800,
        },
      },
    });

    const claude = await (
      await management('api-call', key, {
        auth_index: byId['claude:delta'].auth_index,
        method: 'GET',
        url: CLAUDE_USAGE_URL,
        header: { Authorization: 'Bearer $TOKEN$', 'anthropic-beta': 'oauth-2025-04-20' },
      })
    ).json();
    expect(claude.status_code).toBe(200);
    expect(JSON.parse(claude.body)).toEqual({
      five_hour: { utilization: 12, resets_at: '2026-10-06T16:00:00.000Z' },
      seven_day: { utilization: 44, resets_at: '2026-10-11T08:00:00.000Z' },
      limits: [
        {
          kind: 'weekly_scoped',
          percent: 61,
          resets_at: '2026-10-11T08:00:00.000Z',
          scope: { model: { display_name: 'Fable' } },
        },
      ],
    });

    const noReading = await (
      await management('api-call', key, {
        auth_index: byId['claude:echo'].auth_index,
        method: 'GET',
        url: CLAUDE_USAGE_URL,
      })
    ).json();
    expect(noReading.status_code).toBe(503);

    const credits = await management('api-call', key, {
      auth_index: byId['codex:alpha'].auth_index,
      method: 'GET',
      url: CODEX_CREDITS_URL,
    });
    expect(credits.status).toBe(403);

    expect(collectorCalls).toEqual(before);
    const everything = JSON.stringify([files, codex, claude, noReading]);
    expect(everything).not.toContain(SENTINEL);
    expect(everything).not.toContain('$TOKEN$');
  });

  it('has no rows before the first dashboard collection, and still starts none', async () => {
    const key = await writeUsageHubKey({ replace: false });
    await startHub();
    const files = (await (await management('auth-files', key)).json()).files;
    expect(files).toEqual([]);
    expect(collectorCalls).toEqual({ codexRows: 0, claudeHistory: 0, claudeLive: 0 });
  });

  it('stops answering the moment the key is rotated', async () => {
    const first = await writeUsageHubKey({ replace: false });
    await startHub();
    expect((await management('auth-files', first)).status).toBe(200);
    const second = await writeUsageHubKey({ replace: true });
    expect((await management('auth-files', first)).status).toBe(401);
    expect((await management('auth-files', second)).status).toBe(200);
  });
});
