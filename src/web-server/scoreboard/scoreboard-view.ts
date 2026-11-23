/**
 * Builds the Model Scoreboard view from the runs and the catalog (docs/scoreboard.md). Pure: every
 * number, word and verdict the page draws is here. A row is model + runtime + effort, plus process
 * when the task type says so, within one task type; observed and benchmark runs stay in separate
 * columns and are only ever compared with their own kind (and their own benchmark).
 */
import { normalizeModel } from './model-identity';
import type { ReportProblem } from '../status/agent-reports-reader';
import type { RunDoc } from '../status/report-schema';
import type { SourceState } from '../status/status-view-types';
import { ageSeconds, iso } from '../status/status-view-words';
import { resolveRunsState } from './run-corrections';
import type { RunsState } from './runs-reader';
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
  UnscoredExample,
  UnscoredReason,
  UnscoredSummary,
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
  /** A review file (or its dated folder) this scan could not read: its first failure time. */
  reviewSource: RowSourceHealth | null;
  /** The catalog listing failed this scan: ranked verdicts still rest on the retained catalog. */
  catalogHealth: RowSourceHealth | null;
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

interface RowSourceHealth {
  sourceState: SourceState;
  asOf: string | null;
  ageSeconds: number | null;
}

/**
 * Per-row source health traced strictly from this row's own run files against `runs.problems`.
 * Only problems under `writers/` (a run file or its parent folder) can match a row's files;
 * never-read damaged files or scan limits leave every row untouched. The age is since the file
 * first failed (`problem.since`), never the folder scan time.
 */
function tracedRunProblem(
  files: readonly string[],
  problems: readonly ReportProblem[],
  now: number
): RowSourceHealth | null {
  let matched = false;
  let earliest: number | null = null;
  for (const problem of problems) {
    if (!problem.file.startsWith('writers/')) continue;
    const prefix = `${problem.file}/`;
    if (!files.some((file) => file === problem.file || file.startsWith(prefix))) continue;
    matched = true;
    if (
      typeof problem.since === 'number' &&
      Number.isFinite(problem.since) &&
      (earliest === null || problem.since < earliest)
    ) {
      earliest = problem.since;
    }
  }
  if (!matched) return null;
  return {
    sourceState: 'unreadable',
    asOf: iso(earliest),
    ageSeconds: ageSeconds(now, earliest),
  };
}

/**
 * A review file, or the dated folder holding it, that this scan could not read. The review keeps
 * its last good copy and still joins, so every verdict that used at least one joined review
 * carries the review source's failure and its first-observed time. With no finer provenance than
 * the file, this does not try to trace which verdict used which review (2026-11-23 A).
 */
function reviewSourceHealth(
  files: readonly string[],
  problems: readonly ReportProblem[],
  now: number
): RowSourceHealth | null {
  let matched = false;
  let earliest: number | null = null;
  for (const file of files) {
    if (!file.startsWith('writers/')) continue;
    for (const problem of problems) {
      if (!problem.file.startsWith('writers/')) continue;
      const prefix = `${problem.file}/`;
      if (file !== problem.file && !file.startsWith(prefix)) continue;
      matched = true;
      if (
        typeof problem.since === 'number' &&
        Number.isFinite(problem.since) &&
        (earliest === null || problem.since < earliest)
      )
        earliest = problem.since;
    }
  }
  if (!matched) return null;
  return {
    sourceState: 'unreadable',
    asOf: iso(earliest),
    ageSeconds: ageSeconds(now, earliest),
  };
}

/**
 * A ranked verdict rests on the retained catalog. A failed listing is its own known failure; a
 * listing that succeeded can still leave a file failed, and the reader keeps that file's last
 * good copy while the ranking uses it. Both disclose the catalog's failure, from its first
 * reported time — the listing's `failedSince`, or the earliest file problem's `since`.
 */
function catalogVerdictHealth(
  catalog: ScoreboardInputs['catalog'],
  now: number
): RowSourceHealth | null {
  if (catalog.state === 'error') {
    // The first time this listing failed, kept across failed scans by the reader; an older input
    // without the fact keeps its last good read rather than inventing a time.
    const at =
      typeof catalog.failedSince === 'number'
        ? catalog.failedSince
        : typeof catalog.scannedAt === 'number'
          ? catalog.scannedAt
          : null;
    return { sourceState: 'error', asOf: iso(at), ageSeconds: ageSeconds(now, at) };
  }
  if (catalog.state !== 'ok') return null;
  let earliest: number | null = null;
  for (const problem of catalog.problems) {
    if (
      typeof problem.since === 'number' &&
      Number.isFinite(problem.since) &&
      (earliest === null || problem.since < earliest)
    )
      earliest = problem.since;
  }
  if (earliest === null) return null;
  return {
    sourceState: 'unreadable',
    asOf: iso(earliest),
    ageSeconds: ageSeconds(now, earliest),
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
  sourceHealth: RowSourceHealth | null;
}

/** The rows of one task type: one per identity, with a column per kind of data. */
function baseRows(
  type: CatalogTaskType,
  entries: ReadonlyArray<{ doc: RunDoc; file: string }>,
  shared: Shared,
  problems: readonly ReportProblem[],
  now: number
): BaseRow[] {
  const groups = new Map<string, { row: BaseRow; runs: RunDoc[]; files: string[] }>();
  for (const { doc: run, file } of entries) {
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
          sourceHealth: null,
        },
        runs: [],
        files: [],
      };
      groups.set(id, group);
    }
    group.runs.push(run);
    group.files.push(file);
  }
  return [...groups.values()].map(({ row, runs: rowRuns, files }) => {
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
      // A row whose ranking used at least one joined review rests on the review source.
      sourceHealth:
        tracedRunProblem(files, problems, now) ??
        (rowRuns.some((run) => shared.index.of(run.id).length > 0) ? shared.reviewSource : null),
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
  // A ranked verdict compared the retained catalog with the runs: a listing that failed this
  // scan is disclosed on it. A row's own failure is already named and stays the row's.
  const health = row.sourceHealth ?? (headline !== null ? shared.catalogHealth : null);
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
    ...(health ? health : {}),
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

/**
 * Run files the runs reader refused (each named in the runs Source and refused in the readers
 * check) that left no good copy to score. A refused newer version of a run whose earlier copy
 * still reads is scored from that copy and not counted — whether that copy is one of the view's
 * rows or a retained review joined from outside its filter; the runs under a folder the reader
 * could not list count under the scan limit instead, from the folder's last good listing (a
 * lower bound), with the folder problem naming why.
 */
function unreadableRuns(runs: RunsState, joinReviewFiles: readonly (string | null)[] = []): number {
  const read = new Set(runs.runs.map((run) => run.file));
  for (const file of joinReviewFiles) if (typeof file === 'string') read.add(file);
  return runs.problems.filter(
    (problem) => /^writers\/[^/]+\/runs\/.+\.json$/.test(problem.file) && !read.has(problem.file)
  ).length;
}

/**
 * Run files past any scan cap (run files, writers or bytes) or under a folder this scan could not
 * list, from the reader's structured count. The newest days, and the writers with the newest
 * run-date folders, are kept; past the writer cap the count is a lower bound, and under an
 * unlistable folder a provisional last-known count. The runs Source names and qualifies both.
 */
function scanLimitRuns(runs: RunsState): number {
  return runs.skipped?.runFiles ?? 0;
}

export function buildScoreboardView(inputs: ScoreboardInputs): ScoreboardView {
  inputs = { ...inputs, runs: resolveRunsState(inputs.runs) };
  const { catalog, runs, now } = inputs;
  // The alias table applies to every run read, history included, before scoring: a run keeps its
  // raw written id only when it resolves to no catalog id.
  const rawDocs = runs.runs.map(({ doc }) => doc);
  const docs = rawDocs.map((doc) => {
    if (!doc.model.id) return doc;
    const identity = normalizeModel(doc.model.id, doc.model.effort, catalog.models, {
      aliases: catalog.aliases,
      effortSuffixes: catalog.effortSuffixes,
    });
    return {
      ...doc,
      model: { ...doc.model, id: identity.id, effort: identity.effort, provider: null },
    };
  });
  const labels = new Map(catalog.models.map((model) => [model.model, model.label]));
  const knownTypes = new Set(catalog.taskTypes.map((type) => type.id));
  const counts = { noModel: 0, noTaskType: 0, unknownModel: 0, unknownTaskType: 0 };
  const unknownCounts = new Map<string, number>();
  const examplesAll: UnscoredExample[] = [];
  // What the catalog Source names: task types not in the catalog (any model), and models not in
  // it (under a catalog task type).
  const missingTypes = new Map<string, number>();
  const missingModels = new Map<string, number>();
  // Only runs with a catalog model and a catalog task type form rows and verdicts; every other
  // run is counted under one reason in `unscored` and left out, as the page says.
  const byType = new Map<string, Array<{ doc: RunDoc; file: string }>>();
  for (let i = 0; i < rawDocs.length; i++) {
    const raw = rawDocs[i];
    const run = docs[i];
    const model = modelOf(run);
    const type = raw.task.type;
    if (model && type && !knownTypes.has(type)) bump(missingTypes, type);
    else if (model && type && !labels.has(model)) bump(missingModels, model);
    let reason: UnscoredReason | null = null;
    if (!raw.model.id) reason = 'no-model';
    else if (!type) reason = 'no-task-type';
    else if (!model || !labels.has(model)) reason = 'unknown-model';
    else if (!knownTypes.has(type)) reason = 'unknown-task-type';
    if (reason === null && type) {
      const entry = { doc: run, file: runs.runs[i].file };
      const held = byType.get(type);
      if (held) held.push(entry);
      else byType.set(type, [entry]);
      continue;
    }
    if (reason === 'no-model') counts.noModel += 1;
    else if (reason === 'no-task-type') counts.noTaskType += 1;
    else if (reason === 'unknown-model') {
      counts.unknownModel += 1;
      if (raw.model.id) bump(unknownCounts, raw.model.id);
    } else counts.unknownTaskType += 1;
    if (reason)
      examplesAll.push({
        id: raw.id,
        model: raw.model.id,
        taskType: raw.task.type,
        reason,
        endedAt: raw.endedAt,
      });
  }
  const unreadable = unreadableRuns(runs, inputs.joinReviewFiles ?? []);
  const scanLimit = scanLimitRuns(runs);
  const unscoredSummary: UnscoredSummary = {
    total:
      scanLimit +
      unreadable +
      counts.noModel +
      counts.noTaskType +
      counts.unknownModel +
      counts.unknownTaskType,
    scanLimit,
    unreadable,
    noModel: counts.noModel,
    noTaskType: counts.noTaskType,
    unknownModel: counts.unknownModel,
    unknownTaskType: counts.unknownTaskType,
    unknownModels: [...unknownCounts]
      .map(([id, runs]) => ({ id, runs }))
      .sort((a, b) => b.runs - a.runs || a.id.localeCompare(b.id))
      .slice(0, 20),
    examples: [...examplesAll]
      .sort((a, b) => Date.parse(b.endedAt) - Date.parse(a.endedAt) || a.id.localeCompare(b.id))
      .slice(0, 10),
  };
  // A review counts toward the runs it reviews only when its own run is scored — the same
  // catalog model and task type a row needs. A reviewer with no model or task type, or one the
  // catalog does not list, is not evidence: its verdict moves no blind-review measure (sb-R3).
  // Voided and superseded runs never reach the index at all (both paths resolve corrections first).
  const scoredReviewer = (run: RunDoc): boolean => {
    if (!run.model.id) return false;
    const type = run.task.type;
    if (!type || !knownTypes.has(type)) return false;
    const { id } = normalizeModel(run.model.id, run.model.effort, catalog.models, {
      aliases: catalog.aliases,
      effortSuffixes: catalog.effortSuffixes,
    });
    return labels.has(id);
  };
  const index = buildReviewIndex(docs, inputs.joinReviews ?? [], scoredReviewer);
  // Every review source this scan can name: the view's own review runs, and reviews joined from
  // outside its filter (whose file the owner supplies alongside the doc).
  const reviewFiles: string[] = [];
  for (const [i, doc] of docs.entries()) {
    const file = runs.runs[i]?.file;
    if (doc.review && file) reviewFiles.push(file);
  }
  for (const [i, doc] of (inputs.joinReviews ?? []).entries()) {
    const file = inputs.joinReviewFiles?.[i];
    if (doc.review && typeof file === 'string') reviewFiles.push(file);
  }
  const reviewSource = reviewSourceHealth(reviewFiles, runs.problems, now);
  const shared: Shared = {
    index,
    defs: new Map(catalog.measures.map((measure) => [measure.id, measure])),
    labels,
    benchmarkLabels: new Map(catalog.benchmarks.map((item) => [item.id, item.label])),
    minRuns: catalog.minRuns,
    reviewSource,
    catalogHealth: catalogVerdictHealth(catalog, now),
  };
  const base = new Map(
    catalog.taskTypes.map((type) => [
      type.id,
      baseRows(type, byType.get(type.id) ?? [], shared, runs.problems, now),
    ])
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
      runsSource(
        inputs,
        unscoredSummary.noModel + unscoredSummary.noTaskType,
        index.ignored,
        unknownOutcomes(docs),
        negativeQuotaRuns(docs)
      ),
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
    unscored: unscoredSummary,
  };
}
