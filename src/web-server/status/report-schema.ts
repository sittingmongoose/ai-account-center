/**
 * Agent report documents (agent-reports v1): every rule the Status tab applies to a file it reads
 * from `<reports>/v1/writers/<writer>/` lives here, so a field added to the format is added in one
 * place. A document is projected field by field from an allowlist: unknown fields are ignored and
 * never reach the page. Reasons are fixed sentences that name the problem, never the file's text.
 */
import { createHash } from 'crypto';

export const THREAD_SCHEMA = 'agent-reports/thread@1';
export const TASKS_SCHEMA = 'agent-reports/tasks@1';
export const CARD_SCHEMA = 'agent-reports/card@1';
export const ANSWER_SCHEMA = 'agent-reports/answer@1';

export type ReportKind = 'thread' | 'tasks' | 'card';

/** Size caps from the format (bytes). */
export const REPORT_SIZE_CAPS: Readonly<Record<ReportKind, number>> = Object.freeze({
  thread: 16 * 1024,
  tasks: 512 * 1024,
  card: 64 * 1024,
});

export const MACHINES = ['vm', 'mac', 'windows', 'nas1'] as const;
export type MachineId = (typeof MACHINES)[number];

/** `<machine>.<pm-mail name>`, lowercase, 64 characters at most. */
const WRITER = /^(vm|mac|windows|nas1)\.[a-z0-9][a-z0-9.-]*$/;
const SLUG = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const LOCAL_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const PM_MAIL_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const OPTION_KEY = /^[A-Za-z0-9][A-Za-z0-9._-]{0,31}$/;
/** Control characters other than tab and newline, and bidirectional overrides. */
const CONTROL = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f؜‎‏‪-‮⁦-⁩]/;
const LONE_SURROGATE = /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/;
const ISO = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2}(\.\d{1,9})?)?(Z|[+-]\d{2}:\d{2})$/;

export function isWriterName(value: unknown): value is string {
  return typeof value === 'string' && value.length <= 64 && WRITER.test(value);
}

export function writerMachine(writer: string): MachineId {
  return writer.slice(0, writer.indexOf('.')) as MachineId;
}

export function isPmMailName(value: unknown): value is string {
  return typeof value === 'string' && PM_MAIL_NAME.test(value);
}

/** A card id is `<writer>--<local id>`. */
export function isCardId(value: unknown): value is string {
  if (typeof value !== 'string' || value.length > 130) return false;
  const at = value.indexOf('--');
  return at > 0 && isWriterName(value.slice(0, at)) && LOCAL_ID.test(value.slice(at + 2));
}

/**
 * Secret patterns: a file that matches one is refused whole (the format says no secrets). Each
 * entry names the kind it found; the matched text is never shown.
 */
const SECRET_PATTERNS: ReadonlyArray<readonly [string, RegExp]> = [
  ['a private key', /-----BEGIN [A-Z ]*PRIVATE KEY-----/],
  ['an API key', /\bsk-(?:ant-|proj-)?[A-Za-z0-9_-]{20,}/],
  ['a GitHub token', /\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,})/],
  ['a Slack token', /\bxox[abposr]-[A-Za-z0-9-]{10,}/],
  ['an AWS key', /\bAKIA[0-9A-Z]{16}\b/],
  ['a Google API key', /\bAIza[0-9A-Za-z_-]{35}/],
  ['a JSON web token', /\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}/],
  ['a bearer token', /\bBearer\s+[A-Za-z0-9._~+/-]{20,}/i],
  [
    'a credential field',
    /"(?:password|passwd|secret|client_?secret|api_?key|access_?token|refresh_?token|session_?token|cookie)"\s*:\s*"[^"]{6,}"/i,
  ],
  [
    'a credential in a URL',
    /[?&](?:access_token|token|api_key|apikey|key|sig|signature|password|code)=[^&\s"]{8,}/i,
  ],
  ['an email address', /[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}\b/],
];

/** The kind of secret a text contains, or null. */
export function findSecret(text: string): string | null {
  for (const [kind, pattern] of SECRET_PATTERNS) if (pattern.test(text)) return kind;
  return null;
}

export type Parsed<T> = { ok: true; value: T } | { ok: false; reason: string };

/** Thrown inside the projection; becomes `{ ok: false, reason }`. */
class Invalid extends Error {}

function fail(field: string): never {
  throw new Invalid(`${field} is missing or not valid`);
}

function obj(value: unknown, field: string): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) fail(field);
  return value as Record<string, unknown>;
}

function text(value: unknown, field: string, max: number): string {
  if (
    typeof value !== 'string' ||
    value.length === 0 ||
    value.length > max ||
    CONTROL.test(value) ||
    LONE_SURROGATE.test(value)
  )
    fail(field);
  return value;
}

function optText(value: unknown, field: string, max: number): string | null {
  return value === null || value === undefined || value === '' ? null : text(value, field, max);
}

function slug(value: unknown, field: string): string {
  if (typeof value !== 'string' || !SLUG.test(value)) fail(field);
  return value;
}

function optObj<T>(
  value: unknown,
  field: string,
  read: (value: Record<string, unknown>) => T
): T | null {
  return value === null || value === undefined ? null : read(obj(value, field));
}

function optSlug(value: unknown, field: string): string | null {
  return value === null || value === undefined ? null : slug(value, field);
}

function count(value: unknown, field: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) fail(field);
  return value;
}

function iso(value: unknown, field: string): string {
  if (typeof value !== 'string' || !ISO.test(value) || !Number.isFinite(Date.parse(value)))
    fail(field);
  return value;
}

function optIso(value: unknown, field: string): string | null {
  return value === null || value === undefined ? null : iso(value, field);
}

function bool(value: unknown, field: string, fallback: boolean): boolean {
  if (value === undefined || value === null) return fallback;
  if (typeof value !== 'boolean') fail(field);
  return value;
}

function list<T>(
  value: unknown,
  field: string,
  max: number,
  each: (item: unknown, at: string) => T
): T[] {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value) || value.length > max) fail(field);
  return value.map((item, index) => each(item, `${field}[${index}]`));
}

function oneOf<T extends string>(value: unknown, field: string, allowed: readonly T[]): T {
  if (typeof value !== 'string' || !(allowed as readonly string[]).includes(value)) fail(field);
  return value as T;
}

export interface ReportLink {
  label: string;
  path: string;
}

function link(value: unknown, field: string): ReportLink {
  const item = obj(value, field);
  return {
    label: text(item.label, `${field}.label`, 120),
    path: text(item.path, `${field}.path`, 600),
  };
}

export const THREAD_STATES = ['working', 'waiting', 'blocked', 'paused', 'idle', 'done'] as const;
export type ThreadState = (typeof THREAD_STATES)[number];
/** States that may carry `ttlSeconds: null` (shown by age, never stale). */
export const QUIET_STATES: ReadonlySet<ThreadState> = new Set([
  'waiting',
  'idle',
  'done',
  'paused',
]);
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

/** Shared head of every document. */
interface DocHead {
  writer: string;
  seq: number;
  writtenAt: string;
}

function head(doc: Record<string, unknown>, schema: string, writer: string): DocHead {
  if (doc.schema !== schema) fail('schema');
  if (doc.writer !== writer) throw new Invalid('writer does not match its folder');
  return { writer, seq: count(doc.seq, 'seq'), writtenAt: iso(doc.writtenAt, 'writtenAt') };
}

export interface ThreadDoc extends DocHead {
  /** Null only for waiting, idle, done or paused: a thread that ended its turn cannot heartbeat. */
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
    if (!QUIET_STATES.has(state)) fail('ttlSeconds');
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

export const CARD_STATES = ['open', 'withdrawn', 'superseded'] as const;
/**
 * question: options, answered in the deck. objection: a call the agent already made; the owner may
 * object, silence is not an answer. action: something only the owner can do; a Done button.
 */
export const CARD_KINDS = ['question', 'objection', 'action'] as const;
export type CardKind = (typeof CARD_KINDS)[number];
export const URGENCIES = ['now', 'today', 'this-week', 'whenever'] as const;
export type Urgency = (typeof URGENCIES)[number];

/** An image an option shows: a path inside the writer's own folder. */
export interface OptionImage {
  path: string;
  kind: 'built' | 'mockup';
}

export interface CardOption {
  key: string;
  label: string;
  /** What the owner sees or gets with this option. */
  gets: string | null;
  /** What it costs or changes elsewhere. */
  costs: string | null;
  image: OptionImage | null;
}

export interface CardDoc extends DocHead {
  id: string;
  /** A short id the owner can quote ("F-Q3"); the local id when the file names none. */
  label: string;
  project: string | null;
  stream: string | null;
  askedBy: string | null;
  replyTo: { pmMail: string | null; t3Thread: string | null };
  state: (typeof CARD_STATES)[number];
  supersededBy: string | null;
  /** Why a withdrawn card was withdrawn ("Settled by your note"). */
  reason: string | null;
  title: string;
  /** The situation today in plain words (`context` is read as an alias). */
  now: string | null;
  nowAsOf: string | null;
  oldAnswer: { text: string; at: string | null } | null;
  whatChanged: string | null;
  options: CardOption[];
  recommendation: { key: string; why: string | null } | null;
  multiple: boolean;
  urgency: Urgency;
  blocks: string | null;
  batch: string | null;
  evidence: ReportLink[];
  /** The agent's replies to the owner's notes. */
  replies: Array<{ at: string; text: string }>;
  askedAt: string;
  kind: CardKind;
  /** When an answer is needed and what happens or starts costing without one. */
  neededBy: { at: string | null; ifNoAnswer: string | null } | null;
  /** The owner's rule this card loosens, and how. Shown before anything else. */
  exception: { rule: string; text: string } | null;
  reversible: { yes: boolean; how: string | null } | null;
}

/** A relative path inside a writer's folder: plain segments, no `..`, no absolute or Windows paths. */
const IMAGE_PATH = /^[A-Za-z0-9_-][A-Za-z0-9._-]{0,99}(\/[A-Za-z0-9_-][A-Za-z0-9._-]{0,99}){0,5}$/;

export function isWriterFilePath(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length <= 300 &&
    IMAGE_PATH.test(value) &&
    !value.split('/').some((segment) => segment === '.' || segment === '..')
  );
}

function optionImage(value: unknown, field: string): OptionImage | null {
  if (value === null || value === undefined) return null;
  const image = obj(value, field);
  if (!isWriterFilePath(image.path)) fail(`${field}.path`);
  return {
    path: image.path,
    kind: oneOf(image.kind, `${field}.kind`, ['built', 'mockup'] as const),
  };
}

function card(doc: Record<string, unknown>, writer: string, fileId: string): CardDoc {
  const base = head(doc, CARD_SCHEMA, writer);
  if (doc.id !== fileId || !isCardId(doc.id) || !fileId.startsWith(`${writer}--`))
    throw new Invalid('id does not match its file name and writer');
  const replyTo =
    doc.replyTo === undefined || doc.replyTo === null ? {} : obj(doc.replyTo, 'replyTo');
  const pmMail = replyTo.pmMail === undefined || replyTo.pmMail === null ? null : replyTo.pmMail;
  if (pmMail !== null && !isPmMailName(pmMail)) fail('replyTo.pmMail');
  const options = list(doc.options, 'options', 4, (value, at): CardOption => {
    const option = obj(value, at);
    if (typeof option.key !== 'string' || !OPTION_KEY.test(option.key)) fail(`${at}.key`);
    return {
      key: option.key,
      label: text(option.label, `${at}.label`, 300),
      gets: optText(option.gets, `${at}.gets`, 1000),
      costs: optText(option.costs, `${at}.costs`, 1000),
      image: optionImage(option.image, `${at}.image`),
    };
  });
  const kind = oneOf(doc.kind ?? 'question', 'kind', CARD_KINDS);
  if (kind === 'question' && options.length < 2) throw new Invalid('a card needs 2 to 4 options');
  if (kind === 'objection' && options.length === 1)
    throw new Invalid('an objection has no options or 2 to 4');
  if (kind === 'action' && options.length > 0) throw new Invalid('an action card has no options');
  if (new Set(options.map((option) => option.key)).size !== options.length)
    throw new Invalid('options repeat a key');
  let recommendation: CardDoc['recommendation'] = null;
  if (doc.recommendation !== null && doc.recommendation !== undefined) {
    const value = obj(doc.recommendation, 'recommendation');
    if (!options.some((option) => option.key === value.key)) fail('recommendation.key');
    recommendation = {
      key: value.key as string,
      why: optText(value.why, 'recommendation.why', 1000),
    };
  }
  let oldAnswer: CardDoc['oldAnswer'] = null;
  if (doc.oldAnswer !== null && doc.oldAnswer !== undefined) {
    const value = obj(doc.oldAnswer, 'oldAnswer');
    oldAnswer = {
      text: text(value.text, 'oldAnswer.text', 2000),
      at: optIso(value.at, 'oldAnswer.at'),
    };
  }
  const state = oneOf(doc.state ?? 'open', 'state', CARD_STATES);
  const local = fileId.slice(fileId.indexOf('--') + 2);
  return {
    ...base,
    id: fileId,
    label: optText(doc.label, 'label', 32) ?? local,
    project: optSlug(doc.project, 'project'),
    stream: optSlug(doc.stream, 'stream'),
    askedBy: optText(doc.askedBy, 'askedBy', 64),
    replyTo: { pmMail, t3Thread: optText(replyTo.t3Thread, 'replyTo.t3Thread', 200) },
    state,
    supersededBy: state === 'superseded' ? optText(doc.supersededBy, 'supersededBy', 130) : null,
    reason: state === 'open' ? null : optText(doc.reason, 'reason', 500),
    title: text(doc.title, 'title', 300),
    now: optText(doc.now ?? doc.context, 'now', 4000),
    nowAsOf: optIso(doc.nowAsOf, 'nowAsOf'),
    oldAnswer,
    whatChanged: optText(doc.whatChanged, 'whatChanged', 2000),
    options,
    recommendation,
    multiple: bool(doc.multiple, 'multiple', false),
    urgency: oneOf(doc.urgency ?? 'whenever', 'urgency', URGENCIES),
    blocks: optText(doc.blocks, 'blocks', 300),
    batch: optSlug(doc.batch, 'batch'),
    evidence: list(doc.evidence, 'evidence', 20, link),
    kind,
    neededBy: optObj(doc.neededBy, 'neededBy', (value) => ({
      at: optIso(value.at, 'neededBy.at'),
      ifNoAnswer: optText(value.ifNoAnswer, 'neededBy.ifNoAnswer', 500),
    })),
    exception: optObj(doc.exception, 'exception', (value) => ({
      rule: text(value.rule, 'exception.rule', 300),
      text: text(value.text, 'exception.text', 1000),
    })),
    reversible: optObj(doc.reversible, 'reversible', (value) => {
      if (typeof value.yes !== 'boolean') fail('reversible.yes');
      return { yes: value.yes, how: optText(value.how, 'reversible.how', 500) };
    }),
    replies: list(doc.replies, 'replies', 50, (value, at) => {
      const reply = obj(value, at);
      return { at: iso(reply.at, `${at}.at`), text: text(reply.text, `${at}.text`, 2000) };
    }),
    askedAt: iso(doc.askedAt ?? doc.writtenAt, 'askedAt'),
  };
}

export type ReportDoc = ThreadDoc | TasksDoc | CardDoc;

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
  kind: ReportKind,
  bytes: Buffer,
  writer: string,
  cardId = ''
): Parsed<ParsedReport> {
  if (bytes.length > REPORT_SIZE_CAPS[kind])
    return { ok: false, reason: `larger than the ${REPORT_SIZE_CAPS[kind] / 1024} KB cap` };
  if (bytes.length === 0) return { ok: false, reason: 'empty' };
  if (bytes.includes(0)) return { ok: false, reason: 'contains NUL bytes from a damaged write' };
  let source: string;
  try {
    source = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch {
    return { ok: false, reason: 'not UTF-8 text' };
  }
  const secret = findSecret(source);
  if (secret) return { ok: false, reason: `refused: it contains what looks like ${secret}` };
  let raw: unknown;
  try {
    raw = JSON.parse(source);
  } catch {
    return { ok: false, reason: 'not valid JSON' };
  }
  try {
    const doc = obj(raw, 'document');
    const value =
      kind === 'thread'
        ? thread(doc, writer)
        : kind === 'tasks'
          ? tasks(doc, writer)
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
