import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { collectAccountActivity } from '../../../src/web-server/usage/account-activity-collector';

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
    expect(data.session.length).toBeGreaterThan(0);
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
    expect(data.hourly.reduce((sum, hour) => sum + hour.inputTokens, 0)).toBe(1000);
    expect(data.hourly.reduce((sum, hour) => sum + hour.cacheReadTokens, 0)).toBe(800);
    expect(data.session.map((session) => session.sessionId).sort()).toEqual(['sub-1', 'uuid-1']);
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
    expect(logged?.fallbackCost).toBe(0);
    expect(logged?.modelBreakdowns[0].fallbackCost).toBe(0);
    expect(unpriced?.cost).toBeCloseTo(unlogged, 9);
    expect(unpriced?.fallbackCost).toBeCloseTo(unlogged, 9);
  });
});
