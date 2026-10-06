import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  aggregateSessionAggregates,
  codexReaderCount,
  collectAccountActivity,
  collectCodexPartition,
  partitionCodexFiles,
  type CollectAccountActivityDeps,
} from '../../../src/web-server/usage/account-activity-collector';
import {
  CodexPartitionError,
  spawnCodexPartitionReader,
} from '../../../src/web-server/usage/worker-client';
import { runWithScopedCcsHome } from '../../../src/utils/config-manager';
import { setCachedModelsDevRegistry } from '../../../src/web-server/models-dev/registry-cache';
import { scanCodexNativeUsageEntries } from '../../../src/web-server/usage/codex-native-usage-collector';
import { aggregateHourlyUsage } from '../../../src/web-server/usage/data-aggregator';

const NOW = Date.parse('2026-10-01T16:30:00Z');
let root: string;
let file: string;
function meta(provider = 'openai') {
  return { type: 'session_meta', payload: { id: 'native-session', model_provider: provider } };
}
function model(name = 'gpt-5.4') {
  return { type: 'turn_context', payload: { model: name } };
}
function tokens(input: number, cached: number, output: number, timestamp = '2026-10-01T15:05:00Z') {
  return {
    timestamp,
    type: 'event_msg',
    payload: {
      type: 'token_count',
      info: {
        total_token_usage: {
          input_tokens: input,
          cached_input_tokens: cached,
          output_tokens: output,
          reasoning_output_tokens: Math.floor(output / 2),
        },
      },
    },
  };
}
function write(lines: unknown[]) {
  fs.writeFileSync(file, lines.map((line) => JSON.stringify(line)).join('\n') + '\n');
}
async function collect(maxBytesPerFile?: number) {
  return collectAccountActivity(
    { kind: 'codex', codexHome: path.join(root, 'codex'), cacheDir: path.join(root, 'cache') },
    { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, 'cache'), maxBytesPerFile }
  );
}
function sum(
  data: Awaited<ReturnType<typeof collect>>,
  field: 'inputTokens' | 'outputTokens' | 'cacheReadTokens'
) {
  return data.hourly.reduce((total, hour) => total + hour[field], 0);
}

describe('bounded native account activity checkpoints', () => {
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-account-activity-'));
    fs.mkdirSync(path.join(root, 'codex', 'sessions'), { recursive: true });
    file = path.join(root, 'codex', 'sessions', 'rollout-native.jsonl');
  });
  afterEach(() => fs.rmSync(root, { recursive: true, force: true }));

  it('resumes cumulative usage across bounded byte batches without duplicate events or reasoning output', async () => {
    write([
      meta(),
      model(),
      tokens(100, 20, 10),
      tokens(100, 20, 10),
      model('gpt-5.3-codex'),
      tokens(150, 30, 20),
      tokens(180, 40, 25),
    ]);
    let data = await collect(450);
    expect(data.scan?.complete).toBe(false);
    let readBytes = data.scan?.readBytes ?? 0;
    for (let attempt = 0; attempt < 12 && !data.scan?.complete; attempt++) {
      data = await collect(450);
      readBytes += data.scan?.readBytes ?? 0;
    }
    expect(data.scan?.complete).toBe(true);
    expect(readBytes).toBe(fs.statSync(file).size);
    expect(data.eventCount).toBe(3);
    expect(data.hourly[0].requestCount).toBe(3);
    expect(sum(data, 'inputTokens')).toBe(140);
    expect(sum(data, 'cacheReadTokens')).toBe(40);
    expect(sum(data, 'outputTokens')).toBe(25);
    expect(data.hourly[0].modelsUsed.sort()).toEqual(['gpt-5.3-codex', 'gpt-5.4']);
    const warm = await collect();
    expect(warm.scan?.readBytes).toBe(0);
    expect(warm.hourly).toEqual(data.hourly);
    expect(warm.session).toEqual(data.session);
  });

  it('caps deep traversal while retaining measured root usage as partial', async () => {
    write([meta(), model(), tokens(100, 20, 5)]);
    let directory = path.dirname(file);
    for (let index = 0; index < 66; index++) {
      directory = path.join(directory, `deep-${index}`);
      fs.mkdirSync(directory);
    }
    fs.writeFileSync(
      path.join(directory, 'rollout-beyond-depth.jsonl'),
      JSON.stringify(tokens(999999, 0, 1)) + '\n'
    );
    const result = await collect();
    expect(sum(result, 'inputTokens')).toBe(80);
    expect(result.scan?.complete).toBe(false);
    expect(result.scan?.failedFiles).toBeGreaterThan(0);
    expect(result.scan?.totalFiles).toBe(1);
  });

  it.each([
    { limits: { maxDirectories: 4 }, irrelevant: 'directories' },
    { limits: { maxEntries: 8 }, irrelevant: 'files' },
  ])(
    'caps irrelevant $irrelevant before parsing and retains only measured usage',
    async ({ limits, irrelevant }) => {
      write([meta(), model(), tokens(100, 20, 5)]);
      for (let index = 0; index < 16; index++) {
        const entry = path.join(path.dirname(file), `irrelevant-${index}`);
        if (irrelevant === 'directories') fs.mkdirSync(entry);
        else fs.writeFileSync(entry, 'not a native usage file');
      }
      const pending = collectAccountActivity(
        { kind: 'codex', codexHome: path.join(root, 'codex'), cacheDir: path.join(root, 'cache') },
        {
          minDate: NOW - 31 * 86400000,
          cacheDir: path.join(root, 'cache'),
          traversalLimits: limits,
        }
      );
      if (irrelevant === 'directories') {
        const result = await pending;
        expect(sum(result, 'inputTokens')).toBe(80);
        expect(result.scan?.complete).toBe(false);
        expect(result.scan?.failedFiles).toBeGreaterThan(0);
      } else {
        // Enumeration order is unspecified. A cap that hides every native
        // file must report unavailable, rather than a measured zero.
        await pending.then(
          (result) => {
            expect(sum(result, 'inputTokens')).toBe(80);
            expect(result.scan?.complete).toBe(false);
            expect(result.scan?.failedFiles).toBeGreaterThan(0);
          },
          (error: Error) => {
            expect(error.message).toBe('Native log sources are unavailable');
          }
        );
      }
    }
  );

  it('honors an expired overall budget before walking history and invents no measured zero', async () => {
    write([meta(), model(), tokens(100, 20, 5)]);
    let time = NOW;
    const clock = spyOn(Date, 'now').mockImplementation(() => {
      time += 10;
      return time;
    });
    try {
      await expect(
        collectAccountActivity(
          {
            kind: 'codex',
            codexHome: path.join(root, 'codex'),
            cacheDir: path.join(root, 'cache'),
          },
          { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, 'cache'), budgetMs: 1 }
        )
      ).rejects.toThrow('Native log sources are unavailable');
    } finally {
      clock.mockRestore();
    }
  });

  it('reads appends and unfinished tails once, then invalidates truncate/rotate/rewrite fingerprints', async () => {
    write([meta(), model(), tokens(100, 20, 10)]);
    expect(sum(await collect(), 'inputTokens')).toBe(80);
    fs.appendFileSync(file, JSON.stringify(tokens(150, 30, 20)).slice(0, 75));
    const tail = await collect();
    expect(tail.scan?.complete).toBe(false);
    expect(tail.eventCount).toBe(1);
    fs.appendFileSync(file, JSON.stringify(tokens(150, 30, 20)).slice(75) + '\n');
    const appended = await collect();
    expect(appended.scan?.complete).toBe(true);
    expect(appended.eventCount).toBe(2);
    expect(sum(appended, 'inputTokens')).toBe(120);
    write([meta(), model(), tokens(50, 10, 4)]);
    expect(sum(await collect(), 'inputTokens')).toBe(40);
    fs.renameSync(file, path.join(root, 'retired.jsonl'));
    write([meta(), model(), tokens(90, 20, 6)]);
    expect(sum(await collect(), 'inputTokens')).toBe(70);
    const oldSize = fs.statSync(file).size;
    write([meta(), model(), tokens(80, 10, 5)]);
    expect(fs.statSync(file).size).toBe(oldSize);
    expect(sum(await collect(), 'inputTokens')).toBe(70);
    expect(sum(await collect(), 'outputTokens')).toBe(5);
  });

  it('retains pre-cutoff counters but reports only actual UTC events after the cutoff', async () => {
    write([
      meta(),
      model(),
      tokens(100, 20, 10, '2026-08-01T15:00:00Z'),
      tokens(150, 30, 20, '2026-10-01T17:05:00+02:00'),
    ]);
    const data = await collect();
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(1);
    expect(data.hourly.map((hour) => hour.hour)).toEqual(['2026-10-01 15:00']);
    expect(sum(data, 'inputTokens')).toBe(40);
    expect(sum(data, 'cacheReadTokens')).toBe(10);
    expect(sum(data, 'outputTokens')).toBe(10);
    expect(data.session[0].lastActivity).toBe('2026-10-01T15:05:00.000Z');
  });

  it('reports when each session was first and last active in the retained rows', async () => {
    write([
      meta(),
      model(),
      tokens(100, 20, 10, '2026-10-01T13:10:00Z'),
      tokens(150, 30, 20, '2026-10-01T15:05:00Z'),
    ]);
    const data = await collect();
    expect(data.session).toHaveLength(1);
    expect(data.session[0].firstActivity).toBe('2026-10-01T13:10:00.000Z');
    expect(data.session[0].lastActivity).toBe('2026-10-01T15:05:00.000Z');
  });

  it('ignores raw legacy caches and excludes CLIProxy token counts even when they dwarf native data', async () => {
    write([meta(), model(), tokens(100, 20, 5)]);
    fs.mkdirSync(path.join(root, 'cache'), { recursive: true });
    fs.writeFileSync(
      path.join(root, 'cache', 'codex-native-usage-v1.json'),
      '{"version":1,"files":{"wrong":{"entries":[{"inputTokens":999999999}]}}}'
    );
    fs.writeFileSync(
      path.join(root, 'codex', 'sessions', 'rollout-proxy.jsonl'),
      [meta('cliproxy'), model(), tokens(999999999, 0, 10)]
        .map((line) => JSON.stringify(line))
        .join('\n') + '\n'
    );
    const data = await collect();
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(1);
    expect(sum(data, 'inputTokens')).toBe(80);
    expect(sum(data, 'outputTokens')).toBe(5);
  });

  it('matches existing native pricing and model semantics while resolving scoped rates once per model', async () => {
    write([
      meta(),
      model(),
      tokens(100, 20, 10),
      tokens(150, 30, 20),
      model('gpt-5.3-codex'),
      tokens(180, 40, 25),
    ]);
    const result = await runWithScopedCcsHome(root, async () => {
      setCachedModelsDevRegistry({
        openai: {
          id: 'openai',
          models: {
            'gpt-5.4': {
              id: 'gpt-5.4',
              cost: { input: 10, output: 20, cache_read: 5, cache_write: 2 },
            },
            'gpt-5.3-codex': {
              id: 'gpt-5.3-codex',
              cost: { input: 15, output: 25, cache_read: 3, cache_write: 4 },
            },
          },
        },
      });
      const entries = await scanCodexNativeUsageEntries({
        env: { CODEX_HOME: path.join(root, 'codex') },
        disableCache: true,
      });
      return { bounded: await collect(), legacy: aggregateHourlyUsage(entries, 'codex-native') };
    });
    expect(result.bounded.hourly).toEqual(result.legacy);
    expect(result.bounded.hourly[0].totalCost).toBeCloseTo(
      (120 * 10 + 20 * 20 + 30 * 5 + 20 * 15 + 5 * 25 + 10 * 3) / 1e6,
      12
    );
    expect(result.bounded.hourly[0].requestCount).toBe(3);
  });

  it('bounds oversized lines, resumes discarding, and keeps subsequent actual usage with an explicit partial result', async () => {
    const fd = fs.openSync(file, 'w');
    fs.writeSync(fd, JSON.stringify(meta()) + '\n' + JSON.stringify(model()) + '\n');
    fs.writeSync(fd, '{"type":"unknown_native_record","text":"');
    const padding = Buffer.alloc(1024 * 1024, 32);
    for (let index = 0; index < 12; index++) fs.writeSync(fd, padding);
    fs.writeSync(fd, '"}\n' + JSON.stringify(tokens(100, 20, 10)) + '\n');
    fs.closeSync(fd);
    let data = await collect(9 * 1024 * 1024);
    expect(data.scan?.complete).toBe(false);
    data = await collect(9 * 1024 * 1024);
    expect(data.scan?.completedFiles).toBe(1);
    expect(data.scan?.skippedLines).toBe(1);
    expect(data.scan?.complete).toBe(false);
    expect(data.eventCount).toBe(1);
    expect(sum(data, 'inputTokens')).toBe(80);
  });

  it('collects Claude assistant usage including native subagents without parsing large conversation bodies', async () => {
    const projects = path.join(root, 'claude', 'projects', 'project', 'subagents');
    fs.mkdirSync(projects, { recursive: true });
    const usage = {
      type: 'assistant',
      sessionId: 'claude-session',
      timestamp: '2026-10-01T15:00:00Z',
      message: {
        model: 'claude-sonnet-4-6',
        usage: {
          input_tokens: 100,
          output_tokens: 40,
          cache_read_input_tokens: 20,
          cache_creation_input_tokens: 5,
        },
      },
    };
    fs.writeFileSync(
      path.join(projects, 'agent.jsonl'),
      JSON.stringify({ type: 'user', message: { content: 'x'.repeat(2 * 1024 * 1024) } }) +
        '\n' +
        JSON.stringify(usage) +
        '\n'
    );
    const data = await collectAccountActivity(
      { kind: 'claude', projectsDir: path.join(root, 'claude', 'projects') },
      { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, 'cache') }
    );
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(1);
    expect(data.hourly[0]).toMatchObject({
      source: 'custom-parser',
      inputTokens: 100,
      outputTokens: 40,
      cacheCreationTokens: 5,
      cacheReadTokens: 20,
      requestCount: 1,
    });
    const cacheFiles: string[] = [];
    const walk = (directory: string) => {
      for (const item of fs.readdirSync(directory, { withFileTypes: true })) {
        const full = path.join(directory, item.name);
        if (item.isDirectory()) walk(full);
        else cacheFiles.push(full);
      }
    };
    walk(path.join(root, 'cache', 'account-activity-v1'));
    expect(cacheFiles.length).toBe(1);
    expect(fs.statSync(cacheFiles[0]).size).toBeLessThan(2048);
    if (process.platform !== 'win32') expect(fs.statSync(cacheFiles[0]).mode & 0o777).toBe(0o600);
    expect(fs.readFileSync(cacheFiles[0], 'utf8')).not.toContain('xxxx');
  });
});

describe('claude response collapsing', () => {
  let seq = 0;
  function line(mid: string | null, req: string | null, output: number) {
    seq++;
    return {
      type: 'assistant',
      uuid: `uuid-${seq}`,
      ...(req === null ? {} : { requestId: req }),
      sessionId: 's1',
      timestamp: '2026-10-01T15:00:00Z',
      message: {
        ...(mid === null ? {} : { id: mid }),
        model: 'claude-sonnet-4-6',
        usage: {
          input_tokens: 100,
          output_tokens: output,
          cache_read_input_tokens: 20,
          cache_creation_input_tokens: 5,
        },
      },
    };
  }
  function writeClaude(lines: unknown[]) {
    const dir = path.join(root, 'claude', 'projects', 'p');
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(
      path.join(dir, 's.jsonl'),
      lines.map((l) => JSON.stringify(l)).join('\n') + '\n'
    );
  }
  async function collectClaude(maxBytesPerFile?: number) {
    return collectAccountActivity(
      { kind: 'claude', projectsDir: path.join(root, 'claude', 'projects') },
      { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, 'cache'), maxBytesPerFile }
    );
  }
  beforeEach(() => {
    seq = 0;
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-claude-collapse-'));
  });
  afterEach(() => fs.rmSync(root, { recursive: true, force: true }));

  it('counts one multi-line API response once, keeping the last usage', async () => {
    writeClaude([
      line('m1', 'r1', 10),
      line('m1', 'r1', 20),
      line('m1', 'r1', 30),
      line('m2', 'r2', 7),
    ]);
    const data = await collectClaude();
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(2);
    expect(sum(data, 'outputTokens')).toBe(37);
  });

  it('counts lines without a message id solo', async () => {
    writeClaude([line(null, 'r1', 10), line(null, 'r1', 20)]);
    const data = await collectClaude();
    expect(data.eventCount).toBe(2);
    expect(sum(data, 'outputTokens')).toBe(30);
  });

  it('collapses a response split across bounded byte batches without double-counting', async () => {
    writeClaude([line('m1', 'r1', 10), line('m1', 'r1', 20), line('m1', 'r1', 30)]);
    let data = await collectClaude(350);
    for (let attempt = 0; attempt < 6 && !data.scan?.complete; attempt++) {
      data = await collectClaude(350);
    }
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(1);
    expect(sum(data, 'outputTokens')).toBe(30);
    const warm = await collectClaude();
    expect(warm.scan?.readBytes).toBe(0);
    expect(warm.eventCount).toBe(1);
  });

  it('keeps a response being written pending across scans instead of double-counting', async () => {
    // R2-1: a scan landing mid-response must not commit the half-written
    // response at EOF; the next scan continues the same response. Committing
    // early counted block 1, then block 2 as a second response (about +8%
    // Claude cache-read at a 60 s refresh, creeping while the server runs).
    writeClaude([line('m1', 'r1', 50)]);
    let data = await collectClaude();
    expect(data.eventCount).toBe(1);
    expect(sum(data, 'outputTokens')).toBe(50);
    fs.appendFileSync(
      path.join(root, 'claude', 'projects', 'p', 's.jsonl'),
      JSON.stringify(line('m1', 'r1', 55)) + '\n'
    );
    data = await collectClaude();
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(1);
    expect(sum(data, 'outputTokens')).toBe(55);
    // A cold re-read agrees: one response, counted once.
    const cold = await collectAccountActivity(
      { kind: 'claude', projectsDir: path.join(root, 'claude', 'projects') },
      { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, 'cache-cold') }
    );
    expect(cold.eventCount).toBe(1);
    expect(sum(cold, 'outputTokens')).toBe(55);
  });
});

describe('pre-aggregated session rows', () => {
  it('groups helper rows into sessions with priced model breakdowns', () => {
    const a = Date.parse('2026-10-01T15:05:00Z');
    const z = Date.parse('2026-10-01T16:35:00Z');
    const { session } = aggregateSessionAggregates(
      [
        {
          sessionId: 's1',
          model: 'm-a',
          target: 'omp',
          firstMs: a,
          lastMs: z,
          inputTokens: 100,
          outputTokens: 10,
          cacheCreationTokens: 0,
          cacheReadTokens: 0,
          events: 2,
        },
        {
          sessionId: 's1',
          model: 'm-b',
          provider: 'qwen',
          target: 'omp',
          firstMs: a,
          lastMs: a,
          inputTokens: 50,
          outputTokens: 5,
          cacheCreationTokens: 0,
          cacheReadTokens: 0,
          cost: 0.25,
          events: 1,
        },
        {
          sessionId: 's2',
          model: 'm-a',
          target: 'omp',
          firstMs: z + 60_000,
          lastMs: z + 60_000,
          inputTokens: 10,
          outputTokens: 1,
          cacheCreationTokens: 0,
          cacheReadTokens: 0,
          events: 1,
        },
      ],
      'omp-remote'
    );
    expect(session).toHaveLength(2);
    // Most recent first; the whole retained span, not the last hour alone.
    expect(session[0].sessionId).toBe('s2');
    expect(session[1].sessionId).toBe('s1');
    expect(session[1].firstActivity).toBe('2026-10-01T15:05:00.000Z');
    expect(session[1].lastActivity).toBe('2026-10-01T16:35:00.000Z');
    expect(session[1].target).toBe('omp');
    expect(session[1].projectPath).toBe('');
    expect(session[1].inputTokens).toBe(150);
    const names = session[1].modelBreakdowns.map((item) => item.modelName).sort();
    expect(names).toEqual(['m-a', 'm-b']);
    // Logged cost wins for its own model; the rest prices at list rates.
    const logged = session[1].modelBreakdowns.find((item) => item.modelName === 'm-b');
    expect(logged?.cost).toBe(0.25);
    // The routing provider is normalized exactly as aggregateRows does.
    expect(logged?.provider).toBe('alibaba');
    expect(session[1].totalCost).toBeGreaterThanOrEqual(0.25);
  });

  it('drops rows without a session key or a valid span', () => {
    const at = Date.parse('2026-10-01T15:05:00Z');
    const base = {
      model: 'm-a',
      target: 'omp',
      inputTokens: 10,
      outputTokens: 1,
      cacheCreationTokens: 0,
      cacheReadTokens: 0,
      events: 1,
    };
    const { session } = aggregateSessionAggregates(
      [
        { ...base, sessionId: '', firstMs: at, lastMs: at },
        { ...base, sessionId: 'backwards', firstMs: at, lastMs: at - 1 },
        { ...base, sessionId: 'ok', firstMs: at, lastMs: at },
      ],
      'omp-remote'
    );
    expect(session.map((row) => row.sessionId)).toEqual(['ok']);
  });
});

describe('muse oversized-line completion', () => {
  let root: string;
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-muse-complete-'));
  });
  afterEach(() => fs.rmSync(root, { recursive: true, force: true }));

  function museUsage(model: string, input: number, cacheRead: number, output: number, at: number) {
    return {
      record_type: 'event',
      recorded_at: at,
      id: `rec-${at}-${input}`,
      payload: {
        event: {
          kind: 'model_completed',
          model,
          usage: {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cacheRead,
            cache_write_tokens: 0,
          },
        },
      },
    };
  }

  it('completes despite an oversized non-usage checkpoint line, counting the real usage', async () => {
    // Muse writes cumulative `context_projection_checkpoint` records that can be
    // enormous (retained context) and carry no countable usage; one such line
    // used to set skippedLines and pin scan.complete=false forever (N9), marking
    // the whole activity partial. The small `model_completed` events around it
    // are the real usage and must still be counted, and the scan must complete.
    const sessionsDir = path.join(root, 'muse', 'sessions');
    const sessDir = path.join(sessionsDir, '2026-10-01', 'sess-uuid');
    fs.mkdirSync(sessDir, { recursive: true });
    const at = Date.parse('2026-10-01T15:00:00Z');
    const file = path.join(sessDir, 'session.jsonl');
    const fd = fs.openSync(file, 'w');
    fs.writeSync(fd, JSON.stringify(museUsage('muse-spark-1.3', 100, 20, 10, at)) + '\n');
    // An oversized (>8 MiB) context_projection_checkpoint line: no usage record.
    fs.writeSync(
      fd,
      '{"record_type":"event","payload_type":"runtime.session","payload":{"event":{"kind":"context_projection_checkpoint","blob":"'
    );
    const padding = Buffer.alloc(1024 * 1024, 32);
    for (let index = 0; index < 10; index++) fs.writeSync(fd, padding);
    fs.writeSync(fd, '"}},"recorded_at":' + at + ',"id":"cp-1"}\n');
    fs.writeSync(fd, JSON.stringify(museUsage('muse-spark-1.3', 60, 10, 5, at + 60000)) + '\n');
    fs.closeSync(fd);
    const data = await collectAccountActivity(
      { kind: 'muse', sessionsDir },
      { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, 'cache') }
    );
    expect(data.scan?.skippedLines).toBe(0);
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(2);
    // input is netted of cache reads, as the muse parser does.
    expect(data.hourly.reduce((t, h) => t + h.inputTokens, 0)).toBe(80 + 50);
  });

  it('stays partial on an oversized children batch that may wrap usage, counting the rest', async () => {
    // Older muse lines wrap a `model_completed` record in
    // `children[].record_json`; such a batch can exceed MAX_LINE_BYTES, and
    // dropping it silently would lose usage while the scan claims complete.
    // The `children` key in the line prefix keeps the scan honestly partial
    // while the small usage lines around it still count.
    const sessionsDir = path.join(root, 'muse', 'sessions');
    const sessDir = path.join(sessionsDir, '2026-10-01', 'sess-uuid');
    fs.mkdirSync(sessDir, { recursive: true });
    const at = Date.parse('2026-10-01T15:00:00Z');
    const file = path.join(sessDir, 'session.jsonl');
    const fd = fs.openSync(file, 'w');
    fs.writeSync(fd, JSON.stringify(museUsage('muse-spark-1.3', 100, 20, 10, at)) + '\n');
    fs.writeSync(fd, '{"record_type":"event","children":[{"record_json":"{\\"pad\\":\\"');
    const padding = Buffer.alloc(1024 * 1024, 32);
    for (let index = 0; index < 10; index++) fs.writeSync(fd, padding);
    fs.writeSync(fd, '\\"}"}],"recorded_at":' + at + ',"id":"wrap-1"}\n');
    fs.writeSync(fd, JSON.stringify(museUsage('muse-spark-1.3', 60, 10, 5, at + 60000)) + '\n');
    fs.closeSync(fd);
    const data = await collectAccountActivity(
      { kind: 'muse', sessionsDir },
      { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, 'cache') }
    );
    expect(data.scan?.skippedLines).toBe(1);
    expect(data.scan?.complete).toBe(false);
    expect(data.eventCount).toBe(2);
    expect(data.hourly.reduce((t, h) => t + h.inputTokens, 0)).toBe(80 + 50);
  });
});

describe('mtime pre-filter skips files that cannot hold in-window events', () => {
  let root: string;
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-mtime-prefilter-'));
    fs.mkdirSync(path.join(root, 'codex', 'sessions'), { recursive: true });
  });
  afterEach(() => fs.rmSync(root, { recursive: true, force: true }));

  function rollout(name: string, lines: unknown[], mtime: Date) {
    const file = path.join(root, 'codex', 'sessions', name);
    fs.writeFileSync(file, lines.map((line) => JSON.stringify(line)).join('\n') + '\n');
    fs.utimesSync(file, mtime, mtime);
    return file;
  }
  function collect() {
    return collectAccountActivity(
      { kind: 'codex', codexHome: path.join(root, 'codex'), cacheDir: path.join(root, 'cache') },
      { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, 'cache') }
    );
  }
  function checkpoints(): string[] {
    const dir = path.join(root, 'cache', 'account-activity-v1');
    const found: string[] = [];
    if (!fs.existsSync(dir)) return found;
    const walk = (d: string) => {
      for (const item of fs.readdirSync(d, { withFileTypes: true })) {
        const full = path.join(d, item.name);
        if (item.isDirectory()) walk(full);
        else found.push(full);
      }
    };
    walk(dir);
    return found;
  }

  it('reads the recent file, skips the stale one, and keeps the in-window total', async () => {
    // A stale rollout (mtime before the window) holds only out-of-window events;
    // append-only logs cannot gain in-window records after their last write, so
    // it is skipped without being read. The recent rollout is read in full.
    rollout(
      'rollout-old.jsonl',
      [meta(), model(), tokens(999, 0, 999, '2026-08-01T15:00:00Z')],
      new Date(NOW - 40 * 86400000)
    );
    rollout(
      'rollout-recent.jsonl',
      [meta(), model(), tokens(100, 20, 10, '2026-10-01T15:05:00Z')],
      new Date(NOW)
    );
    const data = await collect();
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(1);
    expect(data.hourly.reduce((t, h) => t + h.inputTokens, 0)).toBe(80);
    // Only the recent file was read, so only it has a checkpoint.
    expect(checkpoints()).toHaveLength(1);
  });

  it('still reads a file written 12 h before the window (24 h margin)', async () => {
    // Timezone-naive timestamps and NAS-clocked mtimes can lag the window by
    // hours; the 24 h margin keeps such files readable while months-old logs
    // are still skipped. A 60 s margin would skip this file and lose the event.
    rollout(
      'rollout-lagging.jsonl',
      [meta(), model(), tokens(100, 20, 10, '2026-10-01T15:05:00Z')],
      new Date(NOW - 31 * 86400000 - 12 * 3600000)
    );
    const data = await collect();
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(1);
    expect(data.hourly.reduce((t, h) => t + h.inputTokens, 0)).toBe(80);
    expect(checkpoints()).toHaveLength(1);
  });

  it('reports a complete empty scan (not unavailable) when every file predates the window', async () => {
    // A kind whose logs all predate the window has no in-window usage; the mtime
    // pre-filter drops every file, and the scan must complete empty rather than
    // throw 'Native log sources are unavailable' (which would mark the source
    // failed). Regression: codex threw when a bounded traversal reached only
    // stale date dirs before its deadline.
    rollout(
      'rollout-old.jsonl',
      [meta(), model(), tokens(999, 0, 999, '2026-08-01T15:00:00Z')],
      new Date(NOW - 40 * 86400000)
    );
    const data = await collect();
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(0);
    expect(data.hourly).toHaveLength(0);
  });
});

describe('exact-duplicate session file dedup', () => {
  let root: string;
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-dedup-'));
  });
  afterEach(() => fs.rmSync(root, { recursive: true, force: true }));

  it('counts a byte-identical session file copied under two project dirs once', async () => {
    // A Claude subagent transcript written under two project dirs (a session
    // spanning two cwds) is the same usage twice; the collector must drop the
    // exact copy so tokens are not doubled.
    const line =
      JSON.stringify({
        type: 'assistant',
        sessionId: 'claude-dup',
        timestamp: '2026-10-01T15:00:00Z',
        message: {
          model: 'claude-sonnet-4-6',
          usage: {
            input_tokens: 100,
            output_tokens: 40,
            cache_read_input_tokens: 20,
            cache_creation_input_tokens: 5,
          },
        },
      }) + '\n';
    for (const proj of ['projA', 'projB']) {
      const dir = path.join(root, 'claude', 'projects', proj, 'sess', 'subagents');
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(path.join(dir, 'agent-same.jsonl'), line);
    }
    const data = await collectAccountActivity(
      { kind: 'claude', projectsDir: path.join(root, 'claude', 'projects') },
      { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, 'cache') }
    );
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(1);
    expect(data.hourly.reduce((t, h) => t + h.inputTokens, 0)).toBe(100);
  });

  it('counts two same-size files with different content (no false dedup)', async () => {
    // The exact-duplicate fingerprint is (size, head, tail): two files of the
    // same byte size but different records must both count. Same-length
    // session ids and token values keep the sizes equal.
    const lineFor = (sessionId: string, input: number) =>
      JSON.stringify({
        type: 'assistant',
        sessionId,
        timestamp: '2026-10-01T15:00:00Z',
        message: {
          model: 'claude-sonnet-4-6',
          usage: {
            input_tokens: input,
            output_tokens: 40,
            cache_read_input_tokens: 20,
            cache_creation_input_tokens: 5,
          },
        },
      }) + '\n';
    const lineA = lineFor('claude-neg-aa', 100);
    const lineB = lineFor('claude-neg-bb', 200);
    expect(lineB.length).toBe(lineA.length);
    expect(lineB).not.toBe(lineA);
    fs.mkdirSync(path.join(root, 'claude', 'projects', 'projA', 'sess'), { recursive: true });
    fs.mkdirSync(path.join(root, 'claude', 'projects', 'projB', 'sess'), { recursive: true });
    fs.writeFileSync(path.join(root, 'claude', 'projects', 'projA', 'sess', 'a.jsonl'), lineA);
    fs.writeFileSync(path.join(root, 'claude', 'projects', 'projB', 'sess', 'b.jsonl'), lineB);
    const data = await collectAccountActivity(
      { kind: 'claude', projectsDir: path.join(root, 'claude', 'projects') },
      { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, 'cache') }
    );
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(2);
    expect(data.hourly.reduce((t, h) => t + h.inputTokens, 0)).toBe(300);
  });
});

describe('codex partitioned reading (N14)', () => {
  let root: string;
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-codex-partition-'));
    fs.mkdirSync(path.join(root, 'codex', 'sessions'), { recursive: true });
  });
  afterEach(() => fs.rmSync(root, { recursive: true, force: true }));

  function rollout(name: string, lines: unknown[], mtimeMs = NOW) {
    const file = path.join(root, 'codex', 'sessions', name);
    fs.writeFileSync(file, lines.map((line) => JSON.stringify(line)).join('\n') + '\n');
    fs.utimesSync(file, new Date(mtimeMs), new Date(mtimeMs));
    return file;
  }
  function usageLines(session: string, input: number, output: number, hour: string) {
    return [
      { type: 'session_meta', payload: { id: session, model_provider: 'openai' } },
      { type: 'turn_context', payload: { model: 'gpt-5.4' } },
      tokens(input, 20, output, `2026-10-01T${hour}:05:00Z`),
    ];
  }
  function collect(
    cache: string,
    extra: Record<string, unknown> = {},
    deps?: CollectAccountActivityDeps
  ) {
    return collectAccountActivity(
      { kind: 'codex', codexHome: path.join(root, 'codex'), cacheDir: path.join(root, cache) },
      { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, cache), ...extra },
      deps
    );
  }
  function totals(data: Awaited<ReturnType<typeof collect>>) {
    return {
      events: data.eventCount,
      input: data.hourly.reduce((t, h) => t + h.inputTokens, 0),
      output: data.hourly.reduce((t, h) => t + h.outputTokens, 0),
      read: data.hourly.reduce((t, h) => t + h.cacheReadTokens, 0),
      sessions: data.session.length,
      complete: data.scan?.complete,
      readBytes: data.scan?.readBytes,
    };
  }

  it('pins the reader count: inline below threshold, bounded above it', () => {
    // One file never fans out, however large; below-threshold unread reads inline.
    expect(codexReaderCount(10 * 1024 ** 3, 1, 56)).toBe(1);
    expect(codexReaderCount(1024, 100, 56)).toBe(1);
    expect(codexReaderCount(64 * 1024 ** 2 - 1, 100, 56)).toBe(1);
    // Above the threshold: about 512 MB per reader, at least 2, at most 4.
    expect(codexReaderCount(64 * 1024 ** 2, 100, 56)).toBe(2);
    expect(codexReaderCount(1024 ** 3, 100, 56)).toBe(2);
    expect(codexReaderCount(1024 ** 3 + 1, 100, 56)).toBe(3);
    expect(codexReaderCount(13 * 1024 ** 3, 1422, 56)).toBe(4);
    expect(codexReaderCount(100 * 1024 ** 3, 10000, 256)).toBe(4);
    // A quarter of the CPUs, never fewer than 2 when fanning out; never more than files.
    expect(codexReaderCount(13 * 1024 ** 3, 1422, 4)).toBe(2);
    expect(codexReaderCount(13 * 1024 ** 3, 3, 56)).toBe(3);
    expect(codexReaderCount(13 * 1024 ** 3, 2, 56)).toBe(2);
  });

  it('partitions disjointly, in order, balanced by unread bytes', () => {
    const entries = [
      { file: 'a', unreadBytes: 300 },
      { file: 'b', unreadBytes: 300 },
      { file: 'c', unreadBytes: 300 },
      { file: 'd', unreadBytes: 300 },
    ];
    const chunks = partitionCodexFiles(entries, 2);
    // Disjoint and covering, global order preserved (the merge relies on it).
    expect(chunks.flat().sort()).toEqual(['a', 'b', 'c', 'd']);
    expect(chunks.flat()).toEqual(['a', 'b', 'c', 'd']);
    // Balanced by unread bytes.
    const sizes = new Map(entries.map((e) => [e.file, e.unreadBytes]));
    const sums = chunks.map((chunk) => chunk.reduce((t, f) => t + (sizes.get(f) ?? 0), 0));
    expect(sums).toEqual([600, 600]);
    // One giant file cannot split (one reader per file): its chunk overshoots
    // by at most that file, and every file is still covered exactly once.
    const giant = [
      { file: 'a', unreadBytes: 100 },
      { file: 'b', unreadBytes: 100 },
      { file: 'c', unreadBytes: 800 },
      { file: 'd', unreadBytes: 100 },
      { file: 'e', unreadBytes: 100 },
    ];
    const giantChunks = partitionCodexFiles(giant, 2);
    expect(giantChunks.flat()).toEqual(['a', 'b', 'c', 'd', 'e']);
    const giantSizes = new Map(giant.map((e) => [e.file, e.unreadBytes]));
    const giantSums = giantChunks.map((chunk) =>
      chunk.reduce((t, f) => t + (giantSizes.get(f) ?? 0), 0)
    );
    expect(Math.max(...giantSums)).toBeLessThanOrEqual(1200 / 2 + 800);
    // A cut never lands on the last entry: two uneven files still split.
    expect(
      partitionCodexFiles(
        [
          { file: 'a', unreadBytes: 470 },
          { file: 'b', unreadBytes: 471 },
        ],
        2
      )
    ).toEqual([['a'], ['b']]);
    // Never more chunks than files or readers; one reader keeps one chunk.
    expect(partitionCodexFiles(entries, 99)).toHaveLength(4);
    expect(partitionCodexFiles(entries, 1)).toEqual([['a', 'b', 'c', 'd']]);
    expect(partitionCodexFiles([], 4)).toEqual([[]]);
  });

  it('reads each file exactly once across readers (one reader per file)', async () => {
    rollout('rollout-a.jsonl', usageLines('s-a', 100, 10, '10'));
    rollout('rollout-b.jsonl', usageLines('s-b', 200, 20, '11'));
    rollout('rollout-c.jsonl', usageLines('s-c', 300, 30, '12'));
    rollout('rollout-d.jsonl', usageLines('s-d', 400, 40, '13'));
    const seen: string[][] = [];
    const data = await collect(
      'cache',
      { fanoutThresholdBytes: 1 },
      {
        partitionRunner: async (_request, options, partition) => {
          seen.push([...partition.files]);
          return collectCodexPartition(partition, options);
        },
      }
    );
    expect(seen.length).toBeGreaterThan(1);
    const all = seen.flat();
    expect(all).toHaveLength(4);
    expect(new Set(all).size).toBe(4);
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(4);
  });

  it('merged totals equal a single inline reader on fixtures', async () => {
    rollout('rollout-a.jsonl', usageLines('s-a', 100, 10, '10'));
    rollout('rollout-b.jsonl', usageLines('s-b', 200, 20, '11'));
    rollout('rollout-c.jsonl', usageLines('s-c', 300, 30, '12'));
    const inline = await collect('cache-inline');
    const fanned = await collect('cache-fanned', { fanoutThresholdBytes: 1 });
    expect(totals(fanned)).toEqual(totals(inline));
    expect(fanned.hourly).toEqual(inline.hourly);
    expect(fanned.session).toEqual(inline.session);
    expect(fanned.scan).toEqual(inline.scan);
    // The fanned-out pass wrote the same shared checkpoints an inline pass
    // reads: a warm inline scan over the same cache reads nothing new.
    const warm = await collect('cache-fanned');
    expect(warm.scan?.readBytes).toBe(0);
    expect(warm.hourly).toEqual(inline.hourly);
    expect(warm.session).toEqual(inline.session);
  });

  it('merged totals equal a single reader through real nested workers', async () => {
    rollout('rollout-a.jsonl', usageLines('s-a', 100, 10, '10'));
    rollout('rollout-b.jsonl', usageLines('s-b', 200, 20, '11'));
    rollout('rollout-c.jsonl', usageLines('s-c', 300, 30, '12'));
    rollout('rollout-d.jsonl', usageLines('s-d', 400, 40, '13'));
    const inline = await collect('cache-inline');
    // No injected runner: the default spawns real nested reader threads.
    const fanned = await collect('cache-fanned', { fanoutThresholdBytes: 1 });
    expect(fanned.hourly).toEqual(inline.hourly);
    expect(fanned.session).toEqual(inline.session);
    expect(fanned.scan).toEqual(inline.scan);
    expect(fanned.eventCount).toBe(4);
  });

  it('a slow partition does not hold up the others', async () => {
    rollout('rollout-a.jsonl', usageLines('s-a', 100, 10, '10'));
    rollout('rollout-b.jsonl', usageLines('s-b', 200, 20, '11'));
    rollout('rollout-c.jsonl', usageLines('s-c', 300, 30, '12'));
    rollout('rollout-d.jsonl', usageLines('s-d', 400, 40, '13'));
    const finished: string[] = [];
    let calls = 0;
    const data = await collect(
      'cache',
      { fanoutThresholdBytes: 1 },
      {
        partitionRunner: async (_request, options, partition) => {
          calls++;
          const mine = calls;
          // The first partition started stalls; the others must still finish first.
          if (mine === 1) await new Promise((resolve) => setTimeout(resolve, 500));
          const outcomes = await collectCodexPartition(partition, options);
          finished.push(`p${mine}`);
          return outcomes;
        },
      }
    );
    expect(calls).toBeGreaterThan(1);
    // Every partition ran concurrently: the stalled first finished last.
    expect(finished[finished.length - 1]).toBe('p1');
    expect(finished).toHaveLength(calls);
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(4);
    expect(data.hourly.reduce((t, h) => t + h.inputTokens, 0)).toBe(1000 - 80);
  });

  it('a timed-out reader falls back to this pass starting rows (deadline analog)', async () => {
    rollout('rollout-a.jsonl', usageLines('s-a', 100, 10, '10'));
    rollout('rollout-b.jsonl', usageLines('s-b', 200, 20, '11'));
    // Pass 1 (inline) banks both files' rows; then both grow. Pass 2 fans out
    // and b's reader times out: b contributes its starting rows, like a file
    // the budget never reached, and the scan stays honestly incomplete.
    await collect('cache');
    fs.appendFileSync(
      path.join(root, 'codex', 'sessions', 'rollout-a.jsonl'),
      JSON.stringify(tokens(200, 30, 25, '2026-10-01T10:55:00Z')) + '\n'
    );
    fs.appendFileSync(
      path.join(root, 'codex', 'sessions', 'rollout-b.jsonl'),
      JSON.stringify(tokens(999, 60, 99, '2026-10-01T11:55:00Z')) + '\n'
    );
    let calls = 0;
    const data = await collect(
      'cache',
      { fanoutThresholdBytes: 1 },
      {
        partitionRunner: async (_request, options, partition) => {
          calls++;
          if (partition.files.some((f) => f.endsWith('rollout-b.jsonl')))
            throw new CodexPartitionError(true);
          return collectCodexPartition(partition, options);
        },
      }
    );
    expect(calls).toBe(2);
    expect(data.scan?.complete).toBe(false);
    expect(data.scan?.failedFiles).toBe(0);
    // a's new tail is counted (80 + 200 - 30 - 80); b's starting rows are
    // kept but its new tail is not counted yet.
    expect(data.hourly.reduce((t, h) => t + h.inputTokens, 0)).toBe(170 + 180);
    expect(data.eventCount).toBe(3);
  });

  it('a crashed reader keeps its starting rows and counts its files failed (S1)', async () => {
    rollout('rollout-a.jsonl', usageLines('s-a', 100, 10, '10'));
    rollout('rollout-b.jsonl', usageLines('s-b', 200, 20, '11'));
    // Pass 1 banks both files; then both grow and b's reader crashes (OOM,
    // spawn failure). b still reports its starting rows — dropping the whole
    // partition would dip Codex on the page for a cycle — with failed++ so
    // the next pass re-reads it.
    await collect('cache');
    fs.appendFileSync(
      path.join(root, 'codex', 'sessions', 'rollout-a.jsonl'),
      JSON.stringify(tokens(200, 30, 25, '2026-10-01T10:55:00Z')) + '\n'
    );
    fs.appendFileSync(
      path.join(root, 'codex', 'sessions', 'rollout-b.jsonl'),
      JSON.stringify(tokens(999, 60, 99, '2026-10-01T11:55:00Z')) + '\n'
    );
    const data = await collect(
      'cache',
      { fanoutThresholdBytes: 1 },
      {
        partitionRunner: async (_request, options, partition) => {
          if (partition.files.some((f) => f.endsWith('rollout-b.jsonl')))
            throw new Error('reader crashed');
          return collectCodexPartition(partition, options);
        },
      }
    );
    expect(data.scan?.complete).toBe(false);
    expect(data.scan?.failedFiles).toBe(1);
    expect(data.hourly.reduce((t, h) => t + h.inputTokens, 0)).toBe(170 + 180);
    expect(data.eventCount).toBe(3);
    // The next pass (healthy readers) converges and counts b's new tail.
    const converged = await collect('cache', { fanoutThresholdBytes: 1 });
    expect(converged.scan?.failedFiles).toBe(0);
    expect(converged.hourly.reduce((t, h) => t + h.inputTokens, 0)).toBe(170 + 180 + 759);
  });

  it('dedup and the mtime pre-filter hold under fan-out', async () => {
    // The parent dedups before partitioning, so two byte-identical rollouts
    // count once even fanned out; a stale rollout is never read.
    const lines = usageLines('s-dup', 500, 50, '14');
    rollout('rollout-dupe-1.jsonl', lines, NOW);
    rollout('rollout-dupe-2.jsonl', lines, NOW - 3600000);
    rollout('rollout-c.jsonl', usageLines('s-c', 200, 20, '15'));
    rollout(
      'rollout-stale.jsonl',
      [meta(), model(), tokens(999, 0, 999, '2026-08-01T15:00:00Z')],
      NOW - 40 * 86400000
    );
    let partitions = 0;
    const data = await collect(
      'cache',
      { fanoutThresholdBytes: 1 },
      {
        partitionRunner: async (_request, options, partition) => {
          partitions++;
          return collectCodexPartition(partition, options);
        },
      }
    );
    expect(partitions).toBeGreaterThan(1);
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(2);
    expect(data.hourly.reduce((t, h) => t + h.inputTokens, 0)).toBe(480 + 180);
    const cacheDir = path.join(root, 'cache', 'account-activity-v1');
    const checkpoints: string[] = [];
    const walk = (d: string) => {
      for (const item of fs.readdirSync(d, { withFileTypes: true })) {
        const full = path.join(d, item.name);
        if (item.isDirectory()) walk(full);
        else checkpoints.push(full);
      }
    };
    walk(cacheDir);
    // The kept copy and rollout-c have checkpoints; the dropped copy and the
    // stale file were never read, so they do not.
    expect(checkpoints).toHaveLength(2);
  });

  it('fan-out honors an explicit maxBytesPerFile across passes', async () => {
    // The default byte cap is lifted for partition readers (deadline-bounded
    // instead), but an explicitly set one still bounds each pass, as inline:
    // two files converge over bounded passes with identical totals.
    rollout('rollout-a.jsonl', usageLines('s-a', 100, 10, '10'));
    rollout('rollout-b.jsonl', usageLines('s-b', 200, 20, '11'));
    // 250 bytes: smaller than the ~339-byte files (so several passes are
    // needed) but larger than any line (a cap below a line length stalls even
    // inline, by design: a pass only banks whole lines).
    const extra = { fanoutThresholdBytes: 1, maxBytesPerFile: 250 };
    let data = await collect('cache', extra);
    expect(data.scan?.complete).toBe(false);
    for (let attempt = 0; attempt < 12 && !data.scan?.complete; attempt++) {
      data = await collect('cache', extra);
    }
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(2);
    expect(data.hourly.reduce((t, h) => t + h.inputTokens, 0)).toBe(80 + 180);
    let inline = await collect('cache-inline', { maxBytesPerFile: 250 });
    for (let attempt = 0; attempt < 12 && !inline.scan?.complete; attempt++) {
      inline = await collect('cache-inline', { maxBytesPerFile: 250 });
    }
    expect(data.hourly).toEqual(inline.hourly);
    expect(data.session).toEqual(inline.session);
  });

  it('readers only get files with unread bytes (N1)', async () => {
    // Pass 1 banks a and b; then fresh c and d arrive. Pass 2 fans out, but
    // the readers only see c and d — a and b tally from the just-loaded
    // checkpoints, so no reader starts with nothing to read.
    rollout('rollout-a.jsonl', usageLines('s-a', 100, 10, '10'));
    rollout('rollout-b.jsonl', usageLines('s-b', 200, 20, '11'));
    await collect('cache');
    rollout('rollout-c.jsonl', usageLines('s-c', 300, 30, '12'));
    rollout('rollout-d.jsonl', usageLines('s-d', 400, 40, '13'));
    const seen: string[][] = [];
    const data = await collect(
      'cache',
      { fanoutThresholdBytes: 1 },
      {
        partitionRunner: async (_request, options, partition) => {
          seen.push([...partition.files]);
          return collectCodexPartition(partition, options);
        },
      }
    );
    expect(seen.length).toBe(2);
    const names = seen.flat().map((f) => path.basename(f)).sort();
    expect(names).toEqual(['rollout-c.jsonl', 'rollout-d.jsonl']);
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(4);
    expect(data.hourly.reduce((t, h) => t + h.inputTokens, 0)).toBe(80 + 180 + 280 + 380);
  });

  it('a file moving between partitions counts once (N5)', async () => {
    // Bounded passes (250 B cap) force re-partitioning: between passes a
    // grows by a 200-byte garbage line (no usage; under the cap so passes
    // keep banking whole lines), which moves b to the other partition.
    // Totals still match a fully converged inline run.
    rollout('rollout-a.jsonl', usageLines('s-a', 100, 10, '10'));
    rollout('rollout-b.jsonl', usageLines('s-b', 200, 20, '11'));
    rollout('rollout-c.jsonl', usageLines('s-c', 300, 30, '12'));
    const extra = { fanoutThresholdBytes: 1, maxBytesPerFile: 250 };
    // Record each pass's partitions in call order.
    const calls: string[][] = [];
    const deps: CollectAccountActivityDeps = {
      partitionRunner: async (_request, options, partition) => {
        calls.push(partition.files.map((f) => path.basename(f)));
        return collectCodexPartition(partition, options);
      },
    };
    let data = await collect('cache', extra, deps);
    expect(data.scan?.complete).toBe(false);
    const pass1 = calls.splice(0);
    // Grow a mid-convergence: its unread share jumps, so the next pass
    // repartitions and b lands in a different partition.
    const grown = path.join(root, 'codex', 'sessions', 'rollout-a.jsonl');
    fs.appendFileSync(grown, `${'x'.repeat(199)}\n`);
    fs.utimesSync(grown, new Date(NOW + 3600000), new Date(NOW + 3600000));
    data = await collect('cache', extra, deps);
    const pass2 = calls.splice(0);
    const indexOf = (pass: string[][], name: string) => pass.findIndex((c) => c.includes(name));
    expect(pass1.length).toBe(2);
    expect(pass2.length).toBe(2);
    expect(indexOf(pass1, 'rollout-b.jsonl')).toBe(0);
    expect(indexOf(pass2, 'rollout-b.jsonl')).toBe(1);
    for (let attempt = 0; attempt < 12 && !data.scan?.complete; attempt++) {
      data = await collect('cache', extra, deps);
    }
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(3);
    let inline = await collect('cache-inline');
    for (let attempt = 0; attempt < 12 && !inline.scan?.complete; attempt++) {
      inline = await collect('cache-inline');
    }
    expect(data.hourly).toEqual(inline.hourly);
    expect(data.session).toEqual(inline.session);
  });

  it('rotation and deletion between fan-out passes match inline (N5)', async () => {
    // Pass 1 converges; then b is rewritten with new usage (rotation), c is
    // deleted, and a grows, so pass 2 fans out over a changed tree. Totals
    // match a fully converged inline run over the same final files.
    rollout('rollout-a.jsonl', usageLines('s-a', 100, 10, '10'));
    rollout('rollout-b.jsonl', usageLines('s-b', 200, 20, '11'));
    rollout('rollout-c.jsonl', usageLines('s-c', 300, 30, '12'));
    const extra = { fanoutThresholdBytes: 1 };
    let data = await collect('cache', extra);
    expect(data.scan?.complete).toBe(true);
    fs.writeFileSync(
      path.join(root, 'codex', 'sessions', 'rollout-b.jsonl'),
      usageLines('s-b2', 500, 50, '14')
        .map((line) => JSON.stringify(line))
        .join('\n') + '\n'
    );
    fs.appendFileSync(
      path.join(root, 'codex', 'sessions', 'rollout-a.jsonl'),
      JSON.stringify(tokens(200, 30, 25, '2026-10-01T10:55:00Z')) + '\n'
    );
    fs.rmSync(path.join(root, 'codex', 'sessions', 'rollout-c.jsonl'));
    data = await collect('cache', extra);
    for (let attempt = 0; attempt < 12 && !data.scan?.complete; attempt++) {
      data = await collect('cache', extra);
    }
    expect(data.scan?.complete).toBe(true);
    // b's old rows are gone (checkpoint identity missed on rewrite), c is
    // gone, a counts both its events.
    expect(data.hourly.reduce((t, h) => t + h.inputTokens, 0)).toBe(170 + 480);
    expect(data.eventCount).toBe(3);
    let inline = await collect('cache-inline');
    for (let attempt = 0; attempt < 12 && !inline.scan?.complete; attempt++) {
      inline = await collect('cache-inline');
    }
    expect(data.hourly).toEqual(inline.hourly);
    expect(data.session).toEqual(inline.session);
  });

  it('experiment-roots requests never fan out (owned by the other lane)', async () => {
    // Coordination with fix/aac-fw4-experiment-roots-20261006: a codex
    // request carrying experimentRoots reads inline even above the fan-out
    // threshold, because its checkpoint mode differs. (Roots still come from
    // codexHome here; that lane changes the roots selection.)
    rollout('rollout-a.jsonl', usageLines('s-a', 100, 10, '10'));
    rollout('rollout-b.jsonl', usageLines('s-b', 200, 20, '11'));
    let partitions = 0;
    const data = await collectAccountActivity(
      {
        kind: 'codex',
        codexHome: path.join(root, 'codex'),
        cacheDir: path.join(root, 'cache'),
        experimentRoots: [path.join(root, 'codex', 'sessions')],
      },
      { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, 'cache'), fanoutThresholdBytes: 1 },
      {
        partitionRunner: async () => {
          partitions++;
          throw new Error('must not fan out');
        },
      }
    );
    expect(partitions).toBe(0);
    expect(data.scan?.complete).toBe(true);
    expect(data.eventCount).toBe(2);
  });

  it('a reader that never answers times out and its thread stops', async () => {
    const hangWorker = path.join(root, 'hang-worker.cjs');
    fs.writeFileSync(hangWorker, 'setInterval(() => {}, 1000);\n');
    const started = Date.now();
    const error = await spawnCodexPartitionReader(
      { kind: 'codex', codexHome: path.join(root, 'codex'), cacheDir: path.join(root, 'cache') },
      { minDate: NOW - 31 * 86400000, cacheDir: path.join(root, 'cache') },
      { files: [], checkpointDir: path.join(root, 'cache'), deadline: Date.now() + 60000 },
      100,
      hangWorker
    ).then(
      () => null,
      (e: unknown) => e
    );
    expect(error).toBeInstanceOf(CodexPartitionError);
    expect((error as CodexPartitionError).timedOut).toBe(true);
    // The spawner only settles after terminate() resolves, so a prompt
    // rejection proves the hung thread was actually stopped.
    expect(Date.now() - started).toBeLessThan(5000);
  });
});
