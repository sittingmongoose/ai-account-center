/** Locale/date facts for Rust composition. No source scanning, scoring or view construction. */
import { MACHINE_IDS, MACHINE_LABELS } from './services/dashboard-hosts';
import type { StatusInputs } from './status/status-view';
import { normalizeModel } from './scoreboard/model-identity';
import type { ScoreboardInputs } from './scoreboard/scoreboard-view';

export function rustScoreboardInput(input: ScoreboardInputs, finish?: unknown): unknown {
  const docs = input.runs.runs.map(({ doc }) => {
    if (!doc.model.id) return doc;
    const identity = normalizeModel(doc.model.id, doc.model.effort, input.catalog.models, {
      aliases: input.catalog.aliases,
      effortSuffixes: input.catalog.effortSuffixes,
    });
    return {
      ...doc,
      model: { ...doc.model, id: identity.id, effort: identity.effort, provider: null },
    };
  });
  const keys = docs.flatMap((doc) => {
    const model = doc.model.id;
    if (!model) return [];
    const parts = [model, doc.model.harness ?? '', doc.model.effort ?? ''];
    return [
      parts.join('|'),
      [...parts, doc.process?.id ?? '', doc.process?.version ?? ''].join('|'),
    ];
  });
  return rustReadInput({ ...input, docs, ...(finish ? { finish } : {}) }, input.timeZone, keys);
}

/**
 * The M3 presentation envelope: `Date.parse` epochs of ISO-shaped strings, ISO/clock/day words of
 * every time value and `localeCompare` ranks, all computed by this engine. `rankAll: false` ranks
 * only `extras`; `clockAndDay: false` supplies ISO words only (a composer that reads a clock or
 * day word then refuses). Defaults are the Status/Scoreboard envelope.
 */
export function rustReadInput(
  input: unknown,
  timeZone: string,
  extras: string[] = [],
  options: { rankAll?: boolean; clockAndDay?: boolean } = {}
): unknown {
  const rankAll = options.rankAll ?? true;
  const clockAndDay = options.clockAndDay ?? true;
  const data: unknown = JSON.parse(
    JSON.stringify(input, (_key, value: unknown) =>
      value instanceof Map ? Object.fromEntries(value) : value
    )
  );
  const words = new Set(extras);
  const times = new Set<number>();
  const epochs: Record<string, number> = {};
  const visit = (value: unknown): void => {
    if (typeof value === 'string') {
      if (rankAll) {
        words.add(value);
        if (value.includes('.')) words.add(value.slice(value.indexOf('.') + 1));
      }
      if (/^\d{4}-\d{2}-\d{2}T/.test(value)) {
        const ms = Date.parse(value);
        if (Number.isFinite(ms)) {
          epochs[value] = ms;
          times.add(ms);
        }
      }
    } else if (
      typeof value === 'number' &&
      Number.isSafeInteger(value) &&
      value >= 100_000_000_000 &&
      value <= 8_640_000_000_000_000
    ) {
      times.add(value);
    } else if (Array.isArray(value)) value.forEach(visit);
    else if (value && typeof value === 'object') Object.values(value).forEach(visit);
  };
  visit(data);
  const formatter = (options: Intl.DateTimeFormatOptions) => {
    try {
      return new Intl.DateTimeFormat('en-US', { timeZone, ...options });
    } catch {
      return new Intl.DateTimeFormat('en-US', { timeZone: 'UTC', ...options });
    }
  };
  const clock = formatter({ hour: 'numeric', minute: '2-digit' });
  const day = formatter({ month: 'short', day: 'numeric' });
  const dates = Object.fromEntries(
    [...times].map((ms) => [
      String(ms),
      clockAndDay
        ? {
            iso: new Date(ms).toISOString(),
            clock: clock.format(ms).replace(/[\u202f\u00a0]/g, ' '),
            day: day.format(ms).replace(/[\u202f\u00a0]/g, ' '),
          }
        : { iso: new Date(ms).toISOString() },
    ])
  );
  const ordered = [...words].sort((a, b) => a.localeCompare(b));
  let rank = 0;
  const order = Object.fromEntries(
    ordered.map((word, index) => {
      if (index > 0 && ordered[index - 1].localeCompare(word) !== 0) rank++;
      return [word, rank];
    })
  );
  return { data, presentation: { dates, epochs, order } };
}

export function rustStatusInput(input: StatusInputs): unknown {
  return rustReadInput(
    { ...input, machineIds: MACHINE_IDS, machineLabels: MACHINE_LABELS },
    input.timeZone
  );
}

function object(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}
export function isRustStatusView(value: unknown): boolean {
  if (
    !object(value) ||
    value.version !== 1 ||
    !object(value.headline) ||
    !object(value.headline.counts) ||
    !object(value.headline.strip) ||
    !Array.isArray(value.headline.runs) ||
    !Array.isArray(value.sources) ||
    !object(value.cards) ||
    !Array.isArray(value.cards.open) ||
    !Array.isArray(value.projects)
  )
    return false;
  return ['threads', 'machines', 'simlock', 'usage', 'spend'].every((key) => object(value[key]));
}
export function isRustScoreboardView(value: unknown): boolean {
  return (
    object(value) &&
    value.version === 1 &&
    typeof value.generatedAt === 'string' &&
    object(value.refresh) &&
    Array.isArray(value.sources) &&
    object(value.catalog) &&
    Array.isArray(value.uses) &&
    Array.isArray(value.recent) &&
    object(value.unscored)
  );
}
