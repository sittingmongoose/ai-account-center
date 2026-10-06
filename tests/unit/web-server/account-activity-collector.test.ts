import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  aggregateSessionAggregates,
  collectAccountActivity,
} from '../../../src/web-server/usage/account-activity-collector';
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
});
