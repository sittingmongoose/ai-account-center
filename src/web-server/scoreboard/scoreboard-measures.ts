/**
 * The measures of one row's runs within one task type (docs/scoreboard.md, Measures). Pure.
 * Unknown stays null, never 0; every value carries n.
 */
import type { RunDoc } from '../status/report-schema';
import type { CatalogMeasure, TaskUnit } from './catalog';
import { median, significant, wilson90, roundOrNull } from './scoreboard-stats';
import type { CostRate, MeasureValue, Overturn, Stat } from './scoreboard-view-types';

export const CHEATING_KINDS: ReadonlySet<string> = new Set([
  'shortcut',
  'cheating',
  'fabricated',
  'test-weakened',
  'false-claim',
]);
export const LAZY_KINDS: ReadonlySet<string> = new Set(['incomplete-read', 'scope-cut']);
/** Outcomes that count against reliability; the format has no reason field. */
export const FAILED_OUTCOMES: ReadonlySet<string> = new Set([
  'failed',
  'failure',
  'cancelled',
  'canceled',
  'did-not-start',
  'not-started',
  'error',
  'crashed',
  'timed-out',
  'timeout',
  'aborted',
  'killed',
  'interrupted',
  'abandoned',
]);
const READ_FLOOR = 0.9;

/**
 * `done` is the only success and the listed words the only failures. Anything else (`partial`,
 * `blocked`, a word this server has never seen) says neither, so it counts as neither.
 */
export function outcomeOf(run: RunDoc): 'done' | 'failed' | 'unknown' {
  const word = run.outcome.trim().toLowerCase().replace(/_/g, '-');
  if (word === 'done') return 'done';
  return FAILED_OUTCOMES.has(word) ? 'failed' : 'unknown';
}

/** The runs left out of a quota rate for a negative move, for the runs Source. */
export function negativeQuotaRuns(runs: readonly RunDoc[]): string[] {
  return runs
    .filter(
      (run) =>
        (run.cost?.quota?.deltaPct ?? 0) < 0 || (run.cost?.estimate?.quota?.deltaPct ?? 0) < 0
    )
    .map((run) => run.id);
}

/** The runs with an unknown outcome, by outcome word, for the runs Source. */
export function unknownOutcomes(runs: readonly RunDoc[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const run of runs) {
    if (outcomeOf(run) !== 'unknown') continue;
    const word = run.outcome.replace(/\s+/g, ' ').trim().toLowerCase();
    counts.set(word, (counts.get(word) ?? 0) + 1);
  }
  return counts;
}

/** The reviews that count: `task.type` review, naming a known run, not ending before that run. */
export interface ReviewIndex {
  of(runId: string): RunDoc[];
  /** Runs with a review record that do not count: wrong task type, unknown or later target. */
  ignored: number;
}

export function buildReviewIndex(runs: readonly RunDoc[]): ReviewIndex {
  const known = new Map(runs.map((run) => [run.id, run]));
  const byTarget = new Map<string, RunDoc[]>();
  let ignored = 0;
  for (const run of runs) {
    if (!run.review) continue;
    const target = known.get(run.review.of);
    if (
      run.task.type !== 'review' ||
      !target ||
      target.id === run.id ||
      Date.parse(run.endedAt) < Date.parse(target.endedAt)
    ) {
      ignored += 1;
      continue;
    }
    const held = byTarget.get(target.id);
    if (held) held.push(run);
    else byTarget.set(target.id, [run]);
  }
  for (const reviews of byTarget.values())
    reviews.sort((a, b) => Date.parse(a.endedAt) - Date.parse(b.endedAt));
  return { of: (runId) => byTarget.get(runId) ?? [], ignored };
}

function overturnedUndisputed(review: RunDoc): boolean {
  return review.review?.overturned === true && !review.review.disputed;
}

/** A review that decides something; `cannot-verify` does not. */
export function decisive(review: RunDoc): boolean {
  return review.review?.verdict !== 'cannot-verify';
}

/** A run's size in the task type's unit; a run is one task unless it says otherwise. */
export function unitsOf(run: RunDoc, unit: TaskUnit): number | null {
  const size = run.task.size?.[`${unit}s`] ?? null;
  if (size === null) return unit === 'task' ? 1 : null;
  return size > 0 ? size : null;
}

function ratio(done: number | null | undefined, assigned: number | null | undefined) {
  if (done === null || done === undefined || !assigned) return null;
  return Math.min(1, done / assigned);
}

function filesRatio(run: RunDoc): number | null {
  return ratio(run.scope?.filesRead, run.scope?.filesAssigned);
}

function recordsRatio(run: RunDoc): number | null {
  return ratio(run.scope?.recordsJudged, run.scope?.recordsAssigned);
}

/** What the run read of what it was assigned: the lower of its two ratios when both are known. */
export function readRatio(run: RunDoc): number | null {
  const known = [filesRatio(run), recordsRatio(run)].flatMap((value) => value ?? []);
  return known.length > 0 ? Math.min(...known) : null;
}

function stat(values: readonly number[]): Stat {
  return {
    value: roundOrNull(median(values)),
    n: values.length,
    low: values.length > 0 ? significant(Math.min(...values)) : null,
    high: values.length > 0 ? significant(Math.max(...values)) : null,
  };
}

function blank(unit: string): MeasureValue {
  return {
    value: null,
    unit,
    n: 0,
    low: null,
    high: null,
    interval: null,
    of: null,
    per: null,
    preliminary: false,
  };
}

function rangeValue(unit: string, values: readonly number[]): MeasureValue {
  const median = stat(values);
  return {
    ...blank(unit),
    ...median,
    interval: values.length > 0 ? 'range' : null,
  };
}

function share(unit: string, of: number, per: number, n: number): MeasureValue {
  if (!(per > 0)) return { ...blank(unit), n };
  const interval = wilson90(of, per);
  return {
    ...blank(unit),
    value: significant(of / per),
    n,
    low: interval ? significant(interval.low) : null,
    high: interval ? significant(interval.high) : null,
    interval: interval ? 'wilson90' : null,
    of: significant(of, 10),
    per: significant(per, 10),
  };
}

function speed(runs: readonly RunDoc[], unit: TaskUnit): MeasureValue {
  const finished = runs.filter((run) => outcomeOf(run) === 'done' && run.wallSeconds !== null);
  const perUnit: number[] = [];
  for (const run of finished) {
    const units = unitsOf(run, unit);
    if (units !== null) perUnit.push((run.wallSeconds as number) / units);
  }
  return {
    ...rangeValue(`s/${unit}`, perUnit),
    perTask: stat(finished.map((run) => run.wallSeconds as number)),
  };
}

/**
 * Share of items a blind review passed. Fix rounds on one item (same project and task id) count
 * once, by the item's latest decisive review. An item passes only when that review's `verdict` is
 * `pass` and it was not overturned: a `fail` counts as not passed whatever `overturned` says
 * (`agent-report run --verdict fail` writes `overturned: false`). A latest review that is a
 * disputed overturn leaves the item out of the share, counted as disputed. `cannot-verify` decides
 * nothing.
 */
function quality(runs: readonly RunDoc[], index: ReviewIndex, unit: string): MeasureValue {
  const items = new Map<string, { run: RunDoc; review: RunDoc }[]>();
  let cannotVerify = 0;
  const overturns: Overturn[] = [];
  for (const run of runs) {
    const key = JSON.stringify([run.project, run.task.id]);
    for (const review of index.of(run.id)) {
      if (!decisive(review)) {
        cannotVerify += 1;
        continue;
      }
      const held = items.get(key);
      if (held) held.push({ run, review });
      else items.set(key, [{ run, review }]);
      if (review.review?.overturned)
        overturns.push({
          run: run.id,
          review: review.id,
          by: review.review.by ?? reviewerModel(review),
          disputed: review.review.disputed,
          evidence: review.evidence[0]?.path ?? null,
        });
    }
  }
  let passed = 0;
  let failed = 0;
  let disputed = 0;
  const rounds: number[] = [];
  const firstPass = ({ review }: { review: RunDoc }) =>
    review.review?.verdict === 'pass' && !review.review.overturned;
  for (const reviews of items.values()) {
    reviews.sort((a, b) => Date.parse(a.review.endedAt) - Date.parse(b.review.endedAt));
    const last = reviews[reviews.length - 1].review.review;
    if (last?.overturned && last.disputed) {
      disputed += 1;
    } else if (last?.verdict === 'fail' || last?.overturned) {
      failed += 1;
    } else {
      passed += 1;
      rounds.push(reviews.findIndex(firstPass) + 1);
    }
  }
  return {
    ...share(unit, passed, passed + failed, passed + failed),
    disputed,
    cannotVerify,
    rounds: stat(rounds),
    overturns,
  };
}

function reviewerModel(review: RunDoc): string | null {
  const { provider, id, effort } = review.model;
  if (!id) return null;
  const model = canonicalModel(provider, id);
  return effort ? `${model} ${effort}` : model;
}

/** `provider/id`, or the id alone when it already starts with `<provider>/` or names no provider. */
export function canonicalModel(provider: string | null, id: string): string {
  return !provider || id.startsWith(`${provider}/`) ? id : `${provider}/${id}`;
}

function thoroughness(runs: readonly RunDoc[], unit: string): MeasureValue {
  const ratios = runs.flatMap((run) => {
    const value = readRatio(run);
    return value === null ? [] : [value];
  });
  const of = (read: (run: RunDoc) => number | null) =>
    stat(
      runs.flatMap((run) => {
        const value = read(run);
        return value === null ? [] : [value];
      })
    );
  return { ...rangeValue(unit, ratios), files: of(filesRatio), records: of(recordsRatio) };
}

/** A run with no flag and no known read ratio is not evidence either way, so it is left out. */
function laziness(runs: readonly RunDoc[], unit: string): MeasureValue {
  let lazy = 0;
  let counted = 0;
  for (const run of runs) {
    const flagged = run.flags.some((flag) => LAZY_KINDS.has(flag.kind));
    const read = readRatio(run);
    if (!flagged && read === null) continue;
    counted += 1;
    if (flagged || (read as number) < READ_FLOOR) lazy += 1;
  }
  return share(unit, lazy, counted, counted);
}

function cheating(runs: readonly RunDoc[], unit: string): MeasureValue {
  const flagged = runs.filter((run) => run.flags.some((flag) => CHEATING_KINDS.has(flag.kind)));
  return share(unit, flagged.length, runs.length, runs.length);
}

function reliability(runs: readonly RunDoc[], unit: string): MeasureValue {
  const known = runs.filter((run) => outcomeOf(run) !== 'unknown');
  const good = known.filter((run) => outcomeOf(run) === 'done');
  return share(unit, good.length, known.length, known.length);
}

/**
 * Of the runs that reported done and were reviewed, the share a review overturned. The format has
 * no field for claimed checks, so outcome `done` is the claim. Disputed overturns are left out.
 */
function overclaim(runs: readonly RunDoc[], index: ReviewIndex, unit: string): MeasureValue {
  let claimed = 0;
  let overturned = 0;
  for (const run of runs) {
    if (outcomeOf(run) !== 'done') continue;
    const reviews = index.of(run.id).filter(decisive);
    const standing = reviews.filter(
      (review) => !(review.review?.overturned && review.review.disputed)
    );
    if (standing.length === 0) continue;
    claimed += 1;
    if (standing.some(overturnedUndisputed)) overturned += 1;
  }
  return share(unit, overturned, claimed, claimed);
}

/** Σ cost over the accepted units of the runs whose cost is known. */
function rate(
  runs: readonly RunDoc[],
  unit: TaskUnit,
  index: ReviewIndex,
  costOf: (run: RunDoc) => number | null,
  label: string
): CostRate {
  let total = 0;
  let accepted = 0;
  let n = 0;
  for (const run of runs) {
    const cost = costOf(run);
    const units = unitsOf(run, unit);
    if (cost === null || units === null) continue;
    n += 1;
    // Spent is spent, whatever became of the run; only `done` work that stood is what it bought.
    total += cost;
    if (outcomeOf(run) === 'done' && !index.of(run.id).some(overturnedUndisputed))
      accepted += units;
  }
  return {
    value: n > 0 && accepted > 0 ? significant(total / accepted) : null,
    unit: `${label}/${unit}`,
    n,
  };
}

function quotaRates(
  runs: readonly RunDoc[],
  unit: TaskUnit,
  index: ReviewIndex,
  quotaOf: (run: RunDoc) => { window: string; deltaPct: number } | null | undefined
): Array<CostRate & { window: string }> {
  const windows = [...new Set(runs.flatMap((run) => quotaOf(run)?.window ?? []))].sort();
  return windows.map((window) => ({
    window,
    ...rate(
      runs,
      unit,
      index,
      (run) => {
        const quota = quotaOf(run);
        // A negative move is a window that reset during the run, not a cost.
        return quota?.window === window && quota.deltaPct >= 0 ? quota.deltaPct : null;
      },
      '%'
    ),
  }));
}

function cost(runs: readonly RunDoc[], unit: TaskUnit, index: ReviewIndex): MeasureValue {
  const credits = rate(runs, unit, index, (run) => run.cost?.credits ?? null, 'credits');
  const usd = rate(runs, unit, index, (run) => run.cost?.usd ?? null, 'usd');
  const estimated = runs.filter((run) => run.cost?.estimate);
  const estimateCredits = rate(
    runs,
    unit,
    index,
    (run) => run.cost?.estimate?.credits ?? null,
    'credits'
  );
  return {
    ...blank(credits.unit),
    value: credits.value,
    n: credits.n,
    quota: quotaRates(runs, unit, index, (run) => run.cost?.quota),
    usd: usd.n > 0 ? usd : null,
    estimate:
      estimated.length > 0
        ? {
            label: 'estimate',
            credits: estimateCredits.n > 0 ? estimateCredits : null,
            quota: quotaRates(runs, unit, index, (run) => run.cost?.estimate?.quota),
            methods: [...new Set(estimated.map((run) => run.cost?.estimate?.method ?? ''))].sort(),
            sources: [...new Set(estimated.map((run) => run.cost?.estimate?.source ?? ''))].sort(),
          }
        : null,
  };
}

/** Σof / Σper over the runs where both are known and make sense. */
function metric(
  runs: readonly RunDoc[],
  def: CatalogMeasure,
  names: { of: string; per: string }
): { value: MeasureValue; behind: RunDoc[] } {
  let of = 0;
  let per = 0;
  const behind: RunDoc[] = [];
  for (const run of runs) {
    const top = run.metrics[names.of] ?? null;
    const bottom = run.metrics[names.per] ?? null;
    if (top === null || bottom === null || top < 0 || bottom <= 0) continue;
    of += top;
    per += bottom;
    behind.push(run);
  }
  return { value: share(def.unit, of, per, behind.length), behind };
}

function preliminary(def: CatalogMeasure, behind: readonly RunDoc[]): boolean {
  const name = def.preliminaryUnless;
  return name !== null && behind.some((run) => run.metrics[name] !== 1);
}

/** Every measure a task type lists, by id. A listed measure the catalog does not define is left out. */
export function computeMeasures(
  runs: readonly RunDoc[],
  unit: TaskUnit,
  listed: readonly string[],
  defs: ReadonlyMap<string, CatalogMeasure>,
  index: ReviewIndex
): Record<string, MeasureValue> {
  const out: Record<string, MeasureValue> = {};
  for (const id of listed) {
    const def = defs.get(id);
    if (!def) continue;
    let value: MeasureValue;
    let behind: readonly RunDoc[] = runs;
    if (def.metric) {
      const read = metric(runs, def, def.metric);
      value = read.value;
      behind = read.behind;
    } else if (id === 'speed') value = speed(runs, unit);
    else if (id === 'quality') value = quality(runs, index, def.unit);
    else if (id === 'thoroughness') value = thoroughness(runs, def.unit);
    else if (id === 'laziness') value = laziness(runs, def.unit);
    else if (id === 'cheating') value = cheating(runs, def.unit);
    else if (id === 'cost') value = cost(runs, unit, index);
    else if (id === 'reliability') value = reliability(runs, def.unit);
    else if (id === 'overclaim') value = overclaim(runs, index, def.unit);
    else value = blank(def.unit);
    out[id] = { ...value, preliminary: preliminary(def, behind) };
  }
  return out;
}
