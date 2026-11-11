import { reportV2 } from './report-v2';
/**
 * The pinned-threads panel's collector: every machine's T3 pins (the VM's database read by
 * the aac-status native module, the Mac's, Windows' and Nas1's fetched with one short
 * read-only sqlite3 command over the dashboard's ssh route), merged in the module, and the
 * composed updates for each pinned thread, every word decided there (PINNED-THREADS.md).
 *
 * The share-reading rule of the Status code applies per machine: one that does not answer
 * keeps its last good pins for one scan, and after a second miss reports itself unavailable
 * by name while the other machines' pins still show. State lives here, in TypeScript; the
 * module stays stateless.
 */
import {
  composePinned,
  mergePins,
  openStatusHandle,
  parseRemotePins,
  type MergedPin,
  type PinnedThread,
} from '../../status-native/pinned';
import type { NativeStatusHandle } from '../../status-native/native-loader';
import type { AnswerHistoryEntry } from './answers';
import type { AgentReportsState } from './agent-reports-reader';
import { CLOSING_KINDS } from './answer-entries';
import type { BlockedOn, MachineId } from './report-schema';
import type { PinnedPanel, PinnedView } from './status-view-types';
import { readRemotePins, runCommand, type CommandRunner } from './status-collectors';
import type { ReadersCheckWriter } from './readers-check';
import { StatusDocsReader, type StatusDocsState } from './status-docs';
import { packagedCatalogDir } from '../scoreboard/catalog';
import fs from 'fs';
import path from 'path';

export type PinsMachine = 'vm' | 'mac' | 'windows' | 'nas1';

/** The machines whose pins are read, in the fixed order the panel names them. */
export const PINS_MACHINES: readonly PinsMachine[] = ['vm', 'mac', 'windows', 'nas1'];

/** Why one machine's pins are absent: a fixed code the composer puts in words. */
export type PinsProblemCode = 'did_not_answer' | 'shape' | 'missing' | 'io' | 'limit';

interface MachineState {
  lastGood: PinnedThread[] | null;
  failures: number;
  problem: { code: PinsProblemCode; since: number } | null;
}

export interface PinnedThreadsDeps {
  env?: NodeJS.ProcessEnv;
  file?: string;
  requireFn?: (file: string) => unknown;
  /** A handle to call instead of loading the module (tests). */
  handle?: NativeStatusHandle;
  run?: CommandRunner;
  /** The VM's T3 state database, instead of the default (tests point at a fixture). */
  dbPath?: string | null;
  /** One machine's `sqlite3 -json` answer (tests fake the ssh route). */
  readRemote?: (machine: Exclude<MachineId, 'vm'>, run: CommandRunner) => Promise<string>;
  now?: () => number;
  timeZone?: () => string;
  /** The reports share root, for the status@1 documents. */
  reportsRoot?: () => string;
  /** Written the readers check file after each compose (null in tests that fake the module). */
  readersCheck?: ReadersCheckWriter | null;
}

export class PinnedThreadsCollector {
  private readonly handle: NativeStatusHandle | null;
  private readonly moduleReason: string;
  private readonly machines = new Map<PinsMachine, MachineState>(
    PINS_MACHINES.map((machine) => [machine, { lastGood: null, failures: 0, problem: null }])
  );
  private merged: MergedPin[] = [];
  private problems: Array<{ machine: PinsMachine; code: PinsProblemCode }> = [];
  private composed: PinnedView | null = null;
  /** False until the first collect lands, so no update is composed over an empty pin list. */
  private collected = false;
  readonly docs: StatusDocsReader;

  constructor(private readonly deps: PinnedThreadsDeps = {}) {
    if (deps.handle) {
      this.handle = deps.handle;
      this.moduleReason = '';
    } else {
      const opened = openStatusHandle(deps);
      if ('unavailable' in opened) {
        this.handle = null;
        this.moduleReason = opened.unavailable;
      } else {
        this.handle = opened;
        this.moduleReason = '';
      }
    }
    this.docs = new StatusDocsReader({
      root: deps.reportsRoot,
      now: deps.now,
      handle: () => this.handle,
    });
  }

  private now(): number {
    return this.deps.now?.() ?? Date.now();
  }

  /** The panel now: the last composed view, or why there is none. */
  view(): PinnedPanel {
    if (!this.handle) return { unavailable: this.moduleReason };
    if (!this.collected) return { unavailable: 'the pinned threads have not been read yet' };
    if (this.composed) return this.composed;
    return { unavailable: 'the pinned updates could not be composed' };
  }

  /** Every machine's pins, re-read: the VM from its database, the others over ssh. */
  async collect(): Promise<void> {
    if (!this.handle) return;
    this.collected = true;
    const now = this.now();
    await Promise.all(
      PINS_MACHINES.map(async (machine) => {
        const outcome = await this.readMachine(machine);
        const state = this.machines.get(machine);
        if (!state) return;
        if (typeof outcome !== 'string') {
          state.lastGood = outcome;
          state.failures = 0;
          state.problem = null;
          return;
        }
        // One failed scan keeps the last good pins (the share's grace); a second names it.
        if (state.lastGood !== null && state.failures === 0) {
          state.failures = 1;
          return;
        }
        state.failures += 1;
        state.lastGood = null;
        state.problem =
          state.problem?.code === outcome ? state.problem : { code: outcome, since: now };
      })
    );
    const lists = PINS_MACHINES.flatMap((machine) => {
      const state = this.machines.get(machine);
      return state?.lastGood ? [{ machine, pinned: state.lastGood }] : [];
    });
    const merged = await mergePins(lists, { handle: this.handle });
    if ('pinned' in merged) this.merged = merged.pinned;
    this.problems = PINS_MACHINES.flatMap((machine) => {
      const problem = this.machines.get(machine)?.problem;
      return problem ? [{ machine, code: problem.code }] : [];
    });
  }

  /** One machine's good rows, or its failure code. */
  private async readMachine(machine: PinsMachine): Promise<PinnedThread[] | PinsProblemCode> {
    if (!this.handle) return 'io';
    if (machine === 'vm') return this.readLocalPins();
    const read = this.deps.readRemote ?? readRemotePins;
    let json: string;
    try {
      json = await read(machine, this.deps.run ?? runCommand);
    } catch {
      return 'did_not_answer';
    }
    const parsed = await parseRemotePins(machine, json, { handle: this.handle });
    return 'pinned' in parsed ? parsed.pinned : 'shape';
  }

  /** The VM's own database, with the reader's unavailability code kept as-is. */
  private async readLocalPins(): Promise<PinnedThread[] | PinsProblemCode> {
    if (!this.handle) return 'io';
    let raw: string;
    try {
      raw = await this.handle.call('pins', JSON.stringify({ dbPath: this.deps.dbPath ?? null }));
    } catch {
      return 'io';
    }
    let answer: unknown;
    try {
      answer = JSON.parse(raw) as unknown;
    } catch {
      return 'io';
    }
    const record = answer as Record<string, unknown>;
    if (
      record.unavailable === 'missing' ||
      record.unavailable === 'shape' ||
      record.unavailable === 'limit'
    ) {
      return record.unavailable;
    }
    if (!Array.isArray(record.pinned)) return 'io';
    return record.pinned as PinnedThread[];
  }

  /** Rescan the status@1 documents (the reports share), then recompose. */
  async scanStatusDocs(): Promise<void> {
    if (!this.handle) return;
    await this.docs.scan();
  }

  /** Recompose the updates from the reports the Status reader already validated. */
  async compose(
    reports: AgentReportsState,
    answers: readonly AnswerHistoryEntry[],
    docs?: StatusDocsState
  ): Promise<void> {
    const statuses = docs ?? this.docs.snapshot();
    // Check receipts describe reader acceptance even when pins or native composition are unavailable.
    this.deps.readersCheck?.recordStatusScan(reports, statuses, this.merged);
    this.deps.readersCheck?.flushSoon();
    if (!this.handle) return;
    // Before the first collect there are no pins to compose over; the reports scan that runs
    // first must not publish an empty panel.
    if (!this.collected) return;
    const input = {
      nowMs: this.now(),
      utcOffsetMinutes: zoneOffsetMinutes(this.deps.timeZone?.() ?? 'UTC', this.now()),
      pins: this.merged,
      machineProblems: this.problems,
      threads: reports.threads.map((row) => ({
        ...reportV2(row.doc as unknown as Record<string, unknown>),
        writer: row.writer,
        writtenAt: row.doc.writtenAt,
        writtenAtMs: row.writtenAtMs,
        seq: row.doc.seq,
        t3Thread: row.doc.agent.t3Thread,
        name: row.doc.agent.name,
        harness: row.doc.agent.harness,
        model: row.doc.agent.model,
        modelLabel: modelLabelFor(row.doc.agent.model),
        effort: row.doc.agent.effort,
        account: row.doc.agent.account,
        project: row.doc.project,
        stream: row.doc.stream,
        state: row.doc.state,
        doing: row.doc.doing,
        blockedOn: blockedOnIn(row.doc.blockedOn),
        progress: row.doc.progress,
        ttlSeconds: row.doc.ttlSeconds,
        problem: row.problem?.reason ?? null,
        problemSinceMs: row.problem?.since ?? null,
      })),
      tasks: reports.tasks.map((row) => ({
        ...reportV2(row.doc as unknown as Record<string, unknown>),
        writer: row.writer,
        writtenAtMs: row.writtenAtMs,
        seq: row.doc.seq,
        project: row.doc.project,
        projectLabel: row.doc.projectLabel,
        stream: row.doc.stream,
        streamLabel: row.doc.streamLabel,
        problem: row.problem?.reason ?? null,
        items: row.doc.items.map((item) => ({
          id: item.id,
          title: item.title,
          note: item.note,
          state: item.state,
          reviews: item.reviews.map((review) => ({
            atMs: review.at ? Date.parse(review.at) : null,
            verdict: review.verdict,
          })),
          landed: item.landed ? { atMs: item.landed.at ? Date.parse(item.landed.at) : null } : null,
          blockedOn: blockedOnIn(item.blockedOn),
          updatedAtMs: item.updatedAt ? Date.parse(item.updatedAt) : null,
        })),
      })),
      openCards: openCardRows(reports, answers).map((row) => ({
        writer: row.writer,
        id: row.doc.id,
        seq: row.doc.seq,
        title: row.doc.title,
        kind: row.doc.kind,
        askedAtMs: Date.parse(row.doc.askedAt),
      })),
      statuses: statuses.docs.map((row) => ({
        ...reportV2(row.doc),
        writer: row.writer,
        writtenAt: row.doc.writtenAt,
        writtenAtMs: row.writtenAtMs,
        seq: row.seq,
        t3Thread: row.doc.t3Thread,
        summary: row.doc.summary,
        percent: row.doc.percent,
        done: row.doc.done,
        left: row.doc.left,
        blockers: row.doc.blockers,
        looping: row.doc.looping,
        problem: row.problem?.reason ?? null,
        problemSinceMs: row.problem?.since ?? null,
      })),
      statusProblems: statuses.problems.map((problem) => ({
        file: problem.file,
        reason: problem.reason,
        sinceMs: problem.since,
      })),
    };
    const composed = await composePinned(input, { handle: this.handle });
    if ('view' in composed) this.composed = composed.view as unknown as PinnedView;
    // A refused compose keeps the last good view; its own asOfMs shows its age.
  }

  /** The merged pins of the last collect, for the pin match the check file reports. */
  pins(): MergedPin[] {
    return this.merged;
  }
}

/** The catalog's models, read once from the packaged seed (the file the Scoreboard seeds from). */
let packagedModels: Array<{ model: string; label: string }> | null = null;

function packagedModelRows(): Array<{ model: string; label: string }> {
  if (packagedModels === null) {
    try {
      const raw = JSON.parse(
        fs.readFileSync(path.join(packagedCatalogDir(), 'models.json'), 'utf8')
      ) as { models?: unknown };
      packagedModels = Array.isArray(raw.models)
        ? raw.models.flatMap((row) => {
            const entry = row as { model?: unknown; label?: unknown };
            return typeof entry.model === 'string' && typeof entry.label === 'string'
              ? [{ model: entry.model, label: entry.label }]
              : [];
          })
        : [];
    } catch {
      packagedModels = [];
    }
  }
  return packagedModels;
}

/**
 * The who line's catalog model label for a thread's raw model id (D9): an exact catalog
 * model (`anthropic/claude-opus-5-5`) or the reporter's short form (`claude-opus-5-5`) both
 * find their label. A model the catalog does not know labels as null, so the raw id shows
 * as written; no label is ever invented.
 */
export function modelLabelFor(model: string | null): string | null {
  if (!model) return null;
  const rows = packagedModelRows();
  const exact = rows.find((row) => row.model === model);
  if (exact) return exact.label;
  const short = model.slice(model.lastIndexOf('/') + 1);
  const byShort = rows.find((row) => row.model.slice(row.model.lastIndexOf('/') + 1) === short);
  return byShort?.label ?? null;
}

/** `blockedOn` with its `since` parsed once, for the composer's "waiting since" clock words. */
function blockedOnIn(blocked: BlockedOn | null) {
  return blocked ? { ...blocked, sinceMs: blocked.since ? Date.parse(blocked.since) : null } : null;
}

/** The open cards (questions the owner has not answered), the same rule as the cards panel. */
function openCardRows(
  reports: AgentReportsState,
  answers: readonly AnswerHistoryEntry[]
): AgentReportsState['cards'] {
  const answered = new Set(
    answers
      .filter((entry) => CLOSING_KINDS.has(entry.answer.kind))
      .map((entry) => `${entry.answer.cardId}\n${entry.answer.cardSeq}\n${entry.answer.cardSha256}`)
  );
  return reports.cards.filter(
    (row) =>
      row.doc.state === 'open' && !answered.has(`${row.doc.id}\n${row.doc.seq}\n${row.sha256}`)
  );
}

/** The dashboard time zone's UTC offset in minutes at `at`, for the composer's clock times. */
export function zoneOffsetMinutes(timeZone: string, at: number): number {
  try {
    const parts = new Intl.DateTimeFormat('en-US', {
      timeZone,
      timeZoneName: 'longOffset',
    }).formatToParts(new Date(at));
    const name = parts.find((part) => part.type === 'timeZoneName')?.value ?? 'GMT';
    const match = /^GMT([+-])(\d{1,2})(?::(\d{2}))?$/.exec(name);
    if (!match) return 0;
    const sign = match[1] === '-' ? -1 : 1;
    return sign * (Number(match[2]) * 60 + Number(match[3] ?? 0));
  } catch {
    return -new Date(at).getTimezoneOffset();
  }
}
