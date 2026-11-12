/** The Scoreboard's two Sources (the shape in docs/status-tab.md): what was read, and what was not. */
import type { ReportProblem } from '../status/agent-reports-reader';
import { ageSeconds, clockText, iso, plural } from '../status/status-view-words';
import type { CatalogState, SeedFailure } from './catalog';
import type { RunsState } from './runs-reader';
import type { ScoreboardSource } from './scoreboard-view-types';

export const STALE_AFTER_SECONDS = 180;

export interface ScoreboardInputs {
  now: number;
  timeZone: string;
  root: string;
  catalog: CatalogState;
  runs: RunsState;
  /** Catalog seeds this server could not write at start. */
  seedFailures?: readonly SeedFailure[];
}

function problemList(problems: readonly ReportProblem[]): string {
  const shown = problems
    .slice(0, 5)
    .map((problem) => `${problem.file.replace(/^writers\//, '')} (${problem.reason})`);
  const more = problems.length > 5 ? `, and ${problems.length - 5} more` : '';
  return `${shown.join(', ')}${more}`;
}

function sourceBase(
  id: ScoreboardSource['id'],
  label: string,
  scannedAt: number | null,
  now: number
) {
  return {
    id,
    label,
    asOf: iso(scannedAt),
    ageSeconds: ageSeconds(now, scannedAt),
    staleAfterSeconds: STALE_AFTER_SECONDS,
  };
}

export function runsSource(
  inputs: ScoreboardInputs,
  unscored: number,
  ignoredReviews: number,
  unknownOutcomes: ReadonlyMap<string, number> = new Map(),
  negativeQuota: readonly string[] = []
) {
  const { runs, now, timeZone, root } = inputs;
  const base = sourceBase('scoreboard-runs', 'Agent runs', runs.scannedAt, now);
  if (runs.scannedAt === null)
    return { ...base, state: 'missing', detail: 'Waiting for the first scan.' } as const;
  if (runs.state === 'missing')
    return { ...base, state: 'missing', detail: `No agent reports folder at ${root}.` } as const;
  if (runs.state === 'error')
    return {
      ...base,
      state: 'error',
      detail: 'The agent reports folder could not be listed.',
    } as const;
  if ((base.ageSeconds ?? 0) > STALE_AFTER_SECONDS)
    return {
      ...base,
      state: 'stale',
      detail: `The agent reports folder has not answered since ${clockText(runs.scannedAt, timeZone)}.`,
    } as const;
  const docs = runs.runs.map((run) => run.doc);
  const benchmark = docs.filter((run) => run.source === 'benchmark').length;
  const parts = [
    `${plural(docs.length, 'run')} from ${plural(runs.writers, 'writer')} read ${clockText(runs.scannedAt, timeZone)} (${docs.length - benchmark} observed, ${benchmark} benchmark).`,
  ];
  if (unscored > 0) parts.push(`${plural(unscored, 'run')} with no model or task type not scored.`);
  if (ignoredReviews > 0)
    parts.push(
      `${plural(ignoredReviews, 'review')} ignored: not a review run, or no earlier known run.`
    );
  if (unknownOutcomes.size > 0) {
    const total = [...unknownOutcomes.values()].reduce((sum, value) => sum + value, 0);
    parts.push(
      `${plural(total, 'run')} with an outcome that is neither done nor a failure, left out of reliability, overclaim and speed and counted as no accepted units: ${counted(unknownOutcomes)}.`
    );
  }
  if (negativeQuota.length > 0) {
    const more = negativeQuota.length > 5 ? `, and ${negativeQuota.length - 5} more` : '';
    parts.push(
      `${plural(negativeQuota.length, 'run')} with a negative quota move left out of the quota rates: ${negativeQuota.slice(0, 5).join(', ')}${more}.`
    );
  }
  const fileProblems = runs.problems.filter((p) => !p.file.startsWith('limits/'));
  const limitProblems = runs.problems.filter((p) => p.file.startsWith('limits/'));
  if (fileProblems.length > 0) {
    const since = Math.min(...fileProblems.map((problem) => problem.since));
    parts.unshift(
      `${plural(fileProblems.length, 'file')} unreadable since ${clockText(since, timeZone)}: ${problemList(fileProblems)}.`
    );
    for (const problem of limitProblems) parts.push(problem.reason);
    return { ...base, state: 'unreadable', detail: parts.join(' ') } as const;
  }
  if (limitProblems.length > 0) {
    for (const problem of limitProblems) parts.push(problem.reason);
    return { ...base, state: 'ok', detail: parts.join(' ') } as const;
  }
  return { ...base, state: 'ok', detail: parts.join(' ') } as const;
}

function counted(names: ReadonlyMap<string, number>): string {
  return [...names]
    .sort(([a], [b]) => a.localeCompare(b))
    .slice(0, 8)
    .map(([name, runs]) => `${name} (${plural(runs, 'run')})`)
    .join(', ');
}

export function catalogSource(
  inputs: ScoreboardInputs,
  missingTypes: ReadonlyMap<string, number>,
  missingModels: ReadonlyMap<string, number>
) {
  const { catalog, now, timeZone } = inputs;
  const base = sourceBase('scoreboard-catalog', 'Scoreboard catalog', catalog.scannedAt, now);
  if (catalog.scannedAt === null)
    return { ...base, state: 'missing', detail: 'Waiting for the first scan.' } as const;
  if (catalog.state === 'missing')
    return {
      ...base,
      state: 'missing',
      detail: 'No catalog folder in the agent reports.',
    } as const;
  if (catalog.state === 'error')
    return { ...base, state: 'error', detail: 'The catalog folder could not be listed.' } as const;
  if ((base.ageSeconds ?? 0) > STALE_AFTER_SECONDS)
    return {
      ...base,
      state: 'stale',
      detail: `The catalog folder has not answered since ${clockText(catalog.scannedAt, timeZone)}.`,
    } as const;
  if (catalog.problems.length > 0) {
    const since = Math.min(...catalog.problems.map((problem) => problem.since));
    return {
      ...base,
      state: 'unreadable',
      detail: `${plural(catalog.problems.length, 'catalog file')} unreadable since ${clockText(since, timeZone)}: ${problemList(catalog.problems)}.`,
    } as const;
  }
  if (catalog.missingFiles.length > 0) {
    const seeds = (inputs.seedFailures ?? [])
      .filter((failure) => catalog.missingFiles.includes(failure.file))
      .map((failure) => ` Could not seed ${failure.file}: ${failure.code}.`)
      .join('');
    return {
      ...base,
      state: 'missing',
      detail: `Catalog files missing: ${catalog.missingFiles.join(', ')}.${seeds}`,
    } as const;
  }
  const parts = [`Catalog read ${clockText(catalog.scannedAt, timeZone)}.`];
  if (missingTypes.size > 0)
    parts.push(`Task types in runs but not in the catalog, not scored: ${counted(missingTypes)}.`);
  if (missingModels.size > 0)
    parts.push(`Models in runs but not in the catalog: ${counted(missingModels)}.`);
  return { ...base, state: 'ok', detail: parts.join(' ') } as const;
}
