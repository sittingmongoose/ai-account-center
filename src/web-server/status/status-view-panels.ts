/** The machines and usage panels of the Status tab view. */
import { MACHINES, type MachineId } from './report-schema';
import type { MachineSample, ReadProblem } from './status-collectors';
import { readingPredatesReset } from '../agent-usage/agent-usage-projection';
import type {
  Collected,
  MachineView,
  Source,
  SourceRef,
  StatusInputs,
  StatusView,
  UsageAccountView,
  UsageWarning,
} from './status-view-types';
import {
  ageSeconds,
  clockText,
  dayText,
  DISK_FLOOR_GB,
  gb,
  iso,
  MEMORY_FLOOR_GB,
  round1,
  spanText,
  STALE_AFTER_SECONDS,
} from './status-view-words';

// ---------------------------------------------------------------------------------------------
// Machines, Simlock, usage

const MACHINE_LABELS: Record<MachineId, string> = {
  vm: 'Ubuntu VM',
  mac: 'Mac',
  windows: 'Windows PC',
  nas1: 'Nas1',
};

/** Floor warnings for one reading. */
export function machineWarnings(sample: MachineSample | null): string[] {
  if (!sample) return [];
  const warnings: string[] = [];
  const free = gb(sample.disk?.freeBytes ?? null);
  if (free !== null && free < DISK_FLOOR_GB)
    warnings.push(`Disk ${free.toFixed(1)} GB free, under the ${DISK_FLOOR_GB} GB floor`);
  const available = gb(sample.memAvailableBytes);
  if (available !== null && available < MEMORY_FLOOR_GB)
    warnings.push(
      `Memory ${available.toFixed(1)} GB available, under the ${MEMORY_FLOOR_GB} GB floor`
    );
  return warnings;
}

/** The parts of a source a row on another source's panel carries with the readings it supplied. */
export function sourceRef(source: Source): SourceRef {
  const { id, label, state, asOf, ageSeconds, detail } = source;
  return { id, label, state, asOf, ageSeconds, detail };
}

/** One failed part in words: "Nas1: backend unavailable (since 10:01 AM)." */
export function partProblemText(problem: ReadProblem, timeZone: string): string {
  return `${problem.item}: ${problem.reason.replace(/\.$/, '')} (since ${clockText(problem.since, timeZone)}).`;
}

/**
 * The SourceRef one row carries for the readings it got from a source that names failed parts: the
 * source's own ref while the whole source is failing or stale; else this part's own state and read
 * time (its problem, or `ok` at its own read).
 */
export function partRef(
  source: Source,
  collected: Collected<unknown>,
  part: { readAt?: number | null; problem?: ReadProblem | null } | null,
  now: number,
  timeZone: string,
  absent: string
): SourceRef {
  const whole = sourceRef(source);
  if (collected.error || collected.asOf === null || source.state === 'stale') return whole;
  const { id, label } = source;
  if (!part) return { id, label, state: 'missing', asOf: null, ageSeconds: null, detail: absent };
  const readAt = part.readAt ?? null;
  if (part.problem) {
    const shown =
      readAt !== null
        ? ` Showing its reading from ${clockText(readAt, timeZone)}.`
        : ' No reading yet.';
    return {
      id,
      label,
      state: source.state === 'unreadable' ? 'unreadable' : 'error',
      asOf: iso(readAt),
      ageSeconds: ageSeconds(now, readAt),
      detail: `${partProblemText(part.problem, timeZone)}${shown}`,
    };
  }
  const at = readAt ?? collected.asOf;
  return {
    id,
    label,
    state: 'ok',
    asOf: iso(at),
    ageSeconds: ageSeconds(now, at),
    detail: `Read ${clockText(at, timeZone)}.`,
  };
}

/**
 * One machine. Its CPU, memory and disk come from the machine read; browsers, recordings, GPU and
 * sessions come from browser-check and carry that source's own state (`slotsSource`).
 */
export function machineView(inputs: StatusInputs, id: MachineId, browsers: Source): MachineView {
  const { now, timeZone } = inputs;
  const collected = inputs.machines[id];
  const sample = collected.data;
  const age = ageSeconds(now, collected.asOf);
  const staleAfter = STALE_AFTER_SECONDS.machines;
  let state: MachineView['state'] = 'ok';
  let detail: string | null = null;
  if (collected.error) {
    state = 'unreachable';
    detail = `${collected.error.reason.replace(/\.$/, '')} (since ${clockText(collected.error.since, timeZone)}).`;
    if (collected.asOf !== null)
      detail += ` Showing the reading from ${clockText(collected.asOf, timeZone)}.`;
  } else if (collected.asOf === null) {
    state = 'stale';
    detail = 'Waiting for the first reading.';
  } else if ((age ?? 0) > staleAfter) {
    state = 'stale';
    detail = `No new reading since ${clockText(collected.asOf, timeZone)}.`;
  }
  // A part (CPU, disk, memory) that failed while the rest was read: the machine is not current as a whole.
  for (const [item, label, noun, at, held] of [
    ['cpu', 'CPU', 'CPU', sample?.cpuReadAt, typeof sample?.busyPct === 'number'],
    ['disk', 'Disk', 'disk', sample?.diskReadAt, Boolean(sample?.disk)],
    [
      'memory',
      'Memory',
      'memory',
      sample?.memoryReadAt,
      typeof sample?.memAvailableBytes === 'number',
    ],
  ] as const) {
    const problem = collected.problems?.find((p) => p.item === item);
    if (!problem) continue;
    if (state === 'ok') state = 'stale';
    const kept =
      held && at !== null && at !== undefined
        ? ` Showing the ${noun} reading from ${clockText(at, timeZone)}.`
        : ` No earlier ${noun} reading.`;
    const words = `${label}: ${problem.reason.replace(/\.$/, '')} (since ${clockText(problem.since, timeZone)}).${kept}`;
    detail = detail ? `${detail} ${words}` : words;
  }
  const browser = (inputs.browsers.data ?? []).find((row) => row.machine === id) ?? null;
  return {
    id,
    label: MACHINE_LABELS[id],
    state,
    asOf: iso(collected.asOf),
    ageSeconds: age,
    detail,
    cpu: sample
      ? { cores: sample.cores, load1: round1(sample.load1), busyPct: sample.busyPct }
      : null,
    memory: sample
      ? { availableGb: gb(sample.memAvailableBytes), totalGb: gb(sample.memTotalBytes) }
      : null,
    disk: sample?.disk
      ? [
          {
            mount: sample.disk.mount,
            freeGb: gb(sample.disk.freeBytes) ?? 0,
            totalGb: gb(sample.disk.totalBytes) ?? 0,
          },
        ]
      : [],
    browsers: browser ? browser.browsers : null,
    recordings: browser ? browser.recordings : null,
    gpuPct: browser?.gpuPct ?? null,
    sessions: browser?.sessions ?? [],
    // Each host's slots carry that host's own state and read time: one failed host is not the fleet.
    slotsSource: partRef(
      browsers,
      inputs.browsers,
      browser,
      now,
      timeZone,
      'browser-check has no row for this machine.'
    ),
    warnings: machineWarnings(sample),
  };
}

export function machinesSource(inputs: StatusInputs, rows: MachineView[]): Source {
  const { now, timeZone } = inputs;
  const times = MACHINES.map((id) => inputs.machines[id].asOf).filter(
    (t): t is number => t !== null
  );
  const asOf = times.length > 0 ? Math.max(...times) : null;
  const base = {
    id: 'machines' as const,
    label: 'Machines',
    asOf: iso(asOf),
    ageSeconds: ageSeconds(now, asOf),
    staleAfterSeconds: STALE_AFTER_SECONDS.machines,
  };
  if (asOf === null && rows.every((row) => row.state !== 'unreachable'))
    return { ...base, state: 'missing', detail: 'Waiting for the first reading.' };
  const off = rows.filter((row) => row.state !== 'ok');
  if (off.length === 0)
    return {
      ...base,
      state: 'ok',
      detail: `All four machines read by ${clockText(asOf ?? now, timeZone)}.`,
    };
  return {
    ...base,
    state: 'stale',
    detail: off.map((row) => `${row.label} ${row.state}: ${row.detail ?? ''}`.trim()).join(' '),
  };
}

const USAGE_KINDS = new Set(['rate_limit', 'balance']);

export function buildUsage(inputs: StatusInputs, source: Source): StatusView['usage'] {
  const { usage, now, timeZone } = inputs;
  if (!usage) return { source: 'usage', warnings: [], accounts: [] };
  const sampleStale = source.state !== 'ok';
  const accounts: UsageAccountView[] = [];
  const warnings: Array<UsageWarning & { rank: number }> = [];
  for (const account of usage.accounts) {
    if (account.hidden) continue;
    for (const window of account.windows) {
      if (!USAGE_KINDS.has(window.kind)) continue;
      // The sample's own projection is rechecked at view time: a reset can pass between samples.
      const resetPassed =
        window.resetPassed ||
        readingPredatesReset(window.resetAt, window.sampledAt ?? account.sampledAt, now);
      // Only a current reading of a fresh sample can raise a warning.
      const stale = sampleStale || account.status !== 'ok' || window.cached || resetPassed;
      const atLimit = window.usedPercent !== null && window.usedPercent >= 100 && !resetPassed;
      // A passed reset ends the old window: its pace says nothing about the new one.
      const burn = resetPassed ? null : window.burn;
      const row: UsageAccountView = {
        id: account.id,
        provider: account.provider,
        label: account.label,
        host: account.host,
        active: account.active,
        window: window.label,
        kind: window.kind,
        usedPct: round1(window.usedPercent),
        remaining: window.remaining,
        limit: window.limit,
        unit: window.unit,
        resetsAt: window.resetAt,
        projectedFullAt: burn?.projectedFullAt ?? null,
        runsOutBeforeReset: burn?.runsOutBeforeReset ?? null,
        atLimit,
        stale,
        resetPassed,
      };
      accounts.push(row);
      if (stale) continue;
      const who = `${account.provider} ${account.label} (${window.label})`;
      const reset = window.resetAt ? Date.parse(window.resetAt) : NaN;
      if (atLimit) {
        warnings.push({
          tone: 'bad',
          rank: 0,
          text: Number.isFinite(reset)
            ? `${who}: at its limit until its ${dayText(reset, timeZone)} ${clockText(reset, timeZone)} reset`
            : `${who}: at its limit`,
        });
        continue;
      }
      const full = row.projectedFullAt ? Date.parse(row.projectedFullAt) : NaN;
      if (row.runsOutBeforeReset === true && Number.isFinite(full) && full > now) {
        warnings.push({
          tone: 'warn',
          rank: full - now,
          text: Number.isFinite(reset)
            ? `${who}: full in ${spanText((full - now) / 1000)}, before its ${dayText(reset, timeZone)} reset`
            : `${who}: full in ${spanText((full - now) / 1000)}`,
        });
      }
    }
  }
  accounts.sort(
    (a, b) =>
      a.provider.localeCompare(b.provider) ||
      a.label.localeCompare(b.label) ||
      a.window.localeCompare(b.window)
  );
  warnings.sort((a, b) => a.rank - b.rank);
  return {
    source: 'usage',
    warnings: warnings.map(({ tone, text }) => ({ tone, text })),
    accounts,
  };
}
