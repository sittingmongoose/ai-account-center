/**
 * R4 (DESIGN-M3-COMPLETE 1.4): the owned Scoreboard forward. The service holds the catalog and
 * the runs store, ingests the share and composes the view in process; TypeScript keeps the
 * filter parse and auth, computes the presentation facts once per (generation, catalog, zone)
 * over the service's unfiltered input (a superset of every filter's rows), and forwards the
 * request's filters, clock and facts generation. Rust answers the final bytes and ETag, which
 * the front door writes without parsing; any decline is today's handler from the same read.
 */
import type { Request, Response } from 'express';
import { RustService } from '../rust-service';
import { rustOwnedScoreboardOn } from '../rust-service-groups';
import { dateWordsIn, localeRanks, readFacts, rustInputText } from '../rust-read-input';
import { sendRustView } from '../rust-view-reply';
import { onRustServiceEvent } from '../rust-service-events';
import { addCalendarDays, localMidnight } from '../services/account-analytics-range';
import {
  DEFAULT_DASHBOARD_TIME_ZONE,
  readDashboardPreferences,
} from '../services/dashboard-preferences';
import { ALL_RUNS } from './scoreboard-service';
import type { ScoreboardFilters } from './scoreboard-view';

const ROUTE = '/api/scoreboard';
/** crates/aac-http scoreboard_owned `respond_tagged("scoreboard", 1, …)`. */
export const SCOREBOARD_OWNED_VIEW = 'scoreboard@1';

interface OwnedState {
  boot: string;
  generation: number;
  catalogHash: string;
  zone: string;
  factsId: string;
  /** Set when the service moved past what this front door last pushed. */
  stale: boolean;
}

const states = new WeakMap<RustService, OwnedState>();
const events = new WeakMap<RustService, () => void>();
const groups = new WeakMap<RustService, boolean>();

/** The service that answers the Scoreboard from its own store: the group on, under mode on. */
export function scoreboardOwnedService(res: Response): RustService | null {
  const service: unknown = res.locals.rustService;
  if (!(service instanceof RustService) || service.mode !== 'on') return null;
  let on = groups.get(service);
  if (on === undefined) {
    on = rustOwnedScoreboardOn();
    groups.set(service, on);
  }
  return on ? service : null;
}

function timeZone(): string {
  try {
    return readDashboardPreferences().timeZone;
  } catch {
    return DEFAULT_DASHBOARD_TIME_ZONE;
  }
}

/**
 * The facts generation for the service's current (generation, catalog, zone): fetch the
 * unfiltered input once per generation, compute the envelope facts over it, push them.
 * Null when the service is absent, the push did not land, or the input cannot be carried.
 */
async function ensureFacts(
  service: RustService,
  boot: string,
  zone: string
): Promise<OwnedState | null> {
  let state = states.get(service);
  if (state && state.boot === boot && state.zone === zone && !state.stale) return state;
  const input = (await service.internalJson('/internal/scoreboard/inputs')) as {
    generation?: unknown;
    catalogHash?: unknown;
    catalog?: { taskTypes?: Array<{ id: string; label: string }> };
    runs?: unknown;
    docs?: unknown;
    identity?: unknown;
    seedFailures?: unknown;
  } | null;
  if (
    !input ||
    typeof input.generation !== 'number' ||
    typeof input.catalogHash !== 'string' ||
    !input.catalog ||
    !input.runs ||
    !input.docs
  ) {
    return null;
  }
  const data = {
    now: 0,
    timeZone: zone,
    root: '',
    catalog: input.catalog,
    runs: input.runs,
    joinReviews: [],
    seedFailures: input.seedFailures ?? [],
    docs: input.docs,
    finish: {
      filters: {
        ...ALL_RUNS,
        available: true,
        taskTypes: (input.catalog.taskTypes ?? []).map((type) => ({
          id: type.id,
          label: type.label,
        })),
      },
      identity: input.identity ?? null,
      history: { store: 'on', runs: null, importing: false },
    },
  };
  let factsText: string;
  let dataText: string;
  try {
    const parsed = JSON.parse(rustInputText(data));
    const { words, times, epochs } = readFacts(parsed);
    const format = dateWordsIn(zone);
    const dates = Object.fromEntries([...times].map((ms) => [String(ms), format(ms)]));
    factsText = JSON.stringify({ dates, epochs, order: localeRanks(words) });
    dataText = rustInputText({ timeZone: zone });
  } catch {
    return null;
  }
  const factsId = `${input.generation}:${input.catalogHash}:${zone}`;
  const held = await service.pushFeed(boot, 'scoreboard-facts', factsId, dataText, factsText);
  if (held === null) return null;
  state = {
    boot,
    generation: input.generation,
    catalogHash: input.catalogHash,
    zone,
    factsId: held,
    stale: false,
  };
  states.set(service, state);
  if (!events.has(service)) {
    events.set(
      service,
      onRustServiceEvent((event) => {
        if (event.event === 'scoreboard-changed') {
          const held = states.get(service);
          if (held) held.stale = true;
        }
      })
    );
  }
  return state;
}

/**
 * Answer GET /api/scoreboard from the owned store. True when the Rust answer was written;
 * false when the request declined (counted with its reason) and today's handler must answer.
 */
export async function answerScoreboardOwned(
  req: Request,
  res: Response,
  service: RustService,
  filters: ScoreboardFilters,
  scope: string
): Promise<boolean> {
  const declined = (reason: string): false => {
    service.fedDeclined(ROUTE, reason);
    return false;
  };
  const boot = service.feedBoot(scope);
  if (!boot) return declined('root');
  const zone = timeZone();
  const state = await ensureFacts(service, boot, zone);
  if (!state) return declined('feed');
  const now = Date.now();
  const words = dateWordsIn(zone)(now);
  const headers = {
    'x-aac-feed': JSON.stringify({
      now,
      clock: words.clock,
      day: words.day,
      timeZone: zone,
      from: filters.from,
      to: filters.to,
      source: filters.source,
      taskType: filters.taskType,
      fromMs: filters.from ? localMidnight(filters.from, zone) : null,
      toMs: filters.to ? localMidnight(addCalendarDays(filters.to, 1), zone) - 1 : null,
      generation: state.generation,
      catalogHash: state.catalogHash,
      facts: state.factsId,
    }),
  };
  const reply = await service.fedView(req, ROUTE, headers, SCOREBOARD_OWNED_VIEW);
  if (typeof reply === 'string') {
    // The service moved past this front door's facts: fetch again on the next request.
    if (reply === 'stale') state.stale = true;
    return false;
  }
  if (!reply) return false;
  sendRustView(res, reply);
  return true;
}
