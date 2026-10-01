import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

let tempRoot = '';
let ccsDir = '';
let claudeDir = '';
let codexDir = '';
let aggregator: typeof import('../../../src/web-server/usage/aggregator');
let originalCcsDir: string | undefined;
let originalClaudeConfigDir: string | undefined;
let originalCcsHome: string | undefined;
let originalCodexHome: string | undefined;

function writeClaudeJsonlFixture(): void {
  const projectDir = path.join(claudeDir, 'projects', 'project-one');
  fs.mkdirSync(projectDir, { recursive: true });

  const line = JSON.stringify({
    type: 'assistant',
    sessionId: 'session-a',
    timestamp: '2026-03-02T10:00:00.000Z',
    version: '1.0.0',
    cwd: '/tmp/project',
    message: {
      model: 'claude-sonnet-4-5',
      usage: {
        input_tokens: 100,
        output_tokens: 40,
      },
    },
  });

  fs.writeFileSync(path.join(projectDir, 'usage.jsonl'), `${line}\n`, 'utf-8');
}

function writeCliproxySnapshotFixture(): void {
  const snapshotDir = path.join(ccsDir, 'cache', 'cliproxy-usage');
  fs.mkdirSync(snapshotDir, { recursive: true });

  const snapshot = {
    version: 3,
    timestamp: Date.now(),
    details: [
      {
        model: 'gemini-2.5-pro',
        timestamp: '2026-03-02T10:00:00.000Z',
        source: 'account-a',
        authIndex: '0',
        inputTokens: 50,
        outputTokens: 10,
        cacheReadTokens: 5,
        failed: false,
      },
    ],
    daily: [
      {
        date: '2026-03-02',
        source: 'cliproxy',
        inputTokens: 50,
        outputTokens: 10,
        cacheCreationTokens: 0,
        cacheReadTokens: 5,
        cost: 0.2,
        totalCost: 0.2,
        modelsUsed: ['gemini-2.5-pro'],
        modelBreakdowns: [
          {
            modelName: 'gemini-2.5-pro',
            inputTokens: 50,
            outputTokens: 10,
            cacheCreationTokens: 0,
            cacheReadTokens: 5,
            cost: 0.2,
          },
        ],
      },
    ],
    hourly: [
      {
        hour: '2026-03-02 10:00',
        source: 'cliproxy',
        inputTokens: 50,
        outputTokens: 10,
        cacheCreationTokens: 0,
        cacheReadTokens: 5,
        cost: 0.2,
        totalCost: 0.2,
        modelsUsed: ['gemini-2.5-pro'],
        modelBreakdowns: [
          {
            modelName: 'gemini-2.5-pro',
            inputTokens: 50,
            outputTokens: 10,
            cacheCreationTokens: 0,
            cacheReadTokens: 5,
            cost: 0.2,
          },
        ],
      },
    ],
    monthly: [
      {
        month: '2026-03',
        source: 'cliproxy',
        inputTokens: 50,
        outputTokens: 10,
        cacheCreationTokens: 0,
        cacheReadTokens: 5,
        totalCost: 0.2,
        modelsUsed: ['gemini-2.5-pro'],
        modelBreakdowns: [
          {
            modelName: 'gemini-2.5-pro',
            inputTokens: 50,
            outputTokens: 10,
            cacheCreationTokens: 0,
            cacheReadTokens: 5,
            cost: 0.2,
          },
        ],
      },
    ],
  };

  fs.writeFileSync(path.join(snapshotDir, 'latest.json'), JSON.stringify(snapshot), 'utf-8');
}

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

  fs.mkdirSync(ccsDir, { recursive: true });
  fs.writeFileSync(path.join(ccsDir, 'config.yaml'), yaml, 'utf-8');
}

beforeEach(async () => {
  tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-usage-agg-'));
  ccsDir = path.join(tempRoot, '.ccs');
  claudeDir = path.join(tempRoot, '.claude');
  codexDir = path.join(tempRoot, '.codex');

  originalCcsDir = process.env.CCS_DIR;
  originalClaudeConfigDir = process.env.CLAUDE_CONFIG_DIR;
  originalCcsHome = process.env.CCS_HOME;
  originalCodexHome = process.env.CODEX_HOME;
  process.env.CCS_DIR = ccsDir;
  process.env.CCS_HOME = tempRoot;
  process.env.CLAUDE_CONFIG_DIR = claudeDir;
  process.env.CODEX_HOME = codexDir;

  writeUnifiedConfigFixture();
  writeClaudeJsonlFixture();
  writeCliproxySnapshotFixture();

  // Both registry pricing and proxy management reads use deterministic offline failures.
  spyOn(globalThis, 'fetch').mockRejectedValue(new Error('Network disabled in usage fixtures.'));
  aggregator = await import('../../../src/web-server/usage/aggregator');
  aggregator.shutdownUsageAggregator();
  aggregator.clearUsageCache();
});

afterEach(() => {
  aggregator.shutdownUsageAggregator();
  aggregator.clearUsageCache();

  mock.restore();
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

  if (originalCcsHome !== undefined) {
    process.env.CCS_HOME = originalCcsHome;
  } else {
    delete process.env.CCS_HOME;
  }

  if (originalCodexHome !== undefined) {
    process.env.CODEX_HOME = originalCodexHome;
  } else {
    delete process.env.CODEX_HOME;
  }

  fs.rmSync(tempRoot, { recursive: true, force: true });
});

describe('usage aggregator cliproxy integration', () => {
  it('merges cliproxy snapshot data into getCachedDailyData', async () => {
    const daily = await aggregator.getCachedDailyData();

    expect(daily).toHaveLength(1);
    expect(daily[0].date).toBe('2026-03-02');
    expect(daily[0].inputTokens).toBe(150);
    expect(daily[0].outputTokens).toBe(50);
    expect(daily[0].cacheReadTokens).toBe(5);
    expect(daily[0].modelsUsed).toContain('claude-sonnet-4-5');
    expect(daily[0].modelsUsed).toContain('gemini-2.5-pro');
  });

  it('clearUsageCache resets last fetch timestamp', async () => {
    await aggregator.getCachedDailyData();
    expect(aggregator.getLastFetchTimestamp()).not.toBeNull();

    aggregator.clearUsageCache();
    expect(aggregator.getLastFetchTimestamp()).toBeNull();
  });

  it('avoids double counting when CLAUDE_CONFIG_DIR points at a CCS instance', async () => {
    const instancePath = path.join(ccsDir, 'instances', 'work-profile');
    const instanceProjectDir = path.join(instancePath, 'projects', 'profile-one');
    fs.mkdirSync(instanceProjectDir, { recursive: true });

    const globalLine = JSON.stringify({
      type: 'assistant',
      sessionId: 'session-global',
      timestamp: '2026-03-02T10:00:00.000Z',
      cwd: '/tmp/global',
      message: {
        model: 'claude-sonnet-4-5',
        usage: {
          input_tokens: 10,
          output_tokens: 1,
        },
      },
    });
    fs.writeFileSync(
      path.join(claudeDir, 'projects', 'project-one', 'global.jsonl'),
      `${globalLine}\n`
    );

    const instanceLine = JSON.stringify({
      type: 'assistant',
      sessionId: 'session-instance',
      timestamp: '2026-03-02T11:00:00.000Z',
      cwd: '/tmp/instance',
      message: {
        model: 'gemini-2.5-pro',
        usage: {
          input_tokens: 100,
          output_tokens: 10,
        },
      },
    });
    fs.writeFileSync(path.join(instanceProjectDir, 'usage.jsonl'), `${instanceLine}\n`);

    process.env.CLAUDE_CONFIG_DIR = instancePath;
    aggregator.clearUsageCache();

    const daily = await aggregator.getCachedDailyData();

    expect(daily).toHaveLength(1);
    expect(daily[0].inputTokens).toBe(260);
    expect(daily[0].outputTokens).toBe(61);
    expect(daily[0].modelsUsed).toContain('claude-sonnet-4-5');
    expect(daily[0].modelsUsed).toContain('gemini-2.5-pro');
  });

  it('merges monthly model breakdowns when sources collide', () => {
    const result = aggregator.mergeMonthlyData([
      [
        {
          month: '2026-03',
          source: 'default',
          inputTokens: 10,
          outputTokens: 1,
          cacheCreationTokens: 0,
          cacheReadTokens: 0,
          totalCost: 1,
          modelsUsed: ['claude-sonnet-4-5'],
          modelBreakdowns: [
            {
              modelName: 'claude-sonnet-4-5',
              inputTokens: 10,
              outputTokens: 1,
              cacheCreationTokens: 0,
              cacheReadTokens: 0,
              cost: 1,
            },
          ],
        },
      ],
      [
        {
          month: '2026-03',
          source: 'instance',
          inputTokens: 20,
          outputTokens: 2,
          cacheCreationTokens: 5,
          cacheReadTokens: 3,
          totalCost: 2,
          modelsUsed: ['gemini-2.5-pro'],
          modelBreakdowns: [
            {
              modelName: 'gemini-2.5-pro',
              inputTokens: 20,
              outputTokens: 2,
              cacheCreationTokens: 5,
              cacheReadTokens: 3,
              cost: 2,
            },
          ],
        },
      ],
    ]);

    expect(result).toHaveLength(1);
    expect(result[0].inputTokens).toBe(30);
    expect(result[0].outputTokens).toBe(3);
    expect(result[0].cacheCreationTokens).toBe(5);
    expect(result[0].cacheReadTokens).toBe(3);
    expect(result[0].totalCost).toBe(3);
    expect(result[0].modelBreakdowns).toHaveLength(2);
  });

  it('coalesces legacy providerless breakdowns into the only known provider', () => {
    const result = aggregator.mergeDailyData([
      [
        {
          date: '2026-03-02',
          source: 'legacy-cache',
          inputTokens: 10,
          outputTokens: 1,
          cacheCreationTokens: 0,
          cacheReadTokens: 0,
          cost: 1,
          totalCost: 1,
          modelsUsed: ['gpt-5.5'],
          modelBreakdowns: [
            {
              modelName: 'gpt-5.5',
              inputTokens: 10,
              outputTokens: 1,
              cacheCreationTokens: 0,
              cacheReadTokens: 0,
              cost: 1,
            },
          ],
        },
      ],
      [
        {
          date: '2026-03-02',
          source: 'cliproxy',
          inputTokens: 20,
          outputTokens: 2,
          cacheCreationTokens: 0,
          cacheReadTokens: 0,
          cost: 2,
          totalCost: 2,
          modelsUsed: ['gpt-5.5'],
          modelBreakdowns: [
            {
              modelName: 'gpt-5.5',
              provider: 'openai',
              inputTokens: 20,
              outputTokens: 2,
              cacheCreationTokens: 0,
              cacheReadTokens: 0,
              cost: 2,
            },
          ],
        },
      ],
    ]);

    expect(result[0].modelBreakdowns).toHaveLength(1);
    expect(result[0].modelBreakdowns[0]).toMatchObject({
      modelName: 'gpt-5.5',
      provider: 'openai',
      inputTokens: 30,
      outputTokens: 3,
      cost: 3,
    });
    expect(result[0].modelsUsed).toEqual(['gpt-5.5']);
  });

  it('keeps legacy providerless breakdowns separate when providers are ambiguous', () => {
    const result = aggregator.mergeHourlyData([
      [
        {
          hour: '2026-03-02 10:00',
          source: 'legacy-cache',
          inputTokens: 10,
          outputTokens: 1,
          cacheCreationTokens: 0,
          cacheReadTokens: 0,
          cost: 1,
          totalCost: 1,
          modelsUsed: ['gpt-5.5'],
          modelBreakdowns: [
            {
              modelName: 'gpt-5.5',
              inputTokens: 10,
              outputTokens: 1,
              cacheCreationTokens: 0,
              cacheReadTokens: 0,
              cost: 1,
            },
          ],
        },
      ],
      [
        {
          hour: '2026-03-02 10:00',
          source: 'providers',
          inputTokens: 50,
          outputTokens: 5,
          cacheCreationTokens: 0,
          cacheReadTokens: 0,
          cost: 5,
          totalCost: 5,
          modelsUsed: ['openai/gpt-5.5', 'github-copilot/gpt-5.5'],
          modelBreakdowns: [
            {
              modelName: 'gpt-5.5',
              provider: 'openai',
              inputTokens: 20,
              outputTokens: 2,
              cacheCreationTokens: 0,
              cacheReadTokens: 0,
              cost: 2,
            },
            {
              modelName: 'gpt-5.5',
              provider: 'github-copilot',
              inputTokens: 30,
              outputTokens: 3,
              cacheCreationTokens: 0,
              cacheReadTokens: 0,
              cost: 3,
            },
          ],
        },
      ],
    ]);

    expect(result[0].modelBreakdowns).toHaveLength(3);
    expect(result[0].modelsUsed).toEqual(['gpt-5.5', 'openai/gpt-5.5', 'github-copilot/gpt-5.5']);
  });

  it('falls back to model cardinality when merging legacy hourly buckets without requestCount', () => {
    const result = aggregator.mergeHourlyData([
      [
        {
          hour: '2026-03-02 10:00',
          source: 'legacy-a',
          inputTokens: 10,
          outputTokens: 1,
          cacheCreationTokens: 0,
          cacheReadTokens: 0,
          cost: 1,
          totalCost: 1,
          modelsUsed: ['claude-sonnet-4-5'],
          modelBreakdowns: [
            {
              modelName: 'claude-sonnet-4-5',
              inputTokens: 10,
              outputTokens: 1,
              cacheCreationTokens: 0,
              cacheReadTokens: 0,
              cost: 1,
            },
          ],
        },
      ],
      [
        {
          hour: '2026-03-02 10:00',
          source: 'legacy-b',
          inputTokens: 20,
          outputTokens: 2,
          cacheCreationTokens: 0,
          cacheReadTokens: 0,
          cost: 2,
          totalCost: 2,
          modelsUsed: ['gemini-2.5-pro'],
          modelBreakdowns: [
            {
              modelName: 'gemini-2.5-pro',
              inputTokens: 20,
              outputTokens: 2,
              cacheCreationTokens: 0,
              cacheReadTokens: 0,
              cost: 2,
            },
          ],
        },
      ],
    ]);

    expect(result).toHaveLength(1);
    expect(result[0].requestCount).toBe(2);
  });
});
