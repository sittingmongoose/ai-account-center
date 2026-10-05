import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  collectAccountActivity,
  parseJsonlMappedUsageLine,
} from '../../../src/web-server/usage/account-activity-collector';
import type { JsonlFieldMapping } from '../../../src/web-server/usage/worker-client';
import { analyticsSessionKey } from '../../../src/web-server/usage/analytics-session-key';

const NOW = Date.parse('2026-10-01T16:30:00Z');
const MIN_DATE = NOW - 31 * 86_400_000;
let root: string;

function ompLine(model = 'deepseek-v4.1-flash', cost = 0.25, timestamp = '2026-10-01T15:05:00Z') {
  return JSON.stringify({
    id: 'm1',
    timestamp,
    type: 'message',
    message: {
      role: 'assistant',
      model,
      provider: 'chutes',
      usage: {
        input: 1000,
        output: 200,
        cacheRead: 3000,
        cacheWrite: 400,
        reasoningTokens: 10,
        totalTokens: 4200,
        cost: { total: cost },
      },
    },
  });
}

function museLine(kind = 'model_completed', recordedAt = Date.parse('2026-10-01T15:10:00Z')) {
  return JSON.stringify({
    schema_version: 1,
    recorded_at: recordedAt,
    payload: {
      event: {
        kind,
        model: 'muse-spark-1.3-contributor',
        usage: {
          input_tokens: 500,
          output_tokens: 60,
          cached_tokens: 400,
          cache_read_tokens: 400,
          cache_write_tokens: 5,
          reasoning_tokens: 7,
        },
      },
    },
  });
}

async function collectOmp(roots: string[]) {
  return collectAccountActivity(
    { kind: 'omp', roots },
    { minDate: MIN_DATE, cacheDir: path.join(root, 'cache') }
  );
}

async function collectMuse(sessionsDir: string) {
  return collectAccountActivity(
    { kind: 'muse', sessionsDir },
    { minDate: MIN_DATE, cacheDir: path.join(root, 'cache') }
  );
}

async function collectJsonl(roots: string[], mapping: JsonlFieldMapping) {
  return collectAccountActivity(
    { kind: 'jsonl', roots, mapping },
    { minDate: MIN_DATE, cacheDir: path.join(root, 'cache') }
  );
}

describe('omp and muse account activity', () => {
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-activity-sources-'));
  });
  afterEach(() => fs.rmSync(root, { recursive: true, force: true }));

  it('maps omp session files across roots, including advisor and extensionless files', async () => {
    const first = path.join(root, 'omp1', 'sessions');
    const second = path.join(root, 'omp2', 'sessions');
    fs.mkdirSync(path.join(first, 'slug'), { recursive: true });
    fs.mkdirSync(path.join(second, '2026-10-01T15-00_uuid'), { recursive: true });
    fs.writeFileSync(
      path.join(first, 'slug', '2026-10-01T15-00_uuid.jsonl'),
      `${ompLine()}\n${JSON.stringify({ type: 'session', id: 'x' })}\nplain content\n`
    );
    fs.writeFileSync(path.join(second, '2026-10-01T15-30_uuid2'), `${ompLine('qwen3.8-max', 0)}\n`);
    fs.writeFileSync(
      path.join(second, '2026-10-01T15-00_uuid', '__advisor.jsonl'),
      `${ompLine('k3', 1.5)}\n`
    );
    const data = await collectOmp([
      path.join(root, 'omp1', 'sessions'),
      path.join(root, 'omp2', 'sessions'),
    ]);
    expect(data.eventCount).toBe(3);
    const models = new Map(
      data.hourly.flatMap((hour) => hour.modelBreakdowns).map((m) => [m.modelName, m])
    );
    expect(models.get('deepseek-v4.1-flash')?.inputTokens).toBe(1000);
    // Logged cost wins where nonzero; the breakdown keeps every token field.
    expect(models.get('deepseek-v4.1-flash')?.cost).toBeCloseTo(0.25, 9);
    expect(models.get('k3')?.cost).toBeCloseTo(1.5, 9);
    // A logged 0 is not free: the qwen row prices through the resolver.
    expect(models.get('qwen3.8-max')?.cost ?? 0).toBeGreaterThan(0);
    // One session per id across roots: the advisor file joins the session it belongs to, and
    // every id leaves the reader as its key, never raw.
    expect(data.session.map((session) => session.sessionId).sort()).toEqual(
      [
        analyticsSessionKey('omp', '2026-10-01T15-00_uuid'),
        analyticsSessionKey('omp', '2026-10-01T15-30_uuid2'),
      ].sort()
    );
    expect(JSON.stringify(data.session)).not.toContain('uuid');
  });

  it('resumes omp files from checkpoints without duplicating events', async () => {
    const sessions = path.join(root, 'omp', 'sessions');
    fs.mkdirSync(sessions, { recursive: true });
    const file = path.join(sessions, '2026-10-01T15-00_uuid.jsonl');
    fs.writeFileSync(file, `${ompLine()}\n`);
    const first = await collectOmp([sessions]);
    expect(first.eventCount).toBe(1);
    fs.appendFileSync(file, `${ompLine('qwen3.8-max', 0)}\n`);
    const second = await collectOmp([sessions]);
    expect(second.eventCount).toBe(2);
    expect(second.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(2000);
  });

  it('maps muse sessions once, never doubling attribution events', async () => {
    const sessions = path.join(root, 'muse', 'sessions');
    const dir = path.join(sessions, '2026', '10', '01', 'uuid-1');
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(
      path.join(dir, 'session.jsonl'),
      `${museLine()}\n${museLine('goal_usage_attribution')}\na prompt fragment\n`
    );
    const sub = path.join(dir, 'subagent', 'sub-1');
    fs.mkdirSync(sub, { recursive: true });
    fs.writeFileSync(path.join(sub, 'session.jsonl'), `${museLine()}\n`);
    const data = await collectMuse(sessions);
    expect(data.eventCount).toBe(2);
    // Muse input includes the cache reads: (500 - 400) uncached per event.
    expect(data.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(200);
    expect(data.hourly.reduce((sum, hour) => sum + hour.cacheReadTokens, 0)).toBe(800);
    expect(data.session.map((session) => session.sessionId).sort()).toEqual(
      [analyticsSessionKey('muse', 'sub-1'), analyticsSessionKey('muse', 'uuid-1')].sort()
    );
    expect(JSON.stringify(data.session)).not.toContain('uuid-1');
  });

  it('prices unlogged rows at fallback rates when no table knows the model', async () => {
    const sessions = path.join(root, 'omp', 'sessions');
    fs.mkdirSync(sessions, { recursive: true });
    fs.writeFileSync(
      path.join(sessions, '2026-10-01T15-00_uuid.jsonl'),
      `${ompLine('no-such-model-xyz', 0)}\n`
    );
    const data = await collectOmp([sessions]);
    const breakdown = data.hourly.flatMap((hour) => hour.modelBreakdowns)[0];
    // Unknown-model fallback rates: 3/15/3.75/0.3 $/M.
    const expected = (1000 / 1e6) * 3 + (200 / 1e6) * 15 + (400 / 1e6) * 3.75 + (3000 / 1e6) * 0.3;
    expect(breakdown.cost).toBeCloseTo(expected, 9);
    // The fallback-priced part is reported, so clients can show it as not logged.
    expect(breakdown.fallbackCost).toBeCloseTo(expected, 9);
    expect(data.hourly[0].fallbackCost).toBeCloseTo(expected, 9);
  });

  it('reports no fallback part for logged OMP cost, even without a listed rate', async () => {
    const sessions = path.join(root, 'omp', 'sessions');
    fs.mkdirSync(sessions, { recursive: true });
    fs.writeFileSync(
      path.join(sessions, '2026-10-01T15-00_uuid.jsonl'),
      `${ompLine('no-such-model-xyz', 0.25)}\n${ompLine('no-such-model-xyz', 0, '2026-10-01T14:20:00Z').replace('"m1"', '"m2"')}\n`
    );
    const data = await collectOmp([sessions]);
    const logged = data.hourly.find((hour) => hour.hour === '2026-10-01 15:00');
    const unpriced = data.hourly.find((hour) => hour.hour === '2026-10-01 14:00');
    const unlogged = (1000 / 1e6) * 3 + (200 / 1e6) * 15 + (400 / 1e6) * 3.75 + (3000 / 1e6) * 0.3;
    expect(logged?.cost).toBeCloseTo(0.25, 9);
    expect(logged?.fallbackCost).toBeUndefined();
    expect(logged?.modelBreakdowns[0].fallbackCost).toBeUndefined();
    expect(unpriced?.cost).toBeCloseTo(unlogged, 9);
    expect(unpriced?.fallbackCost).toBeCloseTo(unlogged, 9);
  });

  it('splits logged and unlogged events of one hour, model and session', async () => {
    const sessions = path.join(root, 'omp', 'sessions');
    fs.mkdirSync(sessions, { recursive: true });
    fs.writeFileSync(
      path.join(sessions, '2026-10-01T15-00_uuid.jsonl'),
      `${ompLine('no-such-model-xyz', 0.25)}\n${ompLine('no-such-model-xyz', 0, '2026-10-01T15:20:00Z').replace('"m1"', '"m2"')}\n`
    );
    const data = await collectOmp([sessions]);
    expect(data.hourly).toHaveLength(1);
    const hour = data.hourly[0];
    const unlogged = (1000 / 1e6) * 3 + (200 / 1e6) * 15 + (400 / 1e6) * 3.75 + (3000 / 1e6) * 0.3;
    // The unlogged event is priced (and flagged) on its own, never covered by the logged cost.
    expect(hour.inputTokens).toBe(2000);
    expect(hour.cost).toBeCloseTo(0.25 + unlogged, 9);
    expect(hour.fallbackCost).toBeCloseTo(unlogged, 9);
    expect(data.eventCount).toBe(2);
  });

  it('prices omp rows under the provider that served them, not under the tool', async () => {
    const sessions = path.join(root, 'omp', 'sessions');
    fs.mkdirSync(sessions, { recursive: true });
    fs.writeFileSync(
      path.join(sessions, '2026-10-01T15-00_uuid.jsonl'),
      `${ompLine('no-such-model-xyz', 0.25).replace('"chutes"', '"anthropic"')}\n`
    );
    const data = await collectOmp([sessions]);
    expect(data.hourly[0].modelBreakdowns[0].provider).toBe('anthropic');
    // The tool still names the session.
    expect(data.session[0].target).toBe('omp');
  });

  it('counts a resumed omp session copied into a second root once', async () => {
    const name = '2026-10-01T15-00-00-000Z_01a0d985.jsonl';
    const first = path.join(root, 'job', 'resume-01', 'sessions');
    const second = path.join(root, 'job', 'resume-02', 'sessions');
    fs.mkdirSync(first, { recursive: true });
    fs.mkdirSync(second, { recursive: true });
    const original = `${ompLine()}\n${ompLine('k3', 1.5).replace('"m1"', '"m2"')}\n`;
    fs.writeFileSync(path.join(first, name), original);
    // The resume copied the file and appended to the copy.
    fs.writeFileSync(
      path.join(second, name),
      `${original}${ompLine('qwen3.8-max', 0).replace('"m1"', '"m3"')}\n`
    );
    const data = await collectOmp([first, second]);
    expect(data.eventCount).toBe(3);
    expect(data.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(3000);
    // A different session with the same name that diverged is kept whole.
    const third = path.join(root, 'job', 'other', 'sessions');
    fs.mkdirSync(third, { recursive: true });
    fs.writeFileSync(path.join(third, name), `${ompLine('glm-5.3-flash', 0.1)}\n`);
    const again = await collectOmp([first, second, third]);
    expect(again.eventCount).toBe(4);
  });
});

describe('generic jsonl account activity', () => {
  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-activity-jsonl-'));
  });
  afterEach(() => fs.rmSync(root, { recursive: true, force: true }));

  function writeLogs(directory: string, lines: string[]): string {
    fs.mkdirSync(directory, { recursive: true });
    const file = path.join(directory, 'usage.jsonl');
    fs.writeFileSync(file, `${lines.join('\n')}\n`);
    return directory;
  }

  it('parses mapped records with nested dot paths and epoch timestamps', async () => {
    const logs = writeLogs(path.join(root, 'logs'), [
      JSON.stringify({
        ts: '2026-10-01T15:05:00Z',
        model: 'jsonl-model',
        usage: { input_tokens: 1000, output_tokens: 200 },
      }),
      JSON.stringify({
        ts: Date.parse('2026-10-01T15:35:00Z') / 1000,
        model: 'jsonl-model',
        usage: { input_tokens: 500, output_tokens: 50 },
      }),
    ]);
    const data = await collectJsonl([logs], {
      timestamp: 'ts',
      model: 'model',
      inputTokens: 'usage.input_tokens',
      outputTokens: 'usage.output_tokens',
    });
    expect(data.eventCount).toBe(2);
    expect(data.hourly.map((hour) => hour.hour)).toEqual(['2026-10-01 15:00']);
    const breakdowns = data.hourly.flatMap((hour) => hour.modelBreakdowns);
    expect(breakdowns.map((m) => m.modelName)).toEqual(['jsonl-model']);
    expect(data.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(1500);
    expect(data.hourly.reduce((sum, hour) => sum + hour.outputTokens, 0)).toBe(250);
  });

  it('skips malformed lines and unparseable timestamps without failing', async () => {
    const logs = writeLogs(path.join(root, 'logs'), [
      'plain content',
      JSON.stringify({ ts: 'not a date', usage: { input_tokens: 10 } }),
      JSON.stringify(['ts', '2026-10-01T15:05:00Z']),
      JSON.stringify({
        ts: '2026-10-01T15:05:00Z',
        usage: { input_tokens: 100, output_tokens: 20 },
      }),
    ]);
    const data = await collectJsonl([logs], {
      timestamp: 'ts',
      inputTokens: 'usage.input_tokens',
      outputTokens: 'usage.output_tokens',
    });
    expect(data.eventCount).toBe(1);
    expect(data.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(100);
    expect(data.scan?.complete).toBe(true);
  });

  it('keeps logged costs on their own rows and unmapped counts at zero', async () => {
    const logs = writeLogs(path.join(root, 'logs'), [
      JSON.stringify({ ts: '2026-10-01T15:05:00Z', price: 0.5 }),
      JSON.stringify({ ts: '2026-10-01T15:06:00Z', price: 0 }),
    ]);
    const data = await collectJsonl([logs], { timestamp: 'ts', cost: 'price' });
    expect(data.eventCount).toBe(2);
    const hour = data.hourly.find((entry) => entry.hour === '2026-10-01 15:00');
    // A logged 0 is "not logged", never free: only the 0.5 counts as logged.
    expect(hour?.modelBreakdowns.length).toBe(1);
    expect(hour?.modelBreakdowns[0].cost).toBeCloseTo(0.5, 9);
  });

  it('keeps separate checkpoints per mapping over one root', async () => {
    const logs = writeLogs(path.join(root, 'logs'), [
      JSON.stringify({ ts: '2026-10-01T15:05:00Z', a: 100, b: 7 }),
    ]);
    const first = await collectJsonl([logs], { timestamp: 'ts', inputTokens: 'a' });
    expect(first.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(100);
    const second = await collectJsonl([logs], { timestamp: 'ts', inputTokens: 'b' });
    expect(second.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(7);
    // Neither mapping rereads the file: both checkpoints hold their own rows.
    const again = await collectJsonl([logs], { timestamp: 'ts', inputTokens: 'a' });
    expect(again.eventCount).toBe(1);
    expect(again.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(100);
  });

  it('parses mapped lines defensively', () => {
    const mapping: JsonlFieldMapping = { timestamp: 'ts' };
    expect(parseJsonlMappedUsageLine('nope', mapping)).toBeNull();
    expect(parseJsonlMappedUsageLine('["ts"]', mapping)).toBeNull();
    expect(parseJsonlMappedUsageLine(JSON.stringify({}), mapping)).toBeNull();
    expect(
      parseJsonlMappedUsageLine(JSON.stringify({ ts: '2026-13-99T99:99:99Z' }), mapping)
    ).toBeNull();
    // Epoch milliseconds stay milliseconds; seconds scale up.
    const ms = parseJsonlMappedUsageLine(
      JSON.stringify({ ts: Date.parse('2026-10-01T15:05:00Z') }),
      mapping
    );
    expect(ms?.timestamp).toBe('2026-10-01T15:05:00.000Z');
    const sec = parseJsonlMappedUsageLine(
      JSON.stringify({ ts: Date.parse('2026-10-01T15:05:00Z') / 1000 }),
      mapping
    );
    expect(sec?.timestamp).toBe('2026-10-01T15:05:00.000Z');
    // Negative and non-numeric counts are zero, never negative or NaN.
    const counts = parseJsonlMappedUsageLine(
      JSON.stringify({ ts: '2026-10-01T15:05:00Z', a: -5, b: 'lots' }),
      { timestamp: 'ts', inputTokens: 'a', outputTokens: 'b' }
    );
    expect(counts?.inputTokens).toBe(0);
    expect(counts?.outputTokens).toBe(0);
  });
});
