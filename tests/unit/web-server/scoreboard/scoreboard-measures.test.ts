import { describe, expect, it } from 'bun:test';
import type { CatalogMeasure, TaskUnit } from '../../../../src/web-server/scoreboard/catalog';
import {
  buildReviewIndex,
  canonicalModel,
  computeMeasures,
  unitsOf,
} from '../../../../src/web-server/scoreboard/scoreboard-measures';
import { median, wilson90 } from '../../../../src/web-server/scoreboard/scoreboard-stats';
import { run as projectRun, type RunDoc } from '../../../../src/web-server/status/run-schema';
import { runDoc } from './scoreboard-test-helpers';

const W = 'vm.lead';
const R = 'mac.reviews';

function doc(local: string, fields: Record<string, unknown> = {}, writer = W): RunDoc {
  return projectRun(runDoc(writer, local, fields) as unknown as Record<string, unknown>, writer);
}

function reviewOf(
  local: string,
  of: string,
  fields: Record<string, unknown> = {},
  at = '2026-11-06T16:00:00Z'
) {
  const { disputed, overturned, verdict, by, ...rest } = fields;
  return doc(
    local,
    {
      endedAt: at,
      task: { id: local, type: 'review', title: 'Review' },
      review: {
        of: `${W}--${of}`,
        verdict: verdict ?? 'pass',
        findings: { high: 0, medium: 0, low: 0 },
        overturned: overturned ?? false,
        disputed: disputed ?? false,
        by: by ?? null,
      },
      ...rest,
    },
    R
  );
}

function def(id: string, fields: Partial<CatalogMeasure> = {}): CatalogMeasure {
  return {
    id,
    label: id,
    better: 'higher',
    unit: 'share',
    description: null,
    metric: null,
    preliminaryUnless: null,
    ...fields,
  };
}

const DEFS = new Map(
  [
    def('speed', { better: 'lower', unit: 's/unit' }),
    def('quality'),
    def('thoroughness'),
    def('laziness', { better: 'lower' }),
    def('cheating', { better: 'lower' }),
    def('cost', { better: 'lower', unit: 'credits/unit' }),
    def('reliability'),
    def('overclaim', { better: 'lower' }),
    def('facts-kept', { metric: { of: 'factsKept', per: 'factsTotal' } }),
    def('silent-misses', {
      better: 'lower',
      metric: { of: 'silent', per: 'goldAgreed' },
      preliminaryUnless: 'answerKeyFinal',
    }),
  ].map((measure) => [measure.id, measure])
);

function measure(id: string, runs: RunDoc[], reviews: RunDoc[] = [], unit: TaskUnit = 'task') {
  return computeMeasures(runs, unit, [id], DEFS, buildReviewIndex([...runs, ...reviews]))[id];
}

describe('stats', () => {
  it('matches known 90% Wilson bounds', () => {
    const half = wilson90(5, 10);
    expect(half?.low).toBeCloseTo(0.2693, 3);
    expect(half?.high).toBeCloseTo(0.7307, 3);
    const none = wilson90(0, 10);
    expect(none?.low).toBe(0);
    expect(none?.high).toBeCloseTo(0.2129, 3);
    const all = wilson90(10, 10);
    expect(all?.high).toBe(1);
    expect(all?.low).toBeCloseTo(0.7871, 3);
    expect(wilson90(1, 0)).toBeNull();
    expect(wilson90(3, 2)).toBeNull();
  });

  it('takes the middle value', () => {
    expect(median([])).toBeNull();
    expect(median([3, 1, 2])).toBe(2);
    expect(median([4, 1, 2, 3])).toBe(2.5);
  });
});

describe('identity', () => {
  it('uses an id that already starts with its provider as it is', () => {
    expect(canonicalModel('openai', 'openai/gpt-6.1-sol')).toBe('openai/gpt-6.1-sol');
    expect(canonicalModel('zai', 'glm-5.3')).toBe('zai/glm-5.3');
    expect(canonicalModel(null, 'glm-5.3')).toBe('glm-5.3');
  });

  it('reads a size in the task type unit; a run is one task unless it says otherwise', () => {
    const run = doc('A', { task: { id: 'A', type: 'build', title: 'A', size: { words: 3000 } } });
    expect(unitsOf(run, 'word')).toBe(3000);
    expect(unitsOf(run, 'file')).toBeNull();
    expect(unitsOf(run, 'task')).toBe(1);
  });
});

describe('measures', () => {
  it('speed: median per unit with its range; failed runs and unknown sizes do not count', () => {
    const task = (words: number | null) => ({ id: 'x', type: 'plan', title: 'x', size: { words } });
    const value = measure(
      'speed',
      [
        doc('A', { wallSeconds: 100, task: task(1000) }),
        doc('B', { wallSeconds: 300, task: task(1000) }),
        doc('C', { wallSeconds: 900, task: task(1000) }),
        doc('D', { wallSeconds: 500, task: task(null) }),
        doc('E', { wallSeconds: 5, task: task(1000), outcome: 'failed' }),
      ],
      [],
      'word'
    );
    expect(value).toMatchObject({
      value: 0.3,
      unit: 's/word',
      n: 3,
      low: 0.1,
      high: 0.9,
      interval: 'range',
    });
    expect(value.perTask).toEqual({ value: 400, n: 4, low: 100, high: 900 });
  });

  it('speed: unknown stays null, never 0', () => {
    const value = measure('speed', [doc('A', { wallSeconds: null })]);
    expect(value).toMatchObject({ value: null, n: 0, low: null, high: null });
  });

  it('quality: shares of items, with a Wilson interval and the reviewer', () => {
    const runs = [doc('A'), doc('B')];
    const value = measure('quality', runs, [
      reviewOf('R1', 'A'),
      reviewOf('R2', 'B', { overturned: true, verdict: 'fail', by: 'openai/gpt-6.1-sol high' }),
    ]);
    expect(value).toMatchObject({
      value: 0.5,
      n: 2,
      of: 1,
      per: 2,
      interval: 'wilson90',
      disputed: 0,
    });
    expect(value.low).toBeCloseTo(wilson90(1, 2)?.low ?? 0, 3);
    expect(value.overturns).toEqual([
      {
        run: `${W}--B`,
        review: `${R}--R2`,
        by: 'openai/gpt-6.1-sol high',
        disputed: false,
        evidence: null,
      },
    ]);
  });

  it('quality: fix rounds on one item count once, by the latest review, with the rounds', () => {
    const task = { id: 'WN-1', type: 'build', title: 'WN-1' };
    const value = measure(
      'quality',
      [doc('A', { task }), doc('B', { task }), doc('C')],
      [
        reviewOf('R1', 'A', { overturned: true, verdict: 'fail' }, '2026-11-06T15:50:00Z'),
        reviewOf('R2', 'B', {}, '2026-11-06T16:00:00Z'),
        reviewOf('R3', 'C'),
      ]
    );
    expect(value).toMatchObject({ value: 1, n: 2, of: 2, per: 2 });
    expect(value.rounds).toEqual({ value: 1.5, n: 2, low: 1, high: 2 });
    expect(value.overturns).toHaveLength(1);
  });

  it('quality: a disputed overturn shows as disputed and leaves the share; cannot-verify decides nothing', () => {
    const value = measure(
      'quality',
      [doc('A'), doc('B'), doc('C')],
      [
        reviewOf('R1', 'A', { overturned: true, disputed: true, verdict: 'fail' }),
        reviewOf('R2', 'B', { verdict: 'cannot-verify' }),
        reviewOf('R3', 'C'),
      ]
    );
    expect(value).toMatchObject({ value: 1, n: 1, disputed: 1, cannotVerify: 1 });
    expect(value.overturns?.[0].disputed).toBe(true);
  });

  it('quality: no review is unknown, never 0; only later reviews of known runs count', () => {
    const value = measure(
      'quality',
      [doc('A')],
      [reviewOf('R1', 'nobody'), reviewOf('R2', 'A', {}, '2020-01-01T00:00:00Z')]
    );
    expect(value).toMatchObject({ value: null, n: 0, low: null });
    expect(buildReviewIndex([doc('A'), reviewOf('R1', 'nobody')]).ignored).toBe(1);
    const notAReview = reviewOf('R2', 'A', { task: { id: 'R2', type: 'build', title: 'R2' } });
    const index = buildReviewIndex([doc('A'), notAReview]);
    expect(index.ignored).toBe(1);
    expect(index.of(`${W}--A`)).toEqual([]);
  });

  it('thoroughness: the median of the lower read ratio, capped at 1, with both parts', () => {
    const value = measure('thoroughness', [
      doc('A', {
        scope: { filesAssigned: 10, filesRead: 5, recordsAssigned: 10, recordsJudged: 10 },
      }),
      doc('B', { scope: { filesAssigned: 10, filesRead: 20 } }),
      doc('C', { scope: {} }),
    ]);
    expect(value).toMatchObject({ value: 0.75, n: 2, low: 0.5, high: 1 });
    expect(value.files).toEqual({ value: 0.75, n: 2, low: 0.5, high: 1 });
    expect(value.records).toEqual({ value: 1, n: 1, low: 1, high: 1 });
  });

  it('laziness: flagged or under 90% read; a run with neither fact is not evidence', () => {
    const flag = { kind: 'scope-cut', detail: 'Cut scope', evidence: '/p' };
    const value = measure('laziness', [
      doc('A', { flags: [flag] }),
      doc('B', { scope: { filesAssigned: 10, filesRead: 8 } }),
      doc('C', { scope: { filesAssigned: 10, filesRead: 9 } }),
      doc('D'),
    ]);
    expect(value).toMatchObject({ value: 0.6667, n: 3, of: 2, per: 3 });
    expect(measure('laziness', [doc('D')]).value).toBeNull();
  });

  it('cheating: flagged runs over all runs, counting each run once', () => {
    const flag = (kind: string) => ({ kind, detail: 'd', evidence: '/p' });
    const value = measure('cheating', [
      doc('A', { flags: [flag('shortcut'), flag('fabricated')] }),
      doc('B', { flags: [flag('incomplete-read')] }),
      doc('C'),
      doc('D'),
    ]);
    expect(value).toMatchObject({ value: 0.25, n: 4, of: 1, per: 4 });
  });

  it('cost: measured credits per accepted unit, with quota, usd and estimates kept apart', () => {
    const size = (files: number) => ({ id: 'x', type: 'mechanical', title: 'x', size: { files } });
    const value = measure(
      'cost',
      [
        doc('A', {
          task: size(10),
          cost: { credits: 20, usd: 2, quota: { account: 'account-a', window: '5h', deltaPct: 5 } },
        }),
        doc('B', {
          task: size(10),
          cost: { credits: 10, quota: { account: 'account-a', window: '7d', deltaPct: 1 } },
        }),
        doc('C', {
          task: size(30),
          cost: {
            credits: null,
            estimate: { source: 'quota-watch', method: 'token share', credits: 60 },
          },
        }),
      ],
      [reviewOf('R1', 'B', { overturned: true, verdict: 'fail' })],
      'file'
    );
    // B cost 10 but was overturned, so its units are not accepted: (20 + 10) / 10.
    expect(value).toMatchObject({ value: 3, unit: 'credits/file', n: 2 });
    expect(value.usd).toEqual({ value: 0.2, unit: 'usd/file', n: 1 });
    expect(value.quota).toEqual([
      { window: '5h', value: 0.5, unit: '%/file', n: 1 },
      { window: '7d', value: null, unit: '%/file', n: 1 },
    ]);
    expect(value.estimate).toEqual({
      label: 'estimate',
      credits: { value: 2, unit: 'credits/file', n: 1 },
      quota: [],
      methods: ['token share'],
      sources: ['quota-watch'],
    });
  });

  it('cost: unknown stays null, never 0', () => {
    const value = measure('cost', [doc('A', { cost: { credits: null } })]);
    expect(value).toMatchObject({ value: null, n: 0, estimate: null, usd: null, quota: [] });
  });

  it('reliability: runs that did not end failed or cancelled', () => {
    const value = measure('reliability', [
      doc('A'),
      doc('B', { outcome: 'failed' }),
      doc('C', { outcome: 'cancelled' }),
      doc('D', { outcome: 'done' }),
    ]);
    expect(value).toMatchObject({ value: 0.5, n: 4, of: 2, per: 4 });
  });

  it('overclaim: reviewed runs reporting done that a review overturned, disputes left out', () => {
    const value = measure(
      'overclaim',
      [doc('A'), doc('B'), doc('C'), doc('D'), doc('E', { outcome: 'failed' })],
      [
        reviewOf('R1', 'A', { overturned: true, verdict: 'fail' }),
        reviewOf('R2', 'B'),
        reviewOf('R3', 'C', { overturned: true, disputed: true, verdict: 'fail' }),
        reviewOf('R4', 'E', { overturned: true, verdict: 'fail' }),
      ]
    );
    expect(value).toMatchObject({ value: 0.5, n: 2, of: 1, per: 2 });
  });

  it('metric measures: sum over sum with a Wilson interval over the per-sum, n in runs', () => {
    const value = measure('facts-kept', [
      doc('A', { metrics: { factsKept: 90, factsTotal: 100 } }),
      doc('B', { metrics: { factsKept: 10, factsTotal: 100 } }),
      doc('C', { metrics: { factsKept: null, factsTotal: 100 } }),
      doc('D'),
    ]);
    expect(value).toMatchObject({ value: 0.5, n: 2, of: 100, per: 200, interval: 'wilson90' });
    expect(value.low).toBeCloseTo(wilson90(100, 200)?.low ?? 0, 3);
    expect(measure('facts-kept', [doc('D')])).toMatchObject({ value: null, n: 0 });
  });

  it('preliminaryUnless: preliminary unless every run behind the value has the metric at 1', () => {
    const final = { silent: 1, goldAgreed: 10, answerKeyFinal: 1 };
    expect(measure('silent-misses', [doc('A', { metrics: final })]).preliminary).toBe(false);
    expect(
      measure('silent-misses', [
        doc('A', { metrics: final }),
        doc('B', { metrics: { ...final, answerKeyFinal: 0 } }),
      ]).preliminary
    ).toBe(true);
    expect(
      measure('silent-misses', [doc('A', { metrics: { silent: 1, goldAgreed: 10 } })]).preliminary
    ).toBe(true);
    // A run with no value is not behind the number.
    expect(measure('silent-misses', [doc('A', { metrics: final }), doc('B')]).preliminary).toBe(
      false
    );
  });
});
