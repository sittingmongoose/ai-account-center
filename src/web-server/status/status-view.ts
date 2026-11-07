/**
 * The Status tab view (docs/status-tab.md, GET /api/status v1): every number, word, state and
 * warning the page shows, built from the collected inputs. Pure: the same inputs and clock give the
 * same view. A missing value is null with a reason, never 0, and nothing from a stale source is
 * presented as live.
 */
import { SEQ_WENT_BACK, type ReportProblem, type SeenReport } from './agent-reports-reader';
import { CLOSING_KINDS, deliveryView, summarySteps } from './answers';
import {
  ITEM_STATES,
  MACHINES,
  URGENCIES,
  writerMachine,
  type CardSection,
  type CardDoc,
  type CardMedia,
  type TasksDoc,
  type ThreadDoc,
} from './report-schema';
import { buildUsage, machinesSource, machineView } from './status-view-panels';
import { buildSpend, quotaSource } from './status-view-quota';
import type {
  BatchView,
  SectionView,
  CardNoteView,
  MediaView,
  CardView,
  Collected,
  HeadlineRun,
  ItemCounts,
  ProjectView,
  Source,
  SourceId,
  StatusInputs,
  StatusView,
  StreamView,
  ThreadView,
} from './status-view-types';
import {
  ageSeconds,
  clockText,
  iso,
  PM_MAIL_STALE_MS,
  plural,
  POLL_SECONDS,
  QUIET_DAY_MS,
  STALE_AFTER_SECONDS,
  STATUS_VIEW_VERSION,
  TASKS_STALE_MS,
} from './status-view-words';

export * from './status-view-types';
export * from './status-view-quota';
export { machineWarnings } from './status-view-panels';
export {
  clockText,
  dayText,
  DISK_FLOOR_GB,
  MEMORY_FLOOR_GB,
  POLL_SECONDS,
  spanText,
  STALE_AFTER_SECONDS,
  STATUS_VIEW_VERSION,
} from './status-view-words';

// ---------------------------------------------------------------------------------------------
// Sources

function collectedSource<T>(
  id: SourceId,
  label: string,
  collected: Collected<T>,
  staleAfterSeconds: number,
  now: number,
  timeZone: string
): Source {
  const base = {
    id,
    label,
    asOf: iso(collected.asOf),
    ageSeconds: ageSeconds(now, collected.asOf),
    staleAfterSeconds,
  };
  const shown =
    collected.asOf !== null
      ? ` Showing the reading from ${clockText(collected.asOf, timeZone)}.`
      : '';
  if (collected.error) {
    const since = clockText(collected.error.since, timeZone);
    return {
      ...base,
      state: collected.missing ? 'missing' : 'error',
      detail: `${collected.error.reason.replace(/\.$/, '')} (since ${since}).${shown}`,
    };
  }
  if (collected.asOf === null)
    return { ...base, state: 'missing', detail: 'Waiting for the first reading.' };
  if ((base.ageSeconds ?? 0) > staleAfterSeconds)
    return {
      ...base,
      state: 'stale',
      detail: `No new reading since ${clockText(collected.asOf, timeZone)}.`,
    };
  return { ...base, state: 'ok', detail: `Read ${clockText(collected.asOf, timeZone)}.` };
}

function problemList(problems: ReportProblem[]): string {
  const shown = problems
    .slice(0, 5)
    .map((p) => `${p.file.replace(/^writers\//, '')} (${p.reason})`);
  const more = problems.length > 5 ? `, and ${problems.length - 5} more` : '';
  return `${shown.join(', ')}${more}`;
}

function reportsSource(inputs: StatusInputs): Source {
  const { reports, now, timeZone } = inputs;
  const staleAfter = STALE_AFTER_SECONDS['agent-reports'];
  const base = {
    id: 'agent-reports' as const,
    label: 'Agent reports',
    asOf: iso(reports.scannedAt),
    ageSeconds: ageSeconds(now, reports.scannedAt),
    staleAfterSeconds: staleAfter,
  };
  if (reports.scannedAt === null)
    return { ...base, state: 'missing', detail: 'Waiting for the first scan.' };
  if (reports.state === 'missing')
    return { ...base, state: 'missing', detail: `No agent reports folder at ${reports.root}.` };
  if (reports.state === 'error')
    return { ...base, state: 'error', detail: 'The agent reports folder could not be listed.' };
  if ((base.ageSeconds ?? 0) > staleAfter)
    return {
      ...base,
      state: 'stale',
      detail: `The agent reports folder has not answered since ${clockText(reports.scannedAt, timeZone)}.`,
    };
  if (reports.problems.length > 0) {
    const since = Math.min(...reports.problems.map((p) => p.since));
    return {
      ...base,
      state: 'unreadable',
      detail: `${plural(reports.problems.length, 'file')} unreadable since ${clockText(since, timeZone)}: ${problemList(reports.problems)}.`,
    };
  }
  return {
    ...base,
    state: 'ok',
    detail: `${plural(reports.threads.length, 'thread')} and ${plural(reports.cards.length, 'card')} read ${clockText(reports.scannedAt, timeZone)}.`,
  };
}

function usageSource(inputs: StatusInputs): Source {
  const { usage, now, timeZone } = inputs;
  const base = { id: 'usage' as const, label: 'Account usage' };
  if (!usage)
    return {
      ...base,
      state: 'missing',
      asOf: null,
      ageSeconds: null,
      staleAfterSeconds: null,
      detail: 'Waiting for the dashboard’s first account sample.',
    };
  const age = ageSeconds(now, usage.sampledAt);
  const stale = (age ?? 0) > usage.staleAfterSeconds;
  return {
    ...base,
    state: stale ? 'stale' : 'ok',
    asOf: iso(usage.sampledAt),
    ageSeconds: age,
    staleAfterSeconds: usage.staleAfterSeconds,
    detail: stale
      ? `No account sample since ${clockText(usage.sampledAt, timeZone)}.`
      : `Sampled ${clockText(usage.sampledAt, timeZone)}.`,
  };
}

// ---------------------------------------------------------------------------------------------
// Threads, cards, projects

function threadName(row: SeenReport<ThreadDoc>): string {
  return row.doc.agent.name ?? row.writer.slice(row.writer.indexOf('.') + 1);
}

/** The older of the writer's own time and when AAC first saw this version. */
function seenAt(row: { writtenAtMs: number; firstSeenAt: number }): number {
  return Number.isFinite(row.writtenAtMs)
    ? Math.min(row.writtenAtMs, row.firstSeenAt)
    : row.firstSeenAt;
}

/** Past its heartbeat, or unreadable. A quiet thread without a heartbeat (`ttlSeconds: null`) shows its age only. */
function threadStale(row: SeenReport<ThreadDoc>, now: number): boolean {
  const ttl = row.doc.ttlSeconds;
  return row.problem !== null || (ttl !== null && now - seenAt(row) > ttl * 1000);
}

function reportThreadView(row: SeenReport<ThreadDoc>, now: number): ThreadView {
  const at = seenAt(row);
  const ahead = Number.isFinite(row.writtenAtMs) ? (row.writtenAtMs - row.firstSeenAt) / 1000 : 0;
  return {
    name: threadName(row),
    writer: row.writer,
    machine: writerMachine(row.writer),
    project: row.doc.project,
    stream: row.doc.stream,
    state: row.doc.state,
    doing: row.doc.doing,
    blockedOn: row.doc.blockedOn,
    progress: row.doc.progress,
    model: row.doc.agent.model,
    effort: row.doc.agent.effort,
    account: row.doc.agent.account,
    source: 'report',
    lastSeen: iso(at),
    ageSeconds: ageSeconds(now, at),
    stale: threadStale(row, now),
    clockAheadSeconds: ahead > 120 ? Math.round(ahead) : null,
    problem: row.problem?.reason ?? null,
  };
}

const THREAD_RANK: Record<string, number> = {
  blocked: 0,
  waiting: 1,
  working: 2,
  paused: 3,
  idle: 4,
  done: 5,
};

function buildThreads(inputs: StatusInputs): ThreadView[] {
  const { now } = inputs;
  const hidden = new Set(inputs.hiddenThreadNames ?? []);
  const rows: ThreadView[] = [];
  const reporting = new Set<string>();
  for (const row of inputs.reports.threads) {
    const view = reportThreadView(row, now);
    reporting.add(view.name);
    reporting.add(row.writer.slice(row.writer.indexOf('.') + 1));
    if (now - seenAt(row) > QUIET_DAY_MS && row.doc.state !== 'blocked') continue;
    rows.push(view);
  }
  for (const thread of inputs.registry.data ?? []) {
    if (reporting.has(thread.name) || hidden.has(thread.name)) continue;
    const at = thread.lastSeen ? Date.parse(thread.lastSeen) : NaN;
    if (!Number.isFinite(at) || now - at > QUIET_DAY_MS) continue;
    rows.push({
      name: thread.name,
      writer: null,
      machine: thread.machine,
      project: null,
      stream: null,
      state: null,
      doing: thread.about,
      blockedOn: null,
      progress: null,
      model: null,
      effort: null,
      account: null,
      source: 'pm-mail',
      lastSeen: thread.lastSeen,
      ageSeconds: ageSeconds(now, at),
      stale: now - at > PM_MAIL_STALE_MS,
      clockAheadSeconds: null,
      problem: null,
    });
  }
  return rows.sort(
    (a, b) =>
      Number(a.source === 'pm-mail') - Number(b.source === 'pm-mail') ||
      Number(a.stale) - Number(b.stale) ||
      (THREAD_RANK[a.state ?? ''] ?? 9) - (THREAD_RANK[b.state ?? ''] ?? 9) ||
      (a.ageSeconds ?? Infinity) - (b.ageSeconds ?? Infinity) ||
      a.name.localeCompare(b.name)
  );
}

function askerState(inputs: StatusInputs, writer: string): CardView['askerState'] {
  const thread = inputs.reports.threads.find((row) => row.writer === writer);
  if (!thread) return 'unknown';
  return threadStale(thread, inputs.now) ? 'gone' : thread.doc.state;
}

/** The URL an option image is served from: the writer's folder and the path, each segment encoded. */
export function cardFileUrl(writer: string, file: string): string {
  return `/api/status/files/${encodeURIComponent(writer)}/${file
    .split('/')
    .map(encodeURIComponent)
    .join('/')}`;
}

function mediaView(writer: string, item: CardMedia): MediaView {
  return {
    ...item,
    url: cardFileUrl(writer, item.path),
    posterUrl: item.poster ? cardFileUrl(writer, item.poster) : null,
  };
}

function sectionView(writer: string, item: CardSection): SectionView {
  if (item.type === 'media' && !('unknown' in item))
    return { ...item, items: (item.items as CardMedia[]).map((m) => mediaView(writer, m)) };
  return item as SectionView;
}

/** Shown on a judgement card that carries a recommendation (the owner is never anchored). */
export const JUDGEMENT_ANCHORED =
  'This judgement card carries a recommendation, so it cannot be answered: a judgement must not anchor the answer.';

function cardView(
  inputs: StatusInputs,
  doc: CardDoc,
  sha256: string,
  problem: ReportProblem | null,
  notes: CardNoteView[] = []
): CardView {
  const asked = Date.parse(doc.askedAt);
  // Stored history cards are read back as written; missing fields read as empty.
  const anchored = doc.kind === 'judgement' && Boolean(doc.recommendation);
  return {
    id: doc.id,
    label: doc.label ?? doc.id.slice(doc.id.indexOf('--') + 2),
    writer: doc.writer,
    seq: doc.seq,
    project: doc.project ?? null,
    stream: doc.stream ?? null,
    askedBy: doc.askedBy ?? null,
    title: doc.title,
    now: doc.now ?? null,
    nowAsOf: doc.nowAsOf ?? null,
    oldAnswer: doc.oldAnswer ?? null,
    whatChanged: doc.whatChanged ?? null,
    options: (doc.options ?? []).map((option) => ({
      key: option.key,
      label: option.label,
      gets: option.gets ?? null,
      costs: option.costs ?? null,
      media: (option.media ?? []).map((item) => mediaView(doc.writer, item)),
      sections: (option.sections ?? []).map((item) => sectionView(doc.writer, item)),
      stance: option.stance ?? null,
    })),
    recommendation: doc.recommendation ?? null,
    multiple: doc.multiple === true,
    urgency: doc.urgency,
    blocks: doc.blocks ?? null,
    sections: (doc.sections ?? []).map((item) => sectionView(doc.writer, item)),
    batch: doc.batch ?? null,
    evidence: doc.evidence ?? [],
    replies: doc.replies ?? [],
    kind: doc.kind ?? 'question',
    neededBy: doc.neededBy ?? null,
    exception: doc.exception ?? null,
    reversible: doc.reversible ?? null,
    verdict: doc.verdict ?? null,
    group: doc.group ?? null,
    agreement: doc.agreement ?? null,
    doubt: doc.doubt ?? null,
    appliesTo: doc.appliesTo ?? null,
    changesCanon: doc.changesCanon ?? [],
    related: doc.related ?? [],
    askedAt: doc.askedAt,
    ageSeconds: ageSeconds(inputs.now, asked) ?? 0,
    askerState: askerState(inputs, doc.writer),
    cardSha256: sha256,
    notes,
    problem: problem?.reason ?? (anchored ? JUDGEMENT_ANCHORED : null),
    answerable: problem === null && !anchored && doc.kind !== 'sample',
  };
}

/** Whether the shown cards are all there are: see `cards.complete` in status-view-types.ts. */
function cardsComplete(inputs: StatusInputs): boolean {
  if (inputs.answerHistoryProblem) return false;
  const { state, complete, scannedAt } = inputs.reports;
  if (state !== 'ok' || !complete || scannedAt === null) return false;
  return (ageSeconds(inputs.now, scannedAt) ?? 0) <= STALE_AFTER_SECONDS['agent-reports'];
}

function buildCards(inputs: StatusInputs): StatusView['cards'] {
  const summaries = summarySteps(inputs.sends ?? []);
  // Answers and dones close a card version; notes and objections stay on the open card.
  const answers = inputs.answers.filter((e) => CLOSING_KINDS.has(e.answer.kind));
  const answered = new Set(
    answers.map((e) => `${e.answer.cardId}\n${e.answer.cardSeq}\n${e.answer.cardSha256}`)
  );
  const notes = new Map<string, CardNoteView[]>();
  for (const entry of inputs.answers) {
    if (CLOSING_KINDS.has(entry.answer.kind)) continue;
    const list = notes.get(entry.answer.cardId) ?? [];
    list.push({
      kind: entry.answer.kind === 'object' ? 'object' : 'note',
      note: entry.answer.note,
      at: entry.answer.answeredAt,
      delivery: deliveryView(entry, summaries),
    });
    notes.set(entry.answer.cardId, list);
  }
  // After a restart the scan has no memory of a higher seq, but the records do: a card older than
  // one the owner already answered cannot be answered, and saying so spares the owner a loop.
  const highest = new Map<string, number>();
  for (const { answer } of inputs.answers)
    highest.set(answer.cardId, Math.max(highest.get(answer.cardId) ?? 0, answer.cardSeq));
  const open = inputs.reports.cards
    .filter(
      (row) =>
        row.doc.state === 'open' && !answered.has(`${row.doc.id}\n${row.doc.seq}\n${row.sha256}`)
    )
    .map((row) => {
      const older = row.doc.seq < (highest.get(row.doc.id) ?? 0);
      const problem =
        row.problem ??
        (older ? { file: row.file, reason: SEQ_WENT_BACK, since: inputs.now } : null);
      const card = cardView(inputs, row.doc, row.sha256, problem, notes.get(row.doc.id));
      if (inputs.answerHistoryProblem) {
        card.answerable = false;
        card.problem = inputs.answerHistoryProblem;
      }
      return card;
    })
    .sort(
      (a, b) =>
        URGENCIES.indexOf(a.urgency) - URGENCIES.indexOf(b.urgency) ||
        b.ageSeconds - a.ageSeconds ||
        a.id.localeCompare(b.id)
    );
  const batches = new Map<string, BatchView>();
  for (const card of open) {
    if (!card.batch) continue;
    const batch = batches.get(card.batch) ?? { id: card.batch, label: card.batch, cardIds: [] };
    batch.cardIds.push(card.id);
    batches.set(card.batch, batch);
  }
  const history = answers.slice().reverse().slice(0, 50);
  return {
    source: 'agent-reports',
    open,
    answerHistoryProblem: inputs.answerHistoryProblem ?? null,
    answered: inputs.answerHistoryProblem
      ? null
      : history.map((entry) => ({
          card: cardView(inputs, entry.card, entry.answer.cardSha256, null),
          answer: {
            kind: entry.answer.kind as 'answer' | 'done',
            choice: entry.answer.choice,
            note: entry.answer.note,
            via: entry.answer.via,
            answeredAt: entry.answer.answeredAt,
            device: entry.answer.device,
            delivery: deliveryView(entry, summaries),
          },
        })),
    batches: [...batches.values()],
    complete: cardsComplete(inputs),
  };
}

function emptyCounts(): ItemCounts {
  const counts = { total: 0 } as ItemCounts;
  for (const state of ITEM_STATES) counts[state] = 0;
  return counts;
}

function addCounts(into: ItemCounts, from: ItemCounts): void {
  into.total += from.total;
  for (const state of ITEM_STATES) into[state] += from[state];
}

/** (landed + done) of every item that was not dropped; null when there are none. */
function completion(counts: ItemCounts): { done: number; of: number } | null {
  const of = counts.total - counts.dropped;
  return of > 0 ? { done: counts.landed + counts.done, of } : null;
}

function board(inputs: StatusInputs, given: string | null): StreamView['board'] {
  if (!given) return null;
  const mtime = inputs.reports.boards.get(given) ?? null;
  return { path: given, ageSeconds: ageSeconds(inputs.now, mtime) };
}

function tasksStream(inputs: StatusInputs, row: SeenReport<TasksDoc>): StreamView {
  const { doc, problem } = row;
  const counts = emptyCounts();
  for (const item of doc.items) {
    counts.total += 1;
    counts[item.state] += 1;
  }
  const at = seenAt(row);
  return {
    id: doc.stream,
    label: doc.streamLabel ?? doc.stream,
    owner: doc.owner,
    goal: doc.goal,
    writer: row.writer,
    board: board(inputs, doc.board),
    source: 'tasks',
    asOf: iso(at),
    ageSeconds: ageSeconds(inputs.now, at),
    stale: problem !== null || inputs.now - at > TASKS_STALE_MS,
    problem: problem?.reason ?? null,
    counts,
    completion: completion(counts),
    items: doc.items.map((item) => ({
      ...item,
      ageSeconds: item.updatedAt ? ageSeconds(inputs.now, Date.parse(item.updatedAt)) : null,
    })),
  };
}

function buildProjects(inputs: StatusInputs): ProjectView[] {
  const projects = new Map<string, { label: string | null; streams: Map<string, StreamView> }>();
  const project = (id: string) => {
    const found = projects.get(id) ?? { label: null, streams: new Map<string, StreamView>() };
    projects.set(id, found);
    return found;
  };
  const tasks = inputs.reports.tasks
    .slice()
    .sort((a, b) => seenAt(b) - seenAt(a) || a.writer.localeCompare(b.writer));
  for (const row of tasks) {
    const entry = project(row.doc.project);
    entry.label ??= row.doc.projectLabel;
    // Two leads publishing one stream: the most recent board wins.
    if (!entry.streams.has(row.doc.stream))
      entry.streams.set(row.doc.stream, tasksStream(inputs, row));
  }
  const threads = inputs.reports.threads.slice().sort((a, b) => seenAt(b) - seenAt(a));
  for (const row of threads) {
    const { project: id, stream } = row.doc;
    if (!id || !stream) continue;
    const link = row.doc.links.find((l) => /board/i.test(l.label));
    if (!link) continue;
    const entry = project(id);
    if (entry.streams.has(stream)) continue;
    const at = seenAt(row);
    entry.streams.set(stream, {
      id: stream,
      label: stream,
      owner: threadName(row),
      goal: null,
      writer: row.writer,
      board: board(inputs, link.path),
      source: 'board-only',
      asOf: iso(at),
      ageSeconds: ageSeconds(inputs.now, at),
      stale: threadStale(row, inputs.now),
      problem: row.problem?.reason ?? null,
      counts: null,
      completion: null,
      items: [],
    });
  }
  return [...projects.entries()]
    .map(([id, entry]) => {
      const streams = [...entry.streams.values()].sort((a, b) => a.label.localeCompare(b.label));
      const counts = emptyCounts();
      for (const stream of streams) if (stream.counts) addCounts(counts, stream.counts);
      return { id, label: entry.label ?? id, streams, counts, completion: completion(counts) };
    })
    .sort((a, b) => a.label.localeCompare(b.label));
}

// ---------------------------------------------------------------------------------------------
// The view

export function buildStatusView(inputs: StatusInputs): StatusView {
  const { now, timeZone } = inputs;
  const machineRows = MACHINES.map((id) => machineView(inputs, id));
  const quotaSrc = quotaSource(inputs.quota, now, timeZone);
  const usageSrc = usageSource(inputs);
  const sources: Source[] = [
    reportsSource(inputs),
    collectedSource(
      'pm-mail',
      'pm-mail registry',
      inputs.registry,
      STALE_AFTER_SECONDS['pm-mail'],
      now,
      timeZone
    ),
    machinesSource(inputs, machineRows),
    collectedSource(
      'browsers',
      'Browsers and recordings',
      inputs.browsers,
      STALE_AFTER_SECONDS.browsers,
      now,
      timeZone
    ),
    collectedSource(
      'simlock',
      'Simlock',
      inputs.simlock,
      STALE_AFTER_SECONDS.simlock,
      now,
      timeZone
    ),
    usageSrc,
    quotaSrc,
  ];
  const cards = buildCards(inputs);
  const projects = buildProjects(inputs);
  const threads = buildThreads(inputs);
  const usage = buildUsage(inputs, usageSrc);
  const spend = buildSpend(inputs.quota, streamLabels(inputs));
  // Only a current report can raise the budget warning.
  const overLimit = quotaSrc.state === 'ok' ? spend.overLimitBudgets : 0;

  const total = emptyCounts();
  for (const project of projects) addCounts(total, project.counts);
  const done = completion(total);
  const openCards = cards.open.length;
  const blockedItems = total.blocked;
  const workingThreads = threads.filter((t) => t.state === 'working' && !t.stale).length;
  const staleSources = sources.filter((s) => s.state !== 'ok' && s.state !== 'unconfigured').length;

  const runs: HeadlineRun[] = [];
  if (openCards > 0)
    runs.push(
      { text: plural(openCards, 'question'), strong: true, tone: 'warn' },
      { text: ' for you' }
    );
  else runs.push({ text: 'No questions for you' });
  if (done) runs.push({ text: ` · ${done.done} of ${done.of} items landed` });
  if (blockedItems > 0)
    runs.push({ text: ' · ' }, { text: `${blockedItems} blocked`, tone: 'warn' });
  if (overLimit > 0)
    runs.push(
      { text: ' · ' },
      {
        text: `${plural(overLimit, 'budget')} at or over ${overLimit === 1 ? 'its' : 'their'} limit (estimate)`,
        tone: 'bad',
      }
    );
  if (staleSources > 0)
    runs.push(
      { text: ' · ' },
      { text: `${plural(staleSources, 'source')} not current`, tone: 'warn' }
    );

  const bad =
    cards.open.some((card) => card.urgency === 'now') ||
    machineRows.some((row) => row.warnings.length > 0 || row.state === 'unreachable') ||
    usage.warnings.some((w) => w.tone === 'bad') ||
    overLimit > 0;
  const warn = openCards > 0 || blockedItems > 0 || staleSources > 0 || usage.warnings.length > 0;

  return {
    version: STATUS_VIEW_VERSION,
    generatedAt: new Date(now).toISOString(),
    refresh: { pollSeconds: POLL_SECONDS },
    headline: {
      tone: bad ? 'bad' : warn ? 'warn' : 'good',
      runs,
      counts: { openCards, blockedItems, workingThreads, staleSources },
    },
    sources,
    cards,
    projects,
    threads: { source: 'agent-reports', rows: threads },
    machines: { source: 'machines', rows: machineRows },
    simlock: {
      source: 'simlock',
      capacity: inputs.simlock.data?.capacity ?? [],
      leases: inputs.simlock.data?.leases ?? [],
    },
    usage,
    spend,
  };
}

/** Stream ids the agent reports name, with their labels; an id two projects label differently keeps its id. */
function streamLabels(inputs: StatusInputs): Map<string, string> {
  const labels = new Map<string, string>();
  const clash = new Set<string>();
  for (const row of inputs.reports.tasks) {
    const label = row.doc.streamLabel ?? row.doc.stream;
    const seen = labels.get(row.doc.stream);
    if (seen !== undefined && seen !== label) clash.add(row.doc.stream);
    labels.set(row.doc.stream, label);
  }
  for (const id of clash) labels.delete(id);
  return labels;
}
