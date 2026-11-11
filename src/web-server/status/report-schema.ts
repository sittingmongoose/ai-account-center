/**
 * Agent report documents (agent-reports v1): every rule the Status tab applies to a file it reads
 * from `<reports>/v1/writers/<writer>/` lives here, so a field added to the format is added in one
 * place. A document is projected field by field from an allowlist: unknown fields are ignored and
 * never reach the page. Reasons are fixed sentences that name the problem, never the file's text.
 */
import { createHash } from 'crypto';

export type ReportKind = 'thread' | 'tasks' | 'card' | 'run';

/** Size caps from the format (bytes). */
export const REPORT_SIZE_CAPS: Readonly<Record<ReportKind, number>> = Object.freeze({
  thread: 16 * 1024,
  tasks: 512 * 1024,
  card: 64 * 1024,
  run: 32 * 1024,
});

import { card, type CardDoc } from './card-schema';
import { run, type RunDoc } from './run-schema';
import {
  count,
  fail,
  head,
  TASKS_SCHEMA,
  THREAD_SCHEMA,
  type DocHead,
  Invalid,
  jsonFromBytes,
  link,
  list,
  obj,
  oneOf,
  optIso,
  optSlug,
  optText,
  slug,
  text,
  type Parsed,
  type ReportLink,
} from './schema-helpers';

export * from './schema-helpers';
export * from './card-schema';
export * from './run-schema';

export const THREAD_STATES = ['working', 'waiting', 'blocked', 'paused', 'idle', 'done'] as const;
export type ThreadState = (typeof THREAD_STATES)[number];
/**
 * Documented kinds: owner, agent, review, lock, resource, quota. Any short slug is accepted and
 * shown as written, so a new kind needs no rebuild.
 */

export interface BlockedOn {
  kind: string;
  what: string;
  since: string | null;
}

function blockedOn(value: unknown, field: string): BlockedOn | null {
  if (value === null || value === undefined) return null;
  const item = obj(value, field);
  return {
    kind: slug(item.kind, `${field}.kind`),
    what: text(item.what, `${field}.what`, 500),
    since: optIso(item.since, `${field}.since`),
  };
}

export interface ThreadDoc extends DocHead {
  /**
   * Null for any state: a thread that ended its turn cannot heartbeat, whatever state it ended in
   * (blocked on the owner included). Such a thread is shown by age, never stale for lack of one.
   */
  ttlSeconds: number | null;
  agent: {
    name: string | null;
    harness: string | null;
    model: string | null;
    effort: string | null;
    account: string | null;
    machine: string | null;
    t3Thread: string | null;
    parent: string | null;
  };
  project: string | null;
  stream: string | null;
  state: ThreadState;
  doing: string | null;
  blockedOn: BlockedOn | null;
  progress: { done: number; total: number; unit: string | null } | null;
  links: ReportLink[];
}

function thread(doc: Record<string, unknown>, writer: string): ThreadDoc {
  const agent = doc.agent === undefined || doc.agent === null ? {} : obj(doc.agent, 'agent');
  const state = oneOf(doc.state, 'state', THREAD_STATES);
  let ttl: number | null = 1800;
  if (doc.ttlSeconds === null) {
    ttl = null;
  } else if (doc.ttlSeconds !== undefined) {
    ttl = count(doc.ttlSeconds, 'ttlSeconds');
    if (ttl < 60 || ttl > 7 * 86_400) fail('ttlSeconds');
  }
  let progress: ThreadDoc['progress'] = null;
  if (doc.progress !== null && doc.progress !== undefined) {
    const item = obj(doc.progress, 'progress');
    progress = {
      done: count(item.done, 'progress.done'),
      total: count(item.total, 'progress.total'),
      unit: optText(item.unit, 'progress.unit', 40),
    };
  }
  return {
    ...head(doc, THREAD_SCHEMA, writer),
    ttlSeconds: ttl,
    agent: {
      name: optText(agent.name, 'agent.name', 64),
      harness: optText(agent.harness, 'agent.harness', 64),
      model: optText(agent.model, 'agent.model', 120),
      effort: optText(agent.effort, 'agent.effort', 40),
      account: optText(agent.account, 'agent.account', 120),
      machine: optText(agent.machine, 'agent.machine', 64),
      t3Thread: optText(agent.t3Thread, 'agent.t3Thread', 200),
      parent: optText(agent.parent, 'agent.parent', 64),
    },
    project: optSlug(doc.project, 'project'),
    stream: optSlug(doc.stream, 'stream'),
    state,
    doing: optText(doc.doing, 'doing', 500),
    blockedOn: blockedOn(doc.blockedOn, 'blockedOn'),
    progress,
    links: list(doc.links, 'links', 20, link),
  };
}

export const ITEM_STATES = [
  'todo',
  'working',
  'review',
  'fixing',
  'blocked',
  'ready',
  'landed',
  'done',
  'dropped',
] as const;
export type ItemState = (typeof ITEM_STATES)[number];

export interface TaskItem {
  id: string;
  title: string;
  state: ItemState;
  owner: string | null;
  branch: string | null;
  head: string | null;
  reviews: Array<{ by: string; verdict: string; at: string | null }>;
  landed: { sha: string; at: string | null } | null;
  blockedOn: BlockedOn | null;
  why: string | null;
  updatedAt: string | null;
}

export interface TasksDoc extends DocHead {
  project: string;
  projectLabel: string | null;
  stream: string;
  streamLabel: string | null;
  owner: string | null;
  goal: string | null;
  board: string | null;
  items: TaskItem[];
}

function item(value: unknown, field: string): TaskItem {
  const row = obj(value, field);
  let landed: TaskItem['landed'] = null;
  if (row.landed !== null && row.landed !== undefined) {
    const value = obj(row.landed, `${field}.landed`);
    landed = {
      sha: text(value.sha, `${field}.landed.sha`, 64),
      at: optIso(value.at, `${field}.landed.at`),
    };
  }
  return {
    id: text(row.id, `${field}.id`, 64),
    title: text(row.title, `${field}.title`, 300),
    state: oneOf(row.state, `${field}.state`, ITEM_STATES),
    owner: optText(row.owner, `${field}.owner`, 64),
    branch: optText(row.branch, `${field}.branch`, 200),
    head: optText(row.head, `${field}.head`, 64),
    reviews: list(row.reviews, `${field}.reviews`, 20, (review, at) => {
      const value = obj(review, at);
      return {
        by: text(value.by, `${at}.by`, 120),
        verdict: text(value.verdict, `${at}.verdict`, 40),
        at: optIso(value.at, `${at}.at`),
      };
    }),
    landed,
    blockedOn: blockedOn(row.blockedOn, `${field}.blockedOn`),
    why: optText(row.why, `${field}.why`, 500),
    updatedAt: optIso(row.updatedAt, `${field}.updatedAt`),
  };
}

function tasks(doc: Record<string, unknown>, writer: string): TasksDoc {
  const items = list(doc.items, 'items', 2000, item);
  const ids = new Set(items.map((row) => row.id));
  if (ids.size !== items.length) throw new Invalid('items repeat an id');
  return {
    ...head(doc, TASKS_SCHEMA, writer),
    project: slug(doc.project, 'project'),
    projectLabel: optText(doc.projectLabel, 'projectLabel', 120),
    stream: slug(doc.stream, 'stream'),
    streamLabel: optText(doc.streamLabel, 'streamLabel', 120),
    owner: optText(doc.owner, 'owner', 64),
    goal: optText(doc.goal, 'goal', 500),
    board: optText(doc.board, 'board', 600),
    items,
  };
}

export type ReportDoc = ThreadDoc | TasksDoc | CardDoc | RunDoc;

export interface ParsedReport<T extends ReportDoc = ReportDoc> {
  doc: T;
  sha256: string;
}

/**
 * Bytes of one report file -> its projected document. `cardId` is the file name without `.json`
 * for a card. Every refusal is a fixed reason.
 */
export function parseReportFile(
  kind: 'thread',
  bytes: Buffer,
  writer: string
): Parsed<ParsedReport<ThreadDoc>>;
export function parseReportFile(
  kind: 'tasks',
  bytes: Buffer,
  writer: string
): Parsed<ParsedReport<TasksDoc>>;
export function parseReportFile(
  kind: 'card',
  bytes: Buffer,
  writer: string,
  cardId: string
): Parsed<ParsedReport<CardDoc>>;
export function parseReportFile(
  kind: 'run',
  bytes: Buffer,
  writer: string
): Parsed<ParsedReport<RunDoc>>;
export function parseReportFile(
  kind: ReportKind,
  bytes: Buffer,
  writer: string,
  cardId = ''
): Parsed<ParsedReport> {
  const raw = jsonFromBytes(bytes, REPORT_SIZE_CAPS[kind]);
  if (!raw.ok) return raw;
  try {
    const doc = obj(raw.value, 'document');
    const value =
      kind === 'thread'
        ? thread(doc, writer)
        : kind === 'tasks'
          ? tasks(doc, writer)
          : kind === 'run'
            ? run(doc, writer)
            : card(doc, writer, cardId);
    return {
      ok: true,
      value: { doc: value, sha256: createHash('sha256').update(bytes).digest('hex') },
    };
  } catch (error) {
    if (error instanceof Invalid) return { ok: false, reason: error.message };
    return { ok: false, reason: 'could not be read' };
  }
}
