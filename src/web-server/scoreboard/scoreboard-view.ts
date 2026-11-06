/**
 * Builds the Model Scoreboard view from the runs and the catalog (docs/scoreboard.md). Pure: every
 * number, word and verdict the page draws is here. A row is model + runtime + effort, plus process
 * when the task type says so, within one task type; observed and benchmark runs stay in separate
 * columns and are only ever compared with their own kind (and their own benchmark).
 */
import type { RunDoc } from '../status/report-schema';
import type { CatalogMeasure, CatalogTaskType, CatalogUse } from './catalog';
import {
  buildReviewIndex,
  negativeQuotaRuns,
  unknownOutcomes,
  canonicalModel,
  computeMeasures,
  decisive,
  type ReviewIndex,
} from './scoreboard-measures';
import type {
  Column,
  FlagView,
  RunSummary,
  ScoreboardRow,
  ScoreboardTaskType,
  ScoreboardUse,
  ScoreboardView,
} from './scoreboard-view-types';
import { catalogSource, runsSource, type ScoreboardInputs } from './scoreboard-sources';
import { judgeGroup, VERDICT_LABELS } from './scoreboard-verdicts';

export * from './scoreboard-view-types';
export type { ScoreboardInputs } from './scoreboard-sources';

export const SCOREBOARD_POLL_SECONDS = 60;
const RECENT_RUNS = 20;
const COLUMN_RUNS = 20;

function newestFirst(a: RunDoc, b: RunDoc): number {
  return Date.parse(b.endedAt) - Date.parse(a.endedAt) || a.id.localeCompare(b.id);
}

function modelOf(run: RunDoc): string | null {
  return run.model.id ? canonicalModel(run.model.provider, run.model.id) : null;
}

function summary(run: RunDoc, labels: ReadonlyMap<string, string>): RunSummary {
  const model = modelOf(run);
  return {
    id: run.id,
    writer: run.writer,
    source: run.source,
    benchmark: run.benchmark,
    project: run.project,
    stream: run.stream,
    task: { id: run.task.id, type: run.task.type, title: run.task.title },
    model,
    label: model ? (labels.get(model) ?? model) : null,
    runtime: run.model.harness,
    effort: run.model.effort,
    process: run.process && { id: run.process.id, version: run.process.version },
    outcome: run.outcome,
    endedAt: run.endedAt,
    wallSeconds: run.wallSeconds,
    imported: run.imported,
    review: run.review && {
      of: run.review.of,
      verdict: run.review.verdict,
      overturned: run.review.overturned,
      disputed: run.review.disputed,
      by: run.review.by,
    },
    flags: run.flags.length,
    evidence: run.evidence,
  };
}

function flagsOf(runs: readonly RunDoc[]): FlagView[] {
  return [...runs].sort(newestFirst).flatMap((run) =>
    run.flags.map((flag) => ({
      kind: flag.kind,
      run: run.id,
      detail: flag.detail,
      evidence: flag.evidence,
      reportedBy: flag.reportedBy,
    }))
  );
}

interface Shared {
  index: ReviewIndex;
  defs: ReadonlyMap<string, CatalogMeasure>;
  labels: ReadonlyMap<string, string>;
  benchmarkLabels: ReadonlyMap<string, string>;
  minRuns: number;
}

function columnOf(
  runs: readonly RunDoc[],
  source: RunDoc['source'],
  benchmark: string | null,
  type: CatalogTaskType,
  shared: Shared
): Column {
  const measures = computeMeasures(runs, type.unit, type.measures, shared.defs, shared.index);
  const newest = [...runs].sort(newestFirst);
  return {
    source,
    benchmark,
    benchmarkLabel: benchmark ? (shared.benchmarkLabels.get(benchmark) ?? benchmark) : null,
    n: runs.length,
    imported: runs.filter((run) => run.imported).length,
    reviews: runs.reduce((sum, run) => sum + shared.index.of(run.id).filter(decisive).length, 0),
    ranked: false,
    rank: null,
    score: null,
    verdict: 'not-enough-data',
    verdictLabel: VERDICT_LABELS['not-enough-data'],
    why: '',
    preliminary: Object.values(measures).some((value) => value.preliminary),
    measures,
    flags: flagsOf(runs),
    runs: newest.slice(0, COLUMN_RUNS).map((run) => summary(run, shared.labels)),
  };
}

interface BaseRow {
  key: string;
  model: string;
  runtime: string | null;
  effort: string | null;
  process: { id: string; version: string | null } | null;
  observed: Column | null;
  benchmarks: Column[];
}

/** The rows of one task type: one per identity, with a column per kind of data. */
function baseRows(type: CatalogTaskType, runs: readonly RunDoc[], shared: Shared): BaseRow[] {
  const groups = new Map<string, { row: BaseRow; runs: RunDoc[] }>();
  for (const run of runs) {
    const model = modelOf(run);
    if (!model) continue;
    const process = type.byProcess && run.process ? run.process : null;
    const key = [model, run.model.harness ?? '', run.model.effort ?? ''];
    if (type.byProcess) key.push(process?.id ?? '', process?.version ?? '');
    const id = key.join('|');
    let group = groups.get(id);
    if (!group) {
      group = {
        row: {
          key: id,
          model,
          runtime: run.model.harness,
          effort: run.model.effort,
          process: process && { id: process.id, version: process.version },
          observed: null,
          benchmarks: [],
        },
        runs: [],
      };
      groups.set(id, group);
    }
    group.runs.push(run);
  }
  return [...groups.values()].map(({ row, runs: rowRuns }) => {
    const observed = rowRuns.filter((run) => run.source === 'observed');
    const benchmark = rowRuns.filter((run) => run.source === 'benchmark');
    const ids = [...new Set(benchmark.map((run) => run.benchmark))].sort((a, b) =>
      (a ?? '').localeCompare(b ?? '')
    );
    return {
      ...row,
      observed: observed.length > 0 ? columnOf(observed, 'observed', null, type, shared) : null,
      benchmarks: ids.map((id) =>
        columnOf(
          benchmark.filter((run) => run.benchmark === id),
          'benchmark',
          id,
          type,
          shared
        )
      ),
    };
  });
}

function rowOf(row: BaseRow, observed: Column | null, benchmarks: Column[], shared: Shared) {
  const rankedBenchmark = benchmarks.find((column) => column.ranked);
  const headline = observed?.ranked ? observed : (rankedBenchmark ?? null);
  const fallback = [observed, ...benchmarks]
    .filter((column): column is Column => column !== null)
    .sort((a, b) => b.n - a.n)[0];
  const shown = headline ?? fallback;
  const labelled =
    headline && headline.source === 'benchmark'
      ? `In the benchmark ${headline.benchmarkLabel ?? 'with no id'}: ${headline.why}`
      : shown.why;
  const out: ScoreboardRow = {
    key: row.key,
    model: row.model,
    label: shared.labels.get(row.model) ?? row.model,
    inCatalog: shared.labels.has(row.model),
    runtime: row.runtime,
    effort: row.effort,
    process: row.process,
    n: {
      observed: observed?.n ?? 0,
      benchmark: benchmarks.reduce((sum, column) => sum + column.n, 0),
    },
    basis: headline ? headline.source : null,
    benchmark: headline?.benchmark ?? null,
    ranked: headline !== null,
    rank: headline?.rank ?? null,
    verdict: shown.verdict,
    verdictLabel: shown.verdictLabel,
    why: labelled,
    observed,
    benchmarks,
  };
  return out;
}

/** Rows of one task type under one use: every comparison group judged apart. */
function judgedRows(
  base: readonly BaseRow[],
  type: CatalogTaskType,
  use: CatalogUse,
  shared: Shared
): ScoreboardRow[] {
  const judge = (columns: Column[]) =>
    judgeGroup(columns, use, type.measures, shared.defs, shared.minRuns);
  const observedIn = base.flatMap((row) => row.observed ?? []);
  const observedJudged = judge(observedIn);
  const observedOut = new Map(observedIn.map((column, i) => [column, observedJudged[i]]));
  const benchmarkOut = new Map<Column, Column>();
  const ids = new Set(base.flatMap((row) => row.benchmarks.map((column) => column.benchmark)));
  for (const id of ids) {
    const group = base.flatMap((row) => row.benchmarks.filter((column) => column.benchmark === id));
    judge(group).forEach((column, i) => benchmarkOut.set(group[i], column));
  }
  const rows = base.map((row) =>
    rowOf(
      row,
      row.observed ? (observedOut.get(row.observed) ?? null) : null,
      row.benchmarks.map((column) => benchmarkOut.get(column) ?? column),
      shared
    )
  );
  const order = (row: ScoreboardRow) =>
    row.basis === 'observed' ? 0 : row.basis === 'benchmark' ? 1 : 2;
  return rows.sort(
    (a, b) =>
      order(a) - order(b) ||
      (a.rank ?? 0) - (b.rank ?? 0) ||
      b.n.observed + b.n.benchmark - (a.n.observed + a.n.benchmark) ||
      a.key.localeCompare(b.key)
  );
}

function bump(map: Map<string, number>, key: string): void {
  map.set(key, (map.get(key) ?? 0) + 1);
}

export function buildScoreboardView(inputs: ScoreboardInputs): ScoreboardView {
  const { catalog, runs, now } = inputs;
  const docs = runs.runs.map((run) => run.doc);
  const index = buildReviewIndex(docs);
  const labels = new Map(catalog.models.map((model) => [model.model, model.label]));
  const shared: Shared = {
    index,
    defs: new Map(catalog.measures.map((measure) => [measure.id, measure])),
    labels,
    benchmarkLabels: new Map(catalog.benchmarks.map((item) => [item.id, item.label])),
    minRuns: catalog.minRuns,
  };
  const byType = new Map<string, RunDoc[]>();
  const known = new Set(catalog.taskTypes.map((type) => type.id));
  const missingTypes = new Map<string, number>();
  const missingModels = new Map<string, number>();
  let unscored = 0;
  for (const run of docs) {
    const model = modelOf(run);
    if (!model || !run.task.type) {
      unscored += 1;
      continue;
    }
    if (!known.has(run.task.type)) {
      bump(missingTypes, run.task.type);
      continue;
    }
    if (!labels.has(model)) bump(missingModels, model);
    const held = byType.get(run.task.type);
    if (held) held.push(run);
    else byType.set(run.task.type, [run]);
  }
  const base = new Map(
    catalog.taskTypes.map((type) => [type.id, baseRows(type, byType.get(type.id) ?? [], shared)])
  );
  const uses: ScoreboardUse[] = catalog.uses.map((use) => {
    const wanted = use.taskTypes.includes('*')
      ? catalog.taskTypes.filter((type) => byType.has(type.id))
      : use.taskTypes.flatMap((id) => catalog.taskTypes.find((type) => type.id === id) ?? []);
    const taskTypes: ScoreboardTaskType[] = wanted.map((type) => ({
      id: type.id,
      label: type.label,
      description: type.description,
      unit: type.unit,
      measures: type.measures,
      byProcess: type.byProcess,
      rows: judgedRows(base.get(type.id) ?? [], type, use, shared),
    }));
    return { id: use.id, label: use.label, description: use.description, taskTypes };
  });
  return {
    version: 1,
    generatedAt: new Date(now).toISOString(),
    refresh: { pollSeconds: SCOREBOARD_POLL_SECONDS },
    sources: [
      runsSource(inputs, unscored, index.ignored, unknownOutcomes(docs), negativeQuotaRuns(docs)),
      catalogSource(inputs, missingTypes, missingModels),
    ],
    catalog: {
      minRuns: catalog.minRuns,
      models: catalog.models,
      taskTypes: catalog.taskTypes,
      measures: catalog.measures,
      uses: catalog.uses,
      benchmarks: catalog.benchmarks,
    },
    uses,
    recent: [...docs]
      .sort(newestFirst)
      .slice(0, RECENT_RUNS)
      .map((run) => summary(run, labels)),
  };
}
