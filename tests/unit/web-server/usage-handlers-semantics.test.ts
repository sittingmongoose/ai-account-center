import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

import { normalizeProfileQuery } from '../../../src/web-server/usage/profile-filter';

type AggregatorModule = typeof import('../../../src/web-server/usage/aggregator');

interface AssistantFixture {
  project: string;
  sessionId: string;
  timestamp: string;
  model: string;
  inputTokens?: number;
  outputTokens?: number;
  cacheCreationTokens?: number;
  cacheReadTokens?: number;
}

let tempHome = '';
let claudeDir = '';
let codexDir = '';
let aggregator: AggregatorModule;
let originalCcsHome: string | undefined;
let originalCcsDir: string | undefined;
let originalClaudeConfigDir: string | undefined;
let originalCodexHome: string | undefined;

function writeUnifiedConfigFixture(): void {
  const yaml = `version: 2
accounts: {}
profiles: {}
preferences:
  theme: system
  telemetry: false
  auto_update: true
cliproxy:
  oauth_accounts: {}
  providers:
    - gemini
    - codex
    - agy
  variants: {}
cliproxy_server:
  local:
    port: 65534
`;

  fs.mkdirSync(path.join(tempHome, '.ccs'), { recursive: true });
  fs.writeFileSync(path.join(tempHome, '.ccs', 'config.yaml'), yaml, 'utf-8');
}

function writeAssistantEntries(entries: AssistantFixture[]): void {
  writeAssistantEntriesToDir(claudeDir, entries);
}

function writeAssistantEntriesToDir(baseClaudeDir: string, entries: AssistantFixture[]): void {
  for (const entry of entries) {
    const projectDir = path.join(baseClaudeDir, 'projects', entry.project);
    fs.mkdirSync(projectDir, { recursive: true });

    const line = JSON.stringify({
      type: 'assistant',
      sessionId: entry.sessionId,
      timestamp: entry.timestamp,
      version: '1.0.0',
      cwd: `/tmp/${entry.project}`,
      message: {
        model: entry.model,
        usage: {
          input_tokens: entry.inputTokens ?? 0,
          output_tokens: entry.outputTokens ?? 0,
          cache_creation_input_tokens: entry.cacheCreationTokens ?? 0,
          cache_read_input_tokens: entry.cacheReadTokens ?? 0,
        },
      },
    });

    fs.writeFileSync(path.join(projectDir, `${entry.sessionId}.jsonl`), `${line}\n`, 'utf-8');
  }
}

beforeEach(async () => {
  tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-usage-aggregator-semantics-'));
  claudeDir = path.join(tempHome, '.claude');
  codexDir = path.join(tempHome, '.codex');

  originalCcsHome = process.env.CCS_HOME;
  originalCcsDir = process.env.CCS_DIR;
  originalClaudeConfigDir = process.env.CLAUDE_CONFIG_DIR;
  originalCodexHome = process.env.CODEX_HOME;
  process.env.CCS_HOME = tempHome;
  process.env.CCS_DIR = path.join(tempHome, '.ccs');
  process.env.CLAUDE_CONFIG_DIR = claudeDir;
  process.env.CODEX_HOME = codexDir;

  writeUnifiedConfigFixture();

  // Pricing refresh and optional proxy reads must never leave these fixtures.
  spyOn(globalThis, 'fetch').mockRejectedValue(new Error('Network disabled in usage fixtures.'));
  aggregator = await import('../../../src/web-server/usage/aggregator');
  aggregator.shutdownUsageAggregator();
  aggregator.clearUsageCache();
});

afterEach(() => {
  aggregator.shutdownUsageAggregator();
  aggregator.clearUsageCache();

  if (originalCcsHome !== undefined) {
    process.env.CCS_HOME = originalCcsHome;
  } else {
    delete process.env.CCS_HOME;
  }

  if (originalCcsDir !== undefined) {
    process.env.CCS_DIR = originalCcsDir;
  } else {
    delete process.env.CCS_DIR;
  }

  if (originalClaudeConfigDir !== undefined) {
    process.env.CLAUDE_CONFIG_DIR = originalClaudeConfigDir;
  } else {
    delete process.env.CLAUDE_CONFIG_DIR;
  }

  if (originalCodexHome !== undefined) {
    process.env.CODEX_HOME = originalCodexHome;
  } else {
    delete process.env.CODEX_HOME;
  }

  mock.restore();
  fs.rmSync(tempHome, { recursive: true, force: true });
});

describe('retained usage aggregator semantics', () => {
  it('preserves every token category in daily aggregates', async () => {
    writeAssistantEntries([
      {
        project: 'project-one',
        sessionId: 'session-a',
        timestamp: '2026-03-02T10:00:00.000Z',
        model: 'claude-sonnet-4-5',
        inputTokens: 1_000_000,
        outputTokens: 100_000,
        cacheCreationTokens: 100_000,
        cacheReadTokens: 200_000,
      },
    ]);

    const daily = await aggregator.getCachedDailyData();
    expect(daily).toHaveLength(1);
    expect(daily[0]).toMatchObject({
      date: '2026-03-02',
      inputTokens: 1_000_000,
      outputTokens: 100_000,
      cacheCreationTokens: 100_000,
      cacheReadTokens: 200_000,
      modelBreakdowns: [
        expect.objectContaining({
          modelName: 'claude-sonnet-4-5',
          inputTokens: 1_000_000,
          outputTokens: 100_000,
          cacheCreationTokens: 100_000,
          cacheReadTokens: 200_000,
        }),
      ],
    });
  });

  it('counts hourly requests from raw entries instead of distinct models', async () => {
    writeAssistantEntries([
      {
        project: 'project-one',
        sessionId: 'session-a',
        timestamp: '2026-03-02T10:05:00.000Z',
        model: 'claude-sonnet-4-5',
        inputTokens: 100,
        outputTokens: 10,
      },
      {
        project: 'project-two',
        sessionId: 'session-b',
        timestamp: '2026-03-02T10:15:00.000Z',
        model: 'claude-sonnet-4-5',
        inputTokens: 120,
        outputTokens: 15,
      },
      {
        project: 'project-three',
        sessionId: 'session-c',
        timestamp: '2026-03-02T10:30:00.000Z',
        model: 'gemini-2.5-pro',
        inputTokens: 80,
        outputTokens: 20,
      },
    ]);

    const hourly = await aggregator.getCachedHourlyData();
    const targetHour = hourly.find((row) => row.hour === '2026-03-02 10:00');
    expect(targetHour).toMatchObject({ requestCount: 3, inputTokens: 300, outputTokens: 45 });
    expect(targetHour?.modelBreakdowns).toHaveLength(2);
  });

  it('retains warmed data until explicit cache invalidation reloads the source', async () => {
    const entry: AssistantFixture = {
      project: 'project-one',
      sessionId: 'session-a',
      timestamp: '2026-03-02T10:00:00.000Z',
      model: 'claude-sonnet-4-5',
      inputTokens: 100,
      outputTokens: 10,
    };
    writeAssistantEntries([entry]);
    expect(aggregator.getUsageCacheSize()).toBe(0);
    expect(aggregator.getLastFetchTimestamp()).toBeNull();

    const warmed = await aggregator.getCachedDailyData();
    const fetchedAt = aggregator.getLastFetchTimestamp();
    expect(fetchedAt).not.toBeNull();
    expect(aggregator.getUsageCacheSize()).toBeGreaterThan(0);
    expect(warmed[0].inputTokens).toBe(100);

    writeAssistantEntries([{ ...entry, inputTokens: 200 }]);
    expect(await aggregator.getCachedDailyData()).toEqual(warmed);
    expect(aggregator.getLastFetchTimestamp()).toBe(fetchedAt);

    aggregator.clearUsageCache();
    expect(aggregator.getUsageCacheSize()).toBe(0);
    expect(aggregator.getLastFetchTimestamp()).toBeNull();
    const reloaded = await aggregator.getCachedDailyData();
    expect(reloaded[0].inputTokens).toBe(200);
    expect(reloaded[0].outputTokens).toBe(10);
  });

  it('preserves cache-only model activity in daily model breakdowns', async () => {
    writeAssistantEntries([
      {
        project: 'project-one',
        sessionId: 'session-a',
        timestamp: '2026-03-02T10:00:00.000Z',
        model: 'claude-sonnet-4-5',
        inputTokens: 100,
      },
      {
        project: 'project-two',
        sessionId: 'session-b',
        timestamp: '2026-03-02T11:00:00.000Z',
        model: 'gemini-2.5-pro',
        cacheReadTokens: 100,
      },
    ]);

    const daily = await aggregator.getCachedDailyData();
    expect(daily).toHaveLength(1);
    expect(daily[0].inputTokens).toBe(100);
    expect(daily[0].cacheReadTokens).toBe(100);
    expect(daily[0].modelsUsed).toContain('gemini-2.5-pro');
    expect(daily[0].modelBreakdowns).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ modelName: 'claude-sonnet-4-5', inputTokens: 100 }),
        expect.objectContaining({
          modelName: 'gemini-2.5-pro',
          inputTokens: 0,
          cacheReadTokens: 100,
        }),
      ])
    );
  });

  it('isolates daily profile reads without changing the shared all-profile cache', async () => {
    writeAssistantEntries([
      {
        project: 'default-project',
        sessionId: 'session-default',
        timestamp: '2026-03-02T10:00:00.000Z',
        model: 'claude-sonnet-4-5',
        inputTokens: 100,
        outputTokens: 10,
      },
    ]);
    writeAssistantEntriesToDir(path.join(tempHome, '.ccs', 'instances', 'work'), [
      {
        project: 'work-project',
        sessionId: 'session-work',
        timestamp: '2026-03-02T11:00:00.000Z',
        model: 'claude-sonnet-4-5',
        inputTokens: 300,
        outputTokens: 30,
      },
    ]);

    const allProfiles = await aggregator.getCachedDailyData();
    expect(allProfiles).toHaveLength(1);
    expect(allProfiles[0]).toMatchObject({ inputTokens: 400, outputTokens: 40 });
    expect(await aggregator.getCachedDailyData('work')).toEqual([
      expect.objectContaining({ inputTokens: 300, outputTokens: 30 }),
    ]);
    expect(await aggregator.getCachedDailyData('default')).toEqual([
      expect.objectContaining({ inputTokens: 100, outputTokens: 10 }),
    ]);
    expect(await aggregator.getCachedDailyData()).toEqual(allProfiles);
  });

  it('isolates default and account sessions while retaining the combined inventory', async () => {
    writeAssistantEntries([
      {
        project: 'default-project',
        sessionId: 'session-default',
        timestamp: '2026-03-02T10:00:00.000Z',
        model: 'claude-sonnet-4-5',
        inputTokens: 100,
        outputTokens: 10,
      },
    ]);
    writeAssistantEntriesToDir(path.join(tempHome, '.ccs', 'instances', 'work'), [
      {
        project: 'work-project',
        sessionId: 'session-work',
        timestamp: '2026-03-02T11:00:00.000Z',
        model: 'claude-sonnet-4-5',
        inputTokens: 300,
        outputTokens: 30,
      },
    ]);

    expect(await aggregator.getCachedSessionData('default')).toEqual([
      expect.objectContaining({ sessionId: 'session-default', profile: 'default' }),
    ]);
    expect(await aggregator.getCachedSessionData('work')).toEqual([
      expect.objectContaining({ sessionId: 'session-work', profile: 'work' }),
    ]);
    expect(await aggregator.getCachedSessionData()).toHaveLength(2);
  });

  it('preserves shared profile-filter rejection of non-string query values', () => {
    for (const profile of [['work', 'default'], { name: 'work' }]) {
      expect(() => normalizeProfileQuery(profile)).toThrow('Invalid profile filter');
    }
  });
});
