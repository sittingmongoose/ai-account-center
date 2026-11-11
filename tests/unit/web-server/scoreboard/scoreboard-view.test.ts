import { afterEach, describe, expect, it, spyOn } from 'bun:test';
import fs from 'fs';
import path from 'path';
import { CatalogReader, type CatalogUse } from '../../../../src/web-server/scoreboard/catalog';
import { RunsReader } from '../../../../src/web-server/scoreboard/runs-reader';
import { judgeGroup } from '../../../../src/web-server/scoreboard/scoreboard-verdicts';
import {
  buildScoreboardView,
  type Column,
  type MeasureValue,
  type ScoreboardView,
} from '../../../../src/web-server/scoreboard/scoreboard-view';
import { ScoreboardService } from '../../../../src/web-server/scoreboard/scoreboard-service';
import { run as projectRun } from '../../../../src/web-server/status/run-schema';
import { NOW, tempDirs } from '../status/status-test-helpers';
import { runDoc } from './scoreboard-test-helpers';

const dirs = tempDirs();
afterEach(() => dirs.cleanup());

const { writeScoreboardFixture } = (await import(
  path.resolve(import.meta.dir, '../../../../scripts/scoreboard-fixture.mjs')
)) as {
  writeScoreboardFixture: (
    root: string,
    options?: { now?: number }
  ) => { root: string; runs: string[]; damaged: string };
};

const EXAMPLE = path.resolve(
  import.meta.dir,
  '../../../../docs/agent-reports/examples/scoreboard-view.example.json'
);

async function fixtureView(): Promise<ScoreboardView> {
  const root = dirs.make('ccs-scoreboard-');
  writeScoreboardFixture(root, { now: NOW });
  const options = { root: () => root, now: () => NOW };
  const catalog = new CatalogReader(options);
  const runs = new RunsReader(options);
  await Promise.all([catalog.scan(), runs.scan()]);
  return buildScoreboardView({
    now: NOW,
    timeZone: 'UTC',
    root,
    catalog: catalog.snapshot(),
    runs: runs.snapshot(),
  });
}

function rows(view: ScoreboardView, use: string, type: string) {
  const found = view.uses.find((row) => row.id === use)?.taskTypes.find((t) => t.id === type);
  if (!found) throw new Error(`no ${use}/${type}`);
  return found.rows;
}

const OPUS = 'anthropic/claude-opus-5-5|claude-code|xhigh';

describe('the fixture view', () => {
  it('names the damaged file, the catalog gaps and what it could not score', async () => {
    const view = await fixtureView();
    const [runs, catalog] = view.sources;
    expect(runs.id).toBe('scoreboard-runs');
    expect(runs.state).toBe('unreadable');
    expect(runs.detail).toContain('1 file unreadable');
    expect(runs.detail).toContain('vm.scoreboard-lead--damaged.json');
    expect(runs.detail).toContain('contains NUL bytes');
    expect(runs.detail).toContain('1 run with no model or task type not scored');
    expect(runs.detail).toContain('1 review ignored: not a review run, or no earlier known run');
    expect(catalog.id).toBe('scoreboard-catalog');
    expect(catalog.state).toBe('ok');
    // audit-read is in the catalog now (building and PM-user views), so nothing is left unscored by task type.
    expect(catalog.detail).not.toContain('audit-read');
    expect(catalog.detail).toContain('example/example-1 (1 run)');
  });

  it('keeps observed and benchmark data in separate columns', async () => {
    const view = await fixtureView();
    // plan is keyed by process, so the benchmark's process gets its own row; nothing is averaged across them.
    const opus = rows(view, 'planning', 'plan').filter((r) => r.model.includes('opus'));
    const observed = opus.find((r) => r.process === null);
    expect(observed?.n).toEqual({ observed: 2, benchmark: 0 });
    expect(observed?.observed?.n).toBe(2);
    expect(observed?.observed?.source).toBe('observed');
    expect(observed?.observed?.measures['silent-misses']).toMatchObject({ value: null, n: 0 });
    expect(observed?.benchmarks).toEqual([]);
    const row = opus.find((r) => r.process?.id === 'a');
    expect(row?.n).toEqual({ observed: 0, benchmark: 6 });
    expect(row?.observed).toBeNull();
    const bench = row?.benchmarks[0];
    expect(bench?.benchmark).toBe('plan-pilot2');
    expect(bench?.benchmarkLabel).toBe('Planning wizard pilot 2');
    expect(bench?.n).toBe(6);
    expect(bench?.measures['silent-misses']).toMatchObject({
      value: 0.01667,
      n: 6,
      of: 6,
      per: 360,
      preliminary: true,
    });
  });

  it('keys a by-process task type by process, and only there', async () => {
    const view = await fixtureView();
    const scout = rows(view, 'planning', 'scout');
    expect(scout.map((r) => r.key).sort()).toEqual([
      'anthropic/claude-opus-5-5|claude-code|xhigh|scout-pipeline|1.0',
      'anthropic/claude-opus-5-5|claude-code|xhigh|scout-pipeline|2.0',
      'openai/gpt-6.1-sol|codex|high|scout-pipeline|2.0',
      'openai/gpt-6.1-sol|codex|high||',
    ]);
    const v2 = scout.find((r) => r.key.endsWith('|2.0') && r.model.includes('opus'));
    expect(v2?.process).toEqual({ id: 'scout-pipeline', version: '2.0' });
    expect(v2?.observed?.measures['facts-kept'].n).toBe(5);
    expect(rows(view, 'building', 'worknode-build').every((r) => r.process === null)).toBe(true);
    expect(rows(view, 'planning', 'plan').some((r) => r.process?.id === 'a')).toBe(true);
  });

  it('ranks by the bar: minRuns, a blind review for observed, within its own benchmark', async () => {
    const view = await fixtureView();
    const build = rows(view, 'building', 'worknode-build');
    const byModel = (name: string) => build.find((r) => r.model.includes(name));
    expect(byModel('sonnet')).toMatchObject({
      ranked: false,
      rank: null,
      verdict: 'not-enough-data',
      verdictLabel: 'Not enough data yet',
      why: 'too little data (n = 2)',
    });
    expect(byModel('sonnet')?.observed?.runs).toHaveLength(2);
    expect(byModel('haiku')).toMatchObject({ ranked: false, why: 'no blind review yet (n = 5)' });
    expect(byModel('opus')).toMatchObject({ ranked: true, rank: 1, basis: 'observed' });
    // A benchmark column needs no review and ranks only against its own benchmark.
    const plan = rows(view, 'planning', 'plan').filter((r) => r.benchmarks.length > 0);
    expect(plan.map((r) => r.benchmarks[0]?.rank)).toEqual([1, 2]);
    expect(plan[0].basis).toBe('benchmark');
    expect(plan[0].why).toStartWith('In the benchmark Planning wizard pilot 2: ');
  });

  it('gives verdicts from the weights, per use, each with its n', async () => {
    const view = await fixtureView();
    const build = rows(view, 'building', 'worknode-build');
    const opus = build.find((r) => r.key === OPUS);
    const glm = build.find((r) => r.model.includes('glm'));
    const sol = build.find((r) => r.model.includes('sol'));
    expect(opus).toMatchObject({ verdict: 'recommended', verdictLabel: 'Recommended' });
    expect(opus?.why).toContain('(n = 7)');
    expect(sol).toMatchObject({ verdict: 'fine', verdictLabel: 'Fine' });
    expect(glm).toMatchObject({ verdict: 'avoid', verdictLabel: 'Avoid for this' });
    expect(glm?.why).toBe('Flagged for cheating in 2 of 5 runs (n = 5).');
    expect(glm?.observed?.flags.map((flag) => [flag.kind, flag.run, flag.evidence])).toEqual([
      [
        'incomplete-read',
        'vm.scoreboard-lead--WN-23',
        '/mnt/Cursor/share/example-project/evidence/WN-23.md',
      ],
      [
        'test-weakened',
        'vm.scoreboard-lead--WN-22',
        '/mnt/Cursor/share/example-project/evidence/WN-22.md',
      ],
      [
        'shortcut',
        'vm.scoreboard-lead--WN-21',
        '/mnt/Cursor/share/example-project/evidence/WN-21.md',
      ],
    ]);
    // The same row can differ per use: the weights differ.
    expect(rows(view, 'pm-users', 'worknode-build').find((r) => r.key === OPUS)?.rank).toBe(1);
  });

  it('shows disputed and overturned work with who overturned it', async () => {
    const view = await fixtureView();
    const sol = rows(view, 'building', 'worknode-build').find((r) => r.model.includes('sol'));
    const quality = sol?.observed?.measures.quality as MeasureValue;
    expect(quality).toMatchObject({ value: 0.75, n: 4, disputed: 1, cannotVerify: 1 });
    expect(quality.overturns?.map((o) => [o.run, o.by, o.disputed])).toEqual([
      ['vm.scoreboard-lead--WN-14', 'anthropic/claude-opus-5-5 xhigh', false],
      ['vm.scoreboard-lead--WN-15', 'anthropic/claude-opus-5-5 xhigh', true],
    ]);
    // Six items for seven Opus runs: the fix round counts once.
    const opus = rows(view, 'building', 'worknode-build').find((r) => r.key === OPUS);
    expect(opus?.observed?.n).toBe(7);
    expect(opus?.observed?.measures.quality).toMatchObject({ value: 1, n: 6 });
    // A rewritten run keeps its highest seq, not its failed first copy.
    expect(opus?.observed?.measures.reliability).toMatchObject({ value: 1, n: 7 });
  });

  it('expands * to every task type with data and lists named ones even with none', async () => {
    const view = await fixtureView();
    const named = view.uses.find((use) => use.id === 'planning')?.taskTypes.map((t) => t.id);
    expect(named).toEqual(['scout', 'plan', 'worknode-create']);
    expect(rows(view, 'planning', 'worknode-create')).toEqual([]);
    const all = view.uses.find((use) => use.id === 'pm-users')?.taskTypes.map((t) => t.id);
    expect(all).toEqual([
      'scout',
      'plan',
      'worknode-build',
      'build',
      'review',
      'mechanical',
      'audit-read',
    ]);
  });

  it('marks imported runs and labels a model the catalog does not know by its id', async () => {
    const view = await fixtureView();
    const build = rows(view, 'pm-users', 'build');
    const sonnet = build.find((r) => r.model.includes('sonnet'));
    expect(sonnet?.observed?.imported).toBe(2);
    expect(sonnet?.observed?.runs[0].imported?.from).toContain('closure-reaudit');
    const unlisted = build.find((r) => r.model === 'example/example-1');
    expect(unlisted).toMatchObject({ label: 'example/example-1', inCatalog: false });
  });

  it('lists the latest runs, newest first, in plain terms', async () => {
    const view = await fixtureView();
    expect(view.recent).toHaveLength(20);
    const times = view.recent.map((run) => Date.parse(run.endedAt));
    expect(times).toEqual([...times].sort((a, b) => b - a));
    expect(view.recent[0].id).toBe('vm.scoreboard-lead--no-type');
    expect(view.recent[0].task.type).toBeNull();
  });

  it('never carries an account into the view', async () => {
    const view = await fixtureView();
    expect(JSON.stringify(view)).not.toContain('account-a');
  });

  it('matches the committed example view, field by field', async () => {
    const view = await fixtureView();
    const text = `${JSON.stringify(view, null, 2)}\n`;
    if (process.env.UPDATE_SCOREBOARD_EXAMPLE) fs.writeFileSync(EXAMPLE, text);
    expect(text).toBe(fs.readFileSync(EXAMPLE, 'utf8'));
  });
});

describe('verdict rules', () => {
  const DEFS = new Map(
    [
      { id: 'quality', better: 'higher', unit: 'share' },
      { id: 'cheating', better: 'lower', unit: 'share' },
      { id: 'speed', better: 'lower', unit: 's/unit' },
    ].map((def) => [
      def.id,
      {
        ...def,
        label: def.id[0].toUpperCase() + def.id.slice(1),
        description: null,
        metric: null,
        preliminaryUnless: null,
      } as never,
    ])
  );
  const LISTED = ['quality', 'cheating', 'speed'];
  const USE: Pick<CatalogUse, 'weights'> = { weights: { quality: 3, cheating: 3, speed: 1 } };

  function value(number: number | null, n = 10, fields: Partial<MeasureValue> = {}): MeasureValue {
    return {
      value: number,
      unit: 'share',
      n,
      low: null,
      high: null,
      interval: null,
      of: number === null ? null : number * n,
      per: n,
      preliminary: false,
      ...fields,
    };
  }

  function column(
    local: string,
    quality: number | null,
    cheating: number | null,
    speed: number | null,
    fields: Partial<Column> = {}
  ): Column {
    return {
      source: 'observed',
      benchmark: null,
      benchmarkLabel: null,
      n: 10,
      imported: 0,
      // Above the bar (minRuns, 5 reviewed items), so these tests exercise scoring, not the bar.
      reviews: 6,
      ranked: false,
      rank: null,
      score: null,
      verdict: 'not-enough-data',
      verdictLabel: '',
      why: '',
      preliminary: false,
      measures: { quality: value(quality), cheating: value(cheating), speed: value(speed) },
      flags: [],
      runs: [{ id: local } as never],
      ...fields,
    };
  }

  const judge = (columns: Column[]) => judgeGroup(columns, USE, LISTED, DEFS, 5);

  it('a row alone cannot be recommended, whatever it scores', () => {
    const [only] = judge([column('a', 1, 0, 10)]);
    expect(only).toMatchObject({ ranked: true, rank: 1, score: 1, verdict: 'fine' });
    expect(only.why).toBe('The only ranked row here, so nothing to compare it with (n = 10).');
  });

  it('recommends a top row with a peer; close rows share it and rank by score', () => {
    const judged = judge([
      column('b', 0.95, 0, 10),
      column('a', 1, 0, 10),
      column('c', 0.6, 0, 10),
    ]);
    expect(judged.map((c) => [c.rank, c.verdict])).toEqual([
      [2, 'recommended'],
      [1, 'recommended'],
      [3, 'fine'],
    ]);
    expect(judged[1].why).toBe('Strongest on Quality, with no cheating flags (n = 10).');
    expect(judged[2].why).toBe('Behind the top row, weakest on Quality (n = 10).');
  });

  it('avoids on a cheating floor, a heavy share under half, or a low score', () => {
    const judged = judge([
      column('good', 1, 0, 10),
      column('cheat', 1, 0.2, 10),
      column('poor', 0.4, 0, 10),
      column('slow', 0.52, 0.15, 1000),
    ]);
    expect(judged[1]).toMatchObject({ verdict: 'avoid' });
    expect(judged[1].why).toBe('Flagged for cheating in 2 of 10 runs (n = 10).');
    expect(judged[2].why).toBe('Quality is 40% (n = 10).');
    expect(judged[3].verdict).toBe('avoid');
    expect(judged[3].why).toContain('weakest on Speed');
  });

  it('a measure unknown for a row drops out of its score, and low coverage cannot recommend', () => {
    const judged = judgeGroup(
      [column('a', null, null, 10), column('b', 1, 0, 10)],
      { weights: { quality: 3, cheating: 3, speed: 1 } },
      LISTED,
      DEFS,
      5
    );
    expect(judged[0]).toMatchObject({ verdict: 'fine', score: 1 });
    expect(judged[0].why).toBe('Too few measures known to recommend it (n = 10).');
  });

  it('a row with none of the use measures known has no verdict', () => {
    const [none] = judgeGroup([column('a', null, null, null)], USE, LISTED, DEFS, 5);
    expect(none).toMatchObject({ ranked: false, rank: null, verdict: 'not-enough-data' });
    expect(none.why).toContain('(n = 10)');
  });

  it('holds the bar: below minRuns, and observed with no blind review', () => {
    const judged = judge([
      column('few', 1, 0, 10, { n: 4 }),
      column('unreviewed', 1, 0, 10, { reviews: 0 }),
      column('bench', 1, 0, 10, { source: 'benchmark', benchmark: 'b', reviews: 0 }),
    ]);
    expect(judged.map((c) => [c.ranked, c.why])).toEqual([
      [false, 'too little data (n = 4)'],
      [false, 'no blind review yet (n = 10)'],
      [true, 'The only ranked row here, so nothing to compare it with (n = 10).'],
    ]);
  });

  it('one reviewed item alone is never enough for Recommended or Avoid', () => {
    // Fix rounds on one task: 5 runs, 5 reviews, but the pass rate stands on one item (1 of 1).
    const oneItem = (local: string, quality: number, cheating: number) => {
      const held = column(local, quality, cheating, 10, { reviews: 5 });
      held.measures.quality = value(quality, 1);
      return held;
    };
    const judged = judge([oneItem('a', 1, 0), oneItem('poor', 0.4, 0)]);
    expect(judged.map((c) => [c.ranked, c.verdict, c.why])).toEqual([
      [false, 'not-enough-data', 'too few blind reviews yet (1 of 5 needed, n = 10)'],
      [false, 'not-enough-data', 'too few blind reviews yet (1 of 5 needed, n = 10)'],
    ]);
    // minRuns reviewed items clear the bar: the same columns then get their scored verdicts.
    const cleared = judge([
      column('a', 1, 0, 10, { reviews: 5 }),
      column('poor', 0.4, 0, 10, { reviews: 5 }),
    ]);
    expect(cleared.map((c) => [c.ranked, c.verdict])).toEqual([
      [true, 'recommended'],
      [true, 'avoid'],
    ]);
    // A Fine may stand on fewer items, and a benchmark column needs none.
    const fine = column('fine', 1, 0, 10, { reviews: 5 });
    fine.measures.quality = value(1, 1);
    expect(judge([fine])[0]).toMatchObject({ ranked: true, verdict: 'fine' });
  });

  it('five fix rounds on one item are not enough for Recommended', () => {
    // One item, five reviewed rounds (quality 1 of 1): without the item bar it tops the group.
    const thin = column('thin', 1, 0, 10, { reviews: 5 });
    thin.measures.quality = value(1, 1);
    const deep = column('deep', 0.6, 0, 10, { reviews: 5 });
    const judged = judge([thin, deep]);
    expect(judged[0]).toMatchObject({
      ranked: false,
      verdict: 'not-enough-data',
      why: 'too few blind reviews yet (1 of 5 needed, n = 10)',
    });
    expect(judged[1]).toMatchObject({ ranked: true });
  });

  it('one item over five rounds whose last review is disputed is not enough for Recommended', () => {
    // Every item's latest review disputed: quality decides nothing (per null) while 5
    // review runs accrue. The bar holds zero decided items, never the run count.
    const thin = column('thin', 1, 0, 10, { reviews: 5 });
    thin.measures.quality = value(1, 1, { per: null });
    const deep = column('deep', 0.6, 0, 10, { reviews: 5 });
    const judged = judge([thin, deep]);
    expect(judged[0]).toMatchObject({
      ranked: false,
      verdict: 'not-enough-data',
      why: 'too few blind reviews yet (0 of 5 needed, n = 10)',
    });
    expect(judged[1]).toMatchObject({ ranked: true });
  });

  it('five fix rounds on one item are not enough for Avoid', () => {
    // The heavy-share Avoid on one item (quality 40%, weight 3) is held by the same bar.
    const thin = column('thin', 0.4, 0, 10, { reviews: 5 });
    thin.measures.quality = value(0.4, 1);
    const deep = column('deep', 0.9, 0, 10, { reviews: 5 });
    const judged = judge([thin, deep]);
    expect(judged[0]).toMatchObject({
      ranked: false,
      verdict: 'not-enough-data',
      why: 'too few blind reviews yet (1 of 5 needed, n = 10)',
    });
    expect(judged[1]).toMatchObject({ ranked: true });
  });

  it('a cheating Avoid is exempt from the review bar', () => {
    // Cheating flags are per-run and do not depend on reviews: one reviewed item is
    // enough for a cheating-floor Avoid. A quality Avoid on one item is still held.
    const cheat = column('cheat', 1, 0.4, 10, { reviews: 1 });
    cheat.measures.quality = value(1, 1);
    const peer = column('peer', 1, 0, 10, { reviews: 5 });
    const judged = judge([cheat, peer]);
    expect(judged[0]).toMatchObject({
      ranked: true,
      verdict: 'avoid',
      why: 'Flagged for cheating in 4 of 10 runs (n = 10).',
    });
  });

  it('a demoted row does not set the top score, the peers or the ranks', () => {
    // The one-item column (quality 1 of 1) tops the group and is pulled by the bar. The
    // survivor is then the only ranked row: rank 1, fine — never Recommended at rank 2
    // with no rank 1, and never "Behind the top row" behind a demoted row.
    const thin = column('thin', 1, 0, 10, { reviews: 5 });
    thin.measures.quality = value(1, 1);
    const deep = column('deep', 0.97, 0, 10, { reviews: 5 });
    const judged = judge([thin, deep]);
    expect(judged[0]).toMatchObject({ ranked: false, verdict: 'not-enough-data' });
    expect(judged[1]).toMatchObject({
      ranked: true,
      rank: 1,
      verdict: 'fine',
      why: 'The only ranked row here, so nothing to compare it with (n = 10).',
    });
  });

  it('a pulled row does not set the best marks are measured against', () => {
    // The demoted row is the fastest by far. Measured against it, deep's speed mark
    // collapses and deep reads Avoid; measured only against the survivors, it is Fine.
    const thin = column('thin', 1, 0, 1, { reviews: 5 });
    thin.measures.quality = value(1, 1);
    const deep = column('deep', 0.52, 0.15, 100, { reviews: 5 });
    const mid = column('mid', 0.9, 0, 100, { reviews: 5 });
    const judged = judge([thin, deep, mid]);
    expect(judged[0]).toMatchObject({ ranked: false, verdict: 'not-enough-data' });
    expect(judged[1]).toMatchObject({ ranked: true, verdict: 'fine' });
    expect(judged[2]).toMatchObject({ ranked: true });
  });

  it("pulling one demoted row never changes another row's verdict", () => {
    // One fast one-item row (demoted), a two-item row whose Avoid comes only from the
    // fast one's speed best, and a well-reviewed row. The pull of the fast one alone
    // must leave the other two exactly as they judge without it.
    const thin = column('thin', 1, 0, 1, { reviews: 5 });
    thin.measures.quality = value(1, 1);
    const sonnet = column('sonnet', 0.5, 1 / 7, 1000, { reviews: 5 });
    sonnet.measures.quality = value(0.5, 2);
    sonnet.measures.cheating = value(1 / 7, 7);
    const opus = column('opus', 1, 0, 1000, { reviews: 5 });
    const judged = judge([thin, sonnet, opus]);
    expect(judged[0]).toMatchObject({ ranked: false, verdict: 'not-enough-data' });
    expect(judged[1]).toMatchObject({ ranked: true, verdict: 'fine' });
    expect(judged[2]).toMatchObject({ ranked: true, verdict: 'recommended' });
  });

  it('a weak survivor of a pull is the only ranked row, not behind the demoted top', () => {
    const thin = column('thin', 1, 0, 10, { reviews: 5 });
    thin.measures.quality = value(1, 1);
    const deep = column('deep', 0.5, 0, 10, { reviews: 5 });
    const judged = judge([thin, deep]);
    expect(judged[0]).toMatchObject({ ranked: false, verdict: 'not-enough-data' });
    expect(judged[1]).toMatchObject({
      ranked: true,
      rank: 1,
      verdict: 'fine',
      why: 'The only ranked row here, so nothing to compare it with (n = 10).',
    });
  });

  it('says preliminary when a weighted value is', () => {
    const preliminary = column('a', 1, 0, 10);
    preliminary.measures.quality = value(1, 10, { preliminary: true });
    expect(judge([preliminary, column('b', 1, 0, 10)])[0].why).toEndWith(' Preliminary.');
  });
});

describe('service', () => {
  it('serves a short cache, then rescans', async () => {
    const root = dirs.make('ccs-scoreboard-');
    writeScoreboardFixture(root, { now: NOW });
    let now = NOW;
    const service = new ScoreboardService({
      root: () => root,
      now: () => now,
      timeZone: () => 'UTC',
    });
    const first = await service.view();
    expect(await service.view()).toBe(first);
    now += 11_000;
    const second = await service.view();
    expect(second).not.toBe(first);
    expect(second.generatedAt).toBe(new Date(now).toISOString());
  });

  it('answers on a hung share with what it has, and the source says so', async () => {
    const root = dirs.make('ccs-scoreboard-');
    const hung = {
      readdir: () => new Promise<never>(() => undefined),
      stat: () => new Promise<never>(() => undefined),
      readFile: () => new Promise<never>(() => undefined),
    };
    const service = new ScoreboardService({
      root: () => root,
      now: () => NOW,
      timeZone: () => 'UTC',
      fs: hung,
      scanWaitMs: 5,
    });
    const view = await service.view();
    expect(view.sources.map((source) => [source.state, source.detail])).toEqual([
      ['missing', 'Waiting for the first scan.'],
      ['missing', 'Waiting for the first scan.'],
    ]);
    expect(view.uses).toEqual([]);
  });

  it('shows a missing share as sources, never a crash', async () => {
    const service = new ScoreboardService({
      root: () => path.join(dirs.make(), 'none'),
      now: () => NOW,
      timeZone: () => 'UTC',
    });
    const view = await service.view();
    expect(view.sources.map((source) => source.state)).toEqual(['missing', 'missing']);
    expect(view.catalog.minRuns).toBe(5);
    expect(view.recent).toEqual([]);
  });

  it('shows a damaged catalog as a problem and keeps going', async () => {
    const root = dirs.make('ccs-scoreboard-');
    writeScoreboardFixture(root, { now: NOW });
    fs.writeFileSync(path.join(root, 'v1', 'catalog', 'uses.json'), '{ damaged');
    const service = new ScoreboardService({
      root: () => root,
      now: () => NOW,
      timeZone: () => 'UTC',
    });
    const view = await service.view();
    expect(view.sources[1]).toMatchObject({ state: 'unreadable' });
    expect(view.sources[1].detail).toContain('catalog/uses.json (not valid JSON)');
    expect(view.uses).toEqual([]);
    expect(view.recent.length).toBeGreaterThan(0);
  });

  it('seeds the catalog and creates the folders only under an existing root', async () => {
    const root = dirs.make('ccs-scoreboard-');
    const service = new ScoreboardService({
      root: () => root,
      now: () => NOW,
      timeZone: () => 'UTC',
    });
    await service.seed();
    expect(fs.readdirSync(path.join(root, 'v1', 'catalog')).sort()).toEqual([
      'benchmarks.json',
      'measures.json',
      'models.json',
      'task-types.json',
      'uses.json',
    ]);
    const gone = path.join(root, 'not-mounted');
    await new ScoreboardService({ root: () => gone }).seed();
    expect(fs.existsSync(gone)).toBe(false);
  });
});

describe('the runs Source', () => {
  it('names outcomes it could not count, and says what they were left out of', () => {
    const doc = (local: string, outcome: string) => ({
      file: `writers/vm.lead/runs/2026-11-06/${local}.json`,
      firstSeenAt: NOW,
      doc: projectRun(
        runDoc('vm.lead', local, { outcome }) as unknown as Record<string, unknown>,
        'vm.lead'
      ),
    });
    const view = buildScoreboardView({
      now: NOW,
      timeZone: 'UTC',
      root: '/reports',
      catalog: {
        state: 'ok',
        scannedAt: NOW,
        missingFiles: [],
        problems: [],
        minRuns: 5,
        models: [],
        taskTypes: [],
        measures: [],
        uses: [],
        benchmarks: [],
      },
      runs: {
        state: 'ok',
        scannedAt: NOW,
        runs: [doc('A', 'done'), doc('B', 'partial'), doc('C', 'partial'), doc('D', 'blocked')],
        writers: 1,
        problems: [],
      },
    });
    expect(view.sources[0].state).toBe('ok');
    expect(view.sources[0].detail).toContain(
      '3 runs with an outcome that is neither done nor a failure, left out of reliability, overclaim and speed and counted as no accepted units: blocked (1 run), partial (2 runs).'
    );
  });
});

describe('what the Sources name', () => {
  const catalog = (missingFiles: string[]) => ({
    state: 'ok' as const,
    scannedAt: NOW,
    missingFiles,
    problems: [],
    minRuns: 5,
    models: [],
    taskTypes: [],
    measures: [],
    uses: [],
    benchmarks: [],
  });

  it('names runs with a negative quota move', () => {
    const run = projectRun(
      runDoc('vm.lead', 'A', {
        cost: { quota: { account: 'a', window: '5h', deltaPct: -3 } },
      }) as unknown as Record<string, unknown>,
      'vm.lead'
    );
    const view = buildScoreboardView({
      now: NOW,
      timeZone: 'UTC',
      root: '/reports',
      catalog: catalog([]),
      runs: {
        state: 'ok',
        scannedAt: NOW,
        runs: [{ file: 'x', firstSeenAt: NOW, doc: run }],
        writers: 1,
        problems: [],
      },
    });
    expect(view.sources[0].detail).toContain('negative quota move');
    expect(view.sources[0].detail).toContain('vm.lead--A');
  });

  it('names a catalog seed the server could not write, by file and code', async () => {
    const root = dirs.make('ccs-scoreboard-');
    fs.mkdirSync(path.join(root, 'v1', 'catalog'), { recursive: true });
    const link = spyOn(fs.promises, 'link').mockImplementation((async () => {
      throw Object.assign(new Error('words from the system'), { code: 'EPERM' });
    }) as typeof fs.promises.link);
    const service = new ScoreboardService({
      root: () => root,
      now: () => NOW,
      timeZone: () => 'UTC',
    });
    try {
      await service.seed();
    } finally {
      link.mockRestore();
    }
    const { detail, state } = (await service.view()).sources[1];
    expect(state).toBe('missing');
    expect(detail).toContain('Could not seed models.json: EPERM.');
    expect(detail).not.toContain('words from the system');
    expect(fs.readdirSync(path.join(root, 'v1', 'catalog'))).toEqual([]);
  });
});
