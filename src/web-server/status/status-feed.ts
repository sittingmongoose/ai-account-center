/**
 * R2 (DESIGN-M3-COMPLETE 1.2): Status, fed. StatusService keeps reading and collecting as today; what changes is
 * what crosses to the Rust service and back.
 *
 * - **Pushed only when it changed, with a generation number** (`PUT /internal/feed/<source>`, R1's feed): each of
 *   five sources holds some of the composer's top-level input members (`status-reports`: reports;
 *   `status-quota`: quota, spendByStream; `status-pinned`: pinned; `status-collected`: registry, machines,
 *   browsers, simlock, usage; `status-answers`: answers, sends, answerHistoryProblem), as `JSON.stringify` writes
 *   them for the envelope. A sixth, `status-facts`, holds the time zone and the fixed lists (hiddenThreadNames,
 *   machineIds, machineLabels) with that generation's `Intl` facts, computed once with rustReadInput's code over
 *   the whole set: the clock and day words of every time value, the `Date.parse` epochs and the `localeCompare`
 *   ranks.
 * - **Change keys cost no serialization** where the owner replaces what it changed: each member is compared with
 *   what was last pushed by identity, to a depth that covers the objects `readInputs()` builds fresh per read (the
 *   reports snapshot's arrays hold the reader's own immutable rows; the collected panels are replaced on every
 *   collection). The answers change in place (delivery steps), so their key is their JSON text.
 * - **Per request** only the clock, now's clock and day words (two cached `Intl` formats) and the six generation
 *   ids cross (`x-aac-feed`; no body). Rust composes with the existing Status composer over the held generations
 *   and answers the final bytes and `ETag`, which the front door writes without parsing (`sendRustView`).
 * - **Anything unproven declines** with a named reason and the original handler answers from the same read: no
 *   service or another root, an input member this code does not know, a value JSON cannot carry exactly, a push
 *   that did not land, a generation the service does not hold as named (`stale`, which also makes the next request
 *   push again), any decline of Rust's.
 *
 * Generations are per service boot: a new boot holds none, so the front door pushes again on the next request.
 */
import type { Request, Response } from 'express';
import { RustService } from '../rust-service';
import { rustGroupOn } from '../rust-service-groups';
import {
  dateWordsIn,
  localeRanks,
  readFacts,
  rustInputText,
  type DateWords,
  type ReadFacts,
} from '../rust-read-input';
import { sendRustView } from '../rust-view-reply';
import { MACHINE_IDS, MACHINE_LABELS } from '../services/dashboard-hosts';
import { serverZoneFacts } from '../views/home-zone';
import { BootFeeds, ensure, type Generation } from '../views/home-feed';
import type { StatusInputs } from './status-view';

const ROUTE = '/api/status';
/** crates/aac-http status_fed `{VIEW, VERSION}`. */
export const STATUS_FED_VIEW = 'status@1';

/** The fed sources and their members, in the envelope's order (crates/aac-http status_fed.rs). */
export const STATUS_PARTS = [
  { source: 'status-reports', members: ['reports'] },
  { source: 'status-quota', members: ['quota', 'spendByStream'] },
  { source: 'status-pinned', members: ['pinned'] },
  { source: 'status-collected', members: ['registry', 'machines', 'browsers', 'simlock', 'usage'] },
  { source: 'status-answers', members: ['answers', 'sends', 'answerHistoryProblem'] },
  {
    source: 'status-facts',
    members: ['timeZone', 'hiddenThreadNames', 'machineIds', 'machineLabels'],
  },
] as const;
type Part = (typeof STATUS_PARTS)[number];

/**
 * How each member's change is seen. A number: identity to that depth (0: the same object; 1: an object whose
 * members are the same objects; 2: one level further), which holds because StatusService and its readers replace
 * what they change rather than editing it. `text`: the member's JSON text (the answers' delivery steps change in
 * place).
 */
const CHANGE: Record<string, number | 'text'> = {
  reports: 2,
  quota: 1,
  spendByStream: 0,
  pinned: 1,
  registry: 0,
  machines: 1,
  browsers: 0,
  simlock: 0,
  usage: 0,
  answers: 'text',
  sends: 'text',
  answerHistoryProblem: 'text',
  timeZone: 0,
  hiddenThreadNames: 1,
  machineIds: 0,
  machineLabels: 0,
};
const KNOWN = new Set(['now', ...Object.keys(CHANGE)]);

/** Identity to `depth`: what `readInputs()` builds fresh per read is compared member by member. */
export function sameTo(a: unknown, b: unknown, depth: number): boolean {
  if (a === b) return true;
  if (depth <= 0 || !a || !b || typeof a !== 'object' || typeof b !== 'object') return false;
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
    return a.every((value, i) => sameTo(value, b[i], depth - 1));
  }
  if (a instanceof Map || b instanceof Map) {
    if (!(a instanceof Map) || !(b instanceof Map) || a.size !== b.size) return false;
    const right = [...b];
    let i = 0;
    for (const [key, value] of a) {
      const [otherKey, other] = right[i++];
      if (key !== otherKey || !sameTo(value, other, depth - 1)) return false;
    }
    return true;
  }
  if (Object.getPrototypeOf(a) !== Object.getPrototypeOf(b)) return false;
  const left = Object.keys(a);
  const right = Object.keys(b);
  if (left.length !== right.length) return false;
  return left.every(
    (key, i) =>
      key === right[i] &&
      sameTo((a as Record<string, unknown>)[key], (b as Record<string, unknown>)[key], depth - 1)
  );
}

interface Built {
  token: number;
  /** The source's data text, or null when JSON cannot carry it exactly (the request declines `input`). */
  text: string | null;
  facts: ReadFacts | null;
}

/** What the front door has seen and pushed for one service boot. */
class StatusFeeds {
  readonly feeds: BootFeeds;
  /** Per member: the value last seen and, for text members, its text. */
  readonly seen = new Map<string, { value: unknown; text?: string }>();
  /** Per source: a number that moves whenever one of its members changed. */
  readonly tokens = new Map<string, number>();
  readonly built = new Map<string, Built>();
  /** The clock and day formats of one time zone, and the words already formatted in it. */
  zone: { name: string; format: (ms: number) => DateWords; words: Map<number, DateWords> } | null =
    null;
  constructor(boot: string) {
    this.feeds = new BootFeeds(boot);
  }

  /**
   * The source's change token after comparing its members with what was seen last. Nothing is recorded unless
   * every member could be compared (a text member JSON cannot carry throws, and the request declines).
   */
  observe(part: Part, data: Record<string, unknown>): number {
    const updates: Array<[string, { value: unknown; text?: string }]> = [];
    for (const member of part.members) {
      const value = data[member];
      const how = CHANGE[member];
      const before = this.seen.get(member);
      if (how === 'text') {
        const text = value === undefined ? undefined : rustInputText(value);
        if (!before || before.text !== text) updates.push([member, { value, text }]);
      } else if (!before || !sameTo(before.value, value, how)) updates.push([member, { value }]);
    }
    for (const [member, seen] of updates) this.seen.set(member, seen);
    const changed = updates.length > 0 || !this.tokens.has(part.source);
    const token = (this.tokens.get(part.source) ?? 0) + (changed ? 1 : 0);
    this.tokens.set(part.source, token);
    return token;
  }

  /** The source's text and facts at its current token, built once per token. */
  build(part: Part, data: Record<string, unknown>): Built {
    const token = this.tokens.get(part.source) ?? 0;
    const held = this.built.get(part.source);
    if (held && held.token === token) return held;
    let built: Built;
    try {
      const object: Record<string, unknown> = {};
      for (const member of part.members) object[member] = data[member];
      const text = rustInputText(object);
      built = { token, text, facts: readFacts(JSON.parse(text)) };
    } catch {
      built = { token, text: null, facts: null };
    }
    this.built.set(part.source, built);
    return built;
  }

  formats(timeZone: string): NonNullable<StatusFeeds['zone']> {
    if (!this.zone || this.zone.name !== timeZone)
      this.zone = { name: timeZone, format: dateWordsIn(timeZone), words: new Map() };
    return this.zone;
  }

  /**
   * The facts of the whole set (every source at its current token): what `rustReadInput` computes over the whole
   * input, less the request's own clock (which crosses per request). Words already formatted in this zone are kept
   * for the times still present.
   */
  facts(builts: Built[], timeZone: string): string | null {
    if (builts.some((b) => !b.facts)) return null;
    const words = new Set<string>();
    const times = new Set<number>();
    const epochs: Record<string, number> = {};
    for (const { facts } of builts) {
      if (!facts) return null;
      for (const word of facts.words) words.add(word);
      for (const time of facts.times) times.add(time);
      Object.assign(epochs, facts.epochs);
    }
    const zone = this.formats(timeZone);
    const kept = new Map<number, DateWords>();
    const dates: Record<string, DateWords> = {};
    for (const ms of times) {
      const value = zone.words.get(ms) ?? zone.format(ms);
      kept.set(ms, value);
      dates[String(ms)] = value;
    }
    zone.words = kept;
    return JSON.stringify({ dates, epochs, order: localeRanks(words) });
  }
}

const boots = new WeakMap<RustService, StatusFeeds>();
function feedsOf(service: RustService, boot: string): StatusFeeds {
  let feeds = boots.get(service);
  if (!feeds || feeds.feeds.boot !== boot) {
    feeds = new StatusFeeds(boot);
    boots.set(service, feeds);
  }
  return feeds;
}

/** Each service's `groups.status`, read once (config.yaml is not read per request). */
const groups = new WeakMap<RustService, boolean>();

/** The service that answers Status from fed generations: the Status group on, under mode on. */
export function statusFedService(res: Response): RustService | null {
  const service: unknown = res.locals.rustService;
  if (!(service instanceof RustService) || service.mode !== 'on') return null;
  let on = groups.get(service);
  if (on === undefined) {
    on = rustGroupOn('status');
    groups.set(service, on);
  }
  return on ? service : null;
}

/**
 * G16c: answer GET /api/status from the service's own producers (the owned Status group). The service holds
 * every generation and composes the view; the request carries only its clock and the display zone's offsets
 * (the one `Intl` fact a server cannot reproduce, `serverZoneFacts`). True when the Rust answer was written.
 * False means the service declined or did not answer: the Status owner restarts its own producers and this
 * process answers from them for the rest of its life, so no page ever sees a half-produced view.
 */
export async function answerStatusOwned(
  req: Request,
  res: Response,
  service: RustService,
  input: StatusInputs
): Promise<boolean> {
  if (!Number.isSafeInteger(input.now) || typeof input.timeZone !== 'string') return false;
  const zone = serverZoneFacts(input.timeZone, input.now);
  if (!zone) return false;
  const reply = await service.fedView(
    req,
    ROUTE,
    {
      'x-aac-owned': '1',
      'x-aac-now': String(input.now),
      'x-aac-zone': JSON.stringify({ timeZone: input.timeZone, ...zone }),
    },
    STATUS_FED_VIEW
  );
  if (typeof reply === 'string' || !reply) return false;
  sendRustView(res, reply);
  return true;
}

/**
 * Answer GET /api/status from fed generations. True when the Rust answer was written; false when the request
 * declined (counted with its reason) and the original handler must answer from the same `input`, with nothing
 * written.
 */
export async function answerStatusFed(
  req: Request,
  res: Response,
  service: RustService,
  input: StatusInputs,
  scope: string
): Promise<boolean> {
  const declined = (reason: string): false => {
    service.fedDeclined(ROUTE, reason);
    return false;
  };
  const boot = service.feedBoot(scope);
  if (!boot) return declined('root');
  if (
    Object.keys(input).some((key) => !KNOWN.has(key)) ||
    !Number.isSafeInteger(input.now) ||
    typeof input.timeZone !== 'string'
  )
    return declined('input');
  const state = feedsOf(service, boot);
  const data: Record<string, unknown> = {
    ...input,
    machineIds: MACHINE_IDS,
    machineLabels: MACHINE_LABELS,
  };
  let tokens: number[];
  try {
    tokens = STATUS_PARTS.map((part) => state.observe(part, data));
  } catch {
    // A text member JSON cannot carry (a non-finite number): TS's own answer.
    return declined('input');
  }
  const facts = STATUS_PARTS[STATUS_PARTS.length - 1];
  const generations: Generation[] = await Promise.all(
    STATUS_PARTS.map((part, i) => {
      const key = part === facts ? tokens.join('.') : String(tokens[i]);
      return ensure(service, state.feeds, part.source, key, () => {
        const built = state.build(part, data);
        if (built.text === null) return null;
        if (part !== facts) return { data: built.text, facts: '{}' };
        const all = STATUS_PARTS.map((p) => state.build(p, data));
        const text = state.facts(all, input.timeZone);
        return text === null ? null : { data: built.text, facts: text };
      });
    })
  );
  const ids: string[] = [];
  for (const generation of generations) {
    if ('declined' in generation) return declined(generation.declined);
    ids.push(generation.generation);
  }
  const now = state.formats(input.timeZone).format(input.now);
  const headers = {
    'x-aac-feed': JSON.stringify({
      now: input.now,
      clock: now.clock,
      day: now.day,
      reports: ids[0],
      quota: ids[1],
      pinned: ids[2],
      collected: ids[3],
      answers: ids[4],
      facts: ids[5],
    }),
  };
  const reply = await service.fedView(req, ROUTE, headers, STATUS_FED_VIEW);
  if (typeof reply === 'string') {
    // The service no longer holds what this front door recorded (a push raced, or it lost its feeds): push again
    // on the next request instead of declining until the content changes.
    if (reply === 'stale') state.feeds.held.clear();
    return false;
  }
  if (!reply) return false;
  sendRustView(res, reply);
  return true;
}
