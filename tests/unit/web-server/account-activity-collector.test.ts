import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { collectAccountActivity } from '../../../src/web-server/usage/account-activity-collector';
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
