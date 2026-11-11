/**
 * Ranking and verdicts (docs/scoreboard.md, Verdicts). Pure: columns come in unjudged and go out with
 * rank, score, verdict and reason, for one use and one comparison group (observed, or one benchmark
 * of one task type). A verdict's reason is one plain line and always says n.
 */
import type { CatalogMeasure, CatalogUse } from './catalog';
import { significant } from './scoreboard-stats';
import type { Column, MeasureValue, Verdict } from './scoreboard-view-types';

export const SCORE_RECOMMENDED = 0.8;
export const SCORE_AVOID = 0.6;
export const TOP_GAP = 0.05;
export const CHEATING_FLOOR = 0.2;
export const COVERAGE_FLOOR = 0.5;
export const HEAVY_WEIGHT = 3;
const HEAVY_FLOOR = 0.5;
/** A ranked column shows only its newest runs; an unranked one shows them all (up to the view's cap). */
const RANKED_COLUMN_RUNS = 5;

export const VERDICT_LABELS: Readonly<Record<Verdict, string>> = Object.freeze({
  recommended: 'Recommended',
  fine: 'Fine',
  avoid: 'Avoid for this',
  'not-enough-data': 'Not enough data yet',
});

/** The reason a column misses the bar, or null when it passes. */
export function belowBar(column: Pick<Column, 'n' | 'source' | 'reviews'>, minRuns: number) {
  if (column.n < minRuns) return `too little data (n = ${column.n})`;
  if (column.source === 'observed' && column.reviews === 0)
    return `no blind review yet (n = ${column.n})`;
  return null;
}

/**
 * The reviewed count behind the bar: the pass rate's decided item count when the column carries a
 * quality measure — fix rounds on one item count once — else the count of decisive review runs.
 * A task type with no quality pass rate (like plan) has no item share for the bar to protect,
 * so it keeps the review-run count.
 */
function reviewedCount(column: Pick<Column, 'reviews' | 'measures'>): number {
  return column.measures.quality?.per ?? column.reviews;
}

/**
 * The review bar a Recommended or Avoid verdict must clear: one reviewed item alone never lifts
 * a row to either (a 1-of-1 quality share reads 100%), so an observed column needs `minRuns`
 * reviewed items — the same bar runs already follow. Five fix rounds on one item are one item,
 * not five. A Fine can stand on fewer; a benchmark column needs no review.
 */
function belowReviewBar(
  column: Pick<Column, 'source' | 'reviews' | 'measures'>,
  minRuns: number
): boolean {
  if (column.source !== 'observed') return false;
  // Cheating flags are per-run and do not depend on reviews: a cheating-floor Avoid stands
  // on any number of reviewed items, and its cell keeps the "cheated in … runs" wording.
  const cheatingValue = column.measures.cheating?.value;
  if (typeof cheatingValue === 'number' && cheatingValue >= CHEATING_FLOOR) return false;
  return reviewedCount(column) < minRuns;
}

interface Scored {
  column: Column;
  score: number;
  coverage: number;
  marks: Array<{ def: CatalogMeasure; weight: number; mark: number; value: MeasureValue }>;
}

function isShare(def: CatalogMeasure): boolean {
  return def.unit === 'share';
}

function markOf(def: CatalogMeasure, value: number, best: number): number {
  if (isShare(def)) {
    const clamped = Math.min(1, Math.max(0, value));
    return def.better === 'higher' ? clamped : 1 - clamped;
  }
  if (def.better === 'lower') return value === 0 ? 1 : best / value;
  return best === 0 ? 1 : value / best;
}

function pct(value: number): number {
  return Math.round(value * 100);
}

function judge(entry: Scored, top: number, peers: number): { verdict: Verdict; why: string } {
  const { column, score, coverage, marks } = entry;
  const n = column.n;
  const cheating = marks.find((mark) => mark.def.id === 'cheating');
  if (cheating && (cheating.value.value ?? 0) >= CHEATING_FLOOR)
    return {
      verdict: 'avoid',
      why: `Flagged for cheating in ${cheating.value.of} of ${cheating.value.per} runs (n = ${n}).`,
    };
  const weakest = [...marks].sort((a, b) => a.mark - b.mark)[0];
  const heavy = marks
    .filter((mark) => isShare(mark.def) && mark.weight >= HEAVY_WEIGHT && mark.mark < HEAVY_FLOOR)
    .sort((a, b) => a.mark - b.mark)[0];
  if (heavy)
    return {
      verdict: 'avoid',
      why: `${heavy.def.label} is ${pct(heavy.value.value ?? 0)}% (n = ${heavy.value.n}).`,
    };
  if (score < SCORE_AVOID)
    return {
      verdict: 'avoid',
      why: `Scores ${significant(score, 2)} against ${significant(top, 2)} for the best row, weakest on ${weakest.def.label} (n = ${n}).`,
    };
  if (peers < 2)
    return {
      verdict: 'fine',
      why: `The only ranked row here, so nothing to compare it with (n = ${n}).`,
    };
  if (coverage < COVERAGE_FLOOR)
    return { verdict: 'fine', why: `Too few measures known to recommend it (n = ${n}).` };
  if (score >= SCORE_RECOMMENDED && score >= top - TOP_GAP) {
    const praised = marks
      .filter((mark) => mark.def.better === 'higher' || !isShare(mark.def))
      .sort((a, b) => b.mark * b.weight - a.mark * a.weight)[0];
    const clean = cheating?.value.value === 0 ? ', with no cheating flags' : '';
    return {
      verdict: 'recommended',
      why: `${praised ? `Strongest on ${praised.def.label}` : 'Top marks'}${clean} (n = ${n}).`,
    };
  }
  return {
    verdict: 'fine',
    why: `Behind the top row, weakest on ${weakest.def.label} (n = ${n}).`,
  };
}

function unranked(column: Column, why: string): Column {
  return {
    ...column,
    ranked: false,
    rank: null,
    score: null,
    verdict: 'not-enough-data',
    verdictLabel: VERDICT_LABELS['not-enough-data'],
    why,
  };
}

/** One comparison group, judged under one use's weights. Returns the columns in the same order. */
export function judgeGroup(
  columns: readonly Column[],
  use: Pick<CatalogUse, 'weights'>,
  listed: readonly string[],
  defs: ReadonlyMap<string, CatalogMeasure>,
  minRuns: number
): Column[] {
  const weighted = listed.flatMap((id) => {
    const def = defs.get(id);
    const weight = use.weights[id] ?? 0;
    return def && weight > 0 ? [{ def, weight }] : [];
  });
  const totalWeight = weighted.reduce((sum, { weight }) => sum + weight, 0);
  const eligible = columns.filter((column) => belowBar(column, minRuns) === null);
  const best = new Map<string, number>();
  for (const { def } of weighted) {
    const known = eligible.flatMap((column) => column.measures[def.id]?.value ?? []);
    if (known.length > 0)
      best.set(def.id, def.better === 'lower' ? Math.min(...known) : Math.max(...known));
  }
  const scored: Scored[] = [];
  for (const column of eligible) {
    const marks: Scored['marks'] = [];
    for (const { def, weight } of weighted) {
      const value = column.measures[def.id];
      if (value?.value === null || value?.value === undefined) continue;
      marks.push({ def, weight, value, mark: markOf(def, value.value, best.get(def.id) ?? 0) });
    }
    const knownWeight = marks.reduce((sum, mark) => sum + mark.weight, 0);
    if (knownWeight === 0) continue;
    scored.push({
      column,
      marks,
      coverage: knownWeight / totalWeight,
      score: marks.reduce((sum, mark) => sum + mark.mark * mark.weight, 0) / knownWeight,
    });
  }
  scored.sort(
    (a, b) =>
      b.score - a.score ||
      b.column.n - a.column.n ||
      JSON.stringify(a.column.runs[0]?.id).localeCompare(JSON.stringify(b.column.runs[0]?.id))
  );
  // Settle the review bar before the comparison: a row the bar demotes must not set
  // the top score, the peer count or the ranks of the rows that stay. Pull every
  // demoted row out and judge the rest again, until a pass pulls nothing (each pass
  // pulls at least one, so at most scored.length passes).
  const judged = new Map<Column, Column>();
  let remaining = scored;
  while (remaining.length > 0) {
    const top = remaining[0].score;
    const peers = remaining.length;
    const pulled = remaining.filter((entry) => {
      const { verdict } = judge(entry, top, peers);
      return (
        (verdict === 'recommended' || verdict === 'avoid') && belowReviewBar(entry.column, minRuns)
      );
    });
    if (pulled.length === 0) break;
    for (const entry of pulled) {
      judged.set(
        entry.column,
        unranked(
          entry.column,
          `too few blind reviews yet (${reviewedCount(entry.column)} of ${minRuns} needed, n = ${entry.column.n})`
        )
      );
    }
    remaining = remaining.filter((entry) => !pulled.includes(entry));
  }
  const top = remaining[0]?.score ?? 0;
  remaining.forEach((entry, position) => {
    const { verdict, why } = judge(entry, top, remaining.length);
    const preliminary = entry.marks.some((mark) => mark.value.preliminary);
    judged.set(entry.column, {
      ...entry.column,
      ranked: true,
      rank: position + 1,
      score: significant(entry.score, 3),
      verdict,
      verdictLabel: VERDICT_LABELS[verdict],
      why: preliminary ? `${why} Preliminary.` : why,
      runs: entry.column.runs.slice(0, RANKED_COLUMN_RUNS),
    });
  });
  return columns.map(
    (column) =>
      judged.get(column) ??
      unranked(
        column,
        belowBar(column, minRuns) ?? `None of this use's measures is known yet (n = ${column.n}).`
      )
  );
}
