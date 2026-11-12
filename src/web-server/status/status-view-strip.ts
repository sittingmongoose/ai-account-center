/**
 * The Status page's summary strip: the headline's facts as counts the eye can scan, instead
 * of one long sentence (owner, 2026-11-12: "it just looks like an out of place paragraph").
 *
 * Pure, like the rest of the view: the same inputs give the same strip. Every number and word
 * is decided here and the page only lays them out. A problem count of zero is left out, the
 * questions count is always shown (quiet at zero), and a count that cannot be known is "–"
 * with the reason, never 0.
 */
import type {
  HeadlineStrip,
  PinnedPanel,
  PinnedUpdateView,
  ProjectView,
  Source,
  StripItem,
  StripSection,
  StripWorst,
} from './status-view-types';

/** The pinned panel's problem threads, shared by the sentence and the strip so they never disagree. */
export interface PinnedProblems {
  /** Null when there is no panel to read: the counts are unknown, not zero. */
  view: { updates: PinnedUpdateView[] } | null;
  /** Why there is no panel, in the composer's words, or null when there is one. */
  unavailable: string | null;
  looping: PinnedUpdateView[];
  waiting: PinnedUpdateView[];
  /** The looping thread with the most failing rounds (the first one on a tie). */
  worstLoop: PinnedUpdateView | null;
}

export function pinnedProblems(pinned: PinnedPanel | undefined): PinnedProblems {
  if (!pinned || 'unavailable' in pinned) {
    return {
      view: null,
      unavailable: pinned ? pinned.unavailable : "this server doesn't send them yet",
      looping: [],
      waiting: [],
      worstLoop: null,
    };
  }
  const looping = pinned.updates.filter((update) => update.looping?.answer === 'yes');
  const waiting = pinned.updates.filter((update) => update.blocked?.waitingOnYou);
  const rounds = (update: PinnedUpdateView) => update.looping?.rounds ?? 0;
  const worstLoop = looping.reduce<PinnedUpdateView | null>(
    (worst, update) => (worst === null || rounds(update) > rounds(worst) ? update : worst),
    null
  );
  return { view: pinned, unavailable: null, looping, waiting, worstLoop };
}

export interface StripInputs {
  /** Open question, judgement and action cards (D10). */
  questions: number;
  /** Open objection cards. */
  decisions: number;
  /** The agent-reports source: with no reading at all, "no questions" would be a guess. */
  reports: Source;
  pins: PinnedProblems;
  projects: ProjectView[];
  /** Landed and done of every item but the dropped ones; null with no task list. */
  done: { done: number; of: number } | null;
  blocked: number;
  /** Budgets at or over their limit; 0 unless the quota report is current. */
  overLimitBudgets: number;
  sources: Source[];
}

/** How many names a detail sentence lists before "and N more". */
const LISTED = 4;

function count(n: number, one: string, many: string): string {
  return n === 1 ? one : many;
}

function sentence(text: string): string {
  const t = text.trim();
  return /[.!?]$/.test(t) ? t : `${t}.`;
}

function listed(names: string[]): string {
  const shown = names.slice(0, LISTED);
  const more = names.length - shown.length;
  return more > 0 ? `${shown.join(', ')} and ${more} more` : shown.join(', ');
}

function pinName(update: PinnedUpdateView): string {
  const task = update.looping?.answer === 'yes' ? update.looping.task : null;
  return task ? `${update.title} (task ${task})` : update.title;
}

function blockedItems(projects: ProjectView[]): string[] {
  const names: string[] = [];
  for (const project of projects)
    for (const stream of project.streams)
      for (const item of stream.items)
        if (item.state === 'blocked') names.push(`${item.id} ${item.title}`.trim());
  return names;
}

function questionsItem(i: StripInputs): StripItem {
  const base = { key: 'questions' as const, section: 'questions' as StripSection };
  const unread = i.reports.state === 'missing' || i.reports.state === 'error';
  if (i.questions === 0 && unread)
    return {
      ...base,
      value: '–',
      label: 'questions for you',
      short: 'questions',
      tone: 'quiet',
      detail: `Questions for you are unknown: ${sentence(i.reports.detail)}`,
    };
  const n = i.questions;
  return {
    ...base,
    value: String(n),
    label: count(n, 'question for you', 'questions for you'),
    short: count(n, 'question', 'questions'),
    tone: n > 0 ? 'warn' : 'quiet',
    detail: n > 0 ? `${n} ${count(n, 'question', 'questions')} for you.` : 'No questions for you.',
  };
}

function pinnedItems(pins: PinnedProblems): StripItem[] {
  if (pins.view === null)
    return [
      {
        key: 'pinned',
        value: '–',
        label: 'pinned threads',
        short: 'pinned',
        tone: 'quiet',
        section: 'pinned',
        detail: `Pinned threads are unavailable: ${sentence(pins.unavailable ?? 'not read')}`,
      },
    ];
  const out: StripItem[] = [];
  const looping = pins.looping.length;
  if (looping > 0)
    out.push({
      key: 'looping',
      value: String(looping),
      label: count(looping, 'thread looping', 'threads looping'),
      short: 'looping',
      tone: 'bad',
      section: 'pinned',
      detail: `${looping} pinned ${count(looping, 'thread is', 'threads are')} looping: ${listed(pins.looping.map(pinName))}.`,
    });
  const waiting = pins.waiting.length;
  if (waiting > 0)
    out.push({
      key: 'waiting',
      value: String(waiting),
      label: 'waiting on you',
      short: count(waiting, 'waits on you', 'wait on you'),
      tone: 'warn',
      section: 'pinned',
      detail: `${waiting} pinned ${count(waiting, 'thread waits', 'threads wait')} on you: ${listed(pins.waiting.map((u) => u.title))}.`,
    });
  return out;
}

function sourcesItem(sources: Source[]): StripItem | null {
  const behind = sources.filter((s) => s.state !== 'ok' && s.state !== 'unconfigured');
  if (behind.length === 0) return null;
  const n = behind.length;
  const parts = behind.map((s) => `${s.label}: ${s.detail.trim().replace(/\.$/, '')}`);
  return {
    key: 'sources',
    value: String(n),
    label: count(n, 'source not current', 'sources not current'),
    short: 'not current',
    tone: 'warn',
    // Not current sources belong to different sections (or none, like account usage), so a
    // tap names them all instead of jumping to one.
    section: null,
    detail: `Not current: ${parts.join('; ')}.`,
  };
}

function worstOf(pins: PinnedProblems): StripWorst | null {
  const loop = pins.worstLoop;
  if (loop)
    return {
      state: 'looping',
      word: 'Looping',
      tone: 'bad',
      threadId: loop.id,
      title: loop.title,
      task: loop.looping.task ?? null,
      detail: sentence(loop.looping.text || 'Looping.'),
    };
  const wait = pins.waiting[0];
  if (wait)
    return {
      state: 'waiting',
      word: 'Waiting on you',
      tone: 'warn',
      threadId: wait.id,
      title: wait.title,
      task: null,
      detail: sentence(wait.blocked?.text || 'Waiting on you.'),
    };
  return null;
}

export function buildStrip(i: StripInputs): HeadlineStrip {
  const items: StripItem[] = [questionsItem(i)];
  if (i.decisions > 0)
    items.push({
      key: 'decisions',
      value: String(i.decisions),
      label: count(i.decisions, 'decision to look at', 'decisions to look at'),
      short: count(i.decisions, 'decision', 'decisions'),
      tone: 'warn',
      section: 'questions',
      detail: `${i.decisions} ${count(i.decisions, 'decision', 'decisions')} to look at.`,
    });
  items.push(...pinnedItems(i.pins));
  if (i.blocked > 0) {
    const names = blockedItems(i.projects);
    items.push({
      key: 'blocked',
      value: String(i.blocked),
      label: 'blocked',
      short: 'blocked',
      tone: 'warn',
      // The task lists have no section of their own on the page: a tap names the items.
      section: null,
      detail:
        names.length > 0
          ? `Blocked: ${listed(names)}.`
          : `${i.blocked} ${count(i.blocked, 'item is', 'items are')} blocked.`,
    });
  }
  if (i.overLimitBudgets > 0)
    items.push({
      key: 'budgets',
      value: String(i.overLimitBudgets),
      label: 'over budget (estimate)',
      short: 'over budget',
      tone: 'bad',
      section: 'spend',
      detail: `${i.overLimitBudgets} ${count(i.overLimitBudgets, 'budget is', 'budgets are')} at or over ${count(i.overLimitBudgets, 'its', 'their')} limit (estimate).`,
    });
  if (i.done)
    items.push({
      key: 'done',
      value: `${i.done.done}/${i.done.of}`,
      label: 'items done',
      short: 'done',
      tone: null,
      section: null,
      detail: `${i.done.done} of ${i.done.of} items landed or done (dropped items are not counted).`,
    });
  const sources = sourcesItem(i.sources);
  if (sources) items.push(sources);
  return { items, worst: worstOf(i.pins) };
}
