/**
 * R2 (Status, fed): the front door's half of GET /api/status from fed generations (status/status-feed.ts). A
 * stand-in service keeps the generations as the real one does (only a newer numeric generation replaces one; a
 * request naming another is `stale`) and answers fixed bytes, so the pushes, the per-request ids, the facts and
 * every fallback are observed exactly. The real service runs the same protocol in
 * crates/aac-http/tests/lifecycle.rs and, end to end with the real binary, tests/spec/status-read-parity.mjs.
 */
import { afterAll, beforeAll, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import fs from 'fs';
import os from 'os';
import path from 'path';
import type { Server } from 'http';
import type { Request } from 'express';
import { createStatusRouter } from '../../../src/web-server/routes/status-routes';
import { RustService } from '../../../src/web-server/rust-service';
import { rustStatusInput } from '../../../src/web-server/rust-read-input';
import { AgentReportsReader } from '../../../src/web-server/status/agent-reports-reader';
import { STATUS_PARTS, sameTo, statusFedService } from '../../../src/web-server/status/status-feed';
import { buildStatusView } from '../../../src/web-server/status/status-view';
import { getCcsDir } from '../../../src/utils/config-manager';
import {
  NOW,
  cardDoc,
  item,
  tasksDoc,
  threadDoc,
  writeCard,
  writeReport,
} from './status/status-test-helpers';

const RUST_BYTES = '{"version":1,"from":"rust"}';
const ETAG = `W/"${'a'.repeat(64)}"`;
const SOURCES = STATUS_PARTS.map((part) => part.source);

/** The real service's generation rules, in memory; `fedView` answers fixed bytes or the decline it is told to. */
class StandIn extends RustService {
  boot = 'b'.repeat(24);
  held = new Map<string, { generation: string; data: string; facts: string }>();
  puts: { source: string; generation: string; data: string; facts: string }[] = [];
  views: Record<string, string>[] = [];
  decline: string | null = null;
  failPush = false;

  constructor() {
    super('on');
  }
  override feedBoot(scope: string): string | null {
    return scope === SCOPE ? this.boot : null;
  }
  override async pushFeed(
    boot: string,
    source: string,
    generation: string,
    data: string,
    facts: string
  ) {
    if (boot !== this.boot || this.failPush) return null;
    this.puts.push({ source, generation, data, facts });
    const current = this.held.get(source);
    if (!current || Number(generation) > Number(current.generation))
      this.held.set(source, { generation, data, facts });
    return this.held.get(source)?.generation ?? null;
  }
  override async fedView(
    _req: Request,
    route: string,
    headers: Record<string, string>,
    view: string
  ) {
    expect(route).toBe('/api/status');
    expect(view).toBe('status@1');
    this.views.push(headers);
    if (this.decline) {
      this.fedDeclined(route, this.decline);
      return this.decline;
    }
    const feed = JSON.parse(headers['x-aac-feed']) as Record<string, string>;
    const named = SOURCES.map((source) => [source, feed[source.slice('status-'.length)]]);
    if (named.some(([source, generation]) => this.held.get(source)?.generation !== generation))
      return 'stale';
    return { bytes: Buffer.from(RUST_BYTES), etag: ETAG, view };
  }
}

let home: string;
/** The CCS directory the stand-in service reads (the front door's `getCcsDir()` at start). */
let SCOPE = '';
let service: StandIn;
let server: Server;
let url: string;
let input: any;
let reads = 0;
const saved = {
  auth: process.env.CCS_DASHBOARD_AUTH_ENABLED,
  group: process.env.AAC_RUST_GROUP_STATUS,
  dir: process.env.CCS_DIR,
};

async function fixture(): Promise<any> {
  const share = path.join(home, `share-${Math.random().toString(36).slice(2)}`);
  writeReport(share, 'vm.fixture', 'thread.json', threadDoc('vm.fixture'));
  writeReport(share, 'vm.fixture', 'tasks.json', tasksDoc('vm.fixture', [item('A', 'landed')]));
  writeCard(share, cardDoc('vm.fixture', 'question'));
  const reader = new AgentReportsReader({ root: () => share, now: () => NOW });
  await reader.scan();
  const none = () => ({ data: null, asOf: null, error: null });
  return {
    now: NOW,
    timeZone: 'America/New_York',
    reports: reader.snapshot(),
    registry: none(),
    machines: { vm: none(), mac: none(), windows: none(), nas1: none(), alexis: none(), odroid: none(), '1070ti': none() },
    browsers: none(),
    simlock: none(),
    usage: null,
    answers: [],
    sends: [],
    answerHistoryProblem: null,
    hiddenThreadNames: ['aac-doorbell'],
  };
}

async function get(headers: Record<string, string> = {}) {
  const response = await fetch(url, { headers });
  return { response, text: await response.text() };
}

/** What today's handler answers for the current input (the original handler with the service off). */
function today(): string {
  return JSON.stringify(buildStatusView(input));
}

beforeAll(async () => {
  home = fs.mkdtempSync(path.join(process.env.TMPDIR ?? os.tmpdir(), 'aac-status-feed-'));
  process.env.CCS_DIR = path.join(home, '.ccs');
  SCOPE = getCcsDir();
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
  process.env.AAC_RUST_GROUP_STATUS = 'on';
  const app = express();
  app.use((req, res, next) => {
    Object.assign(req, { session: { authenticated: true }, sessionID: 'fixture-session' });
    res.locals.rustService = service;
    next();
  });
  app.use(
    '/api/status',
    createStatusRouter(
      () =>
        ({
          reader: { root: home },
          readInputs: () => {
            reads++;
            return input;
          },
          view: () => buildStatusView(input),
          answers: {},
        }) as any
    )
  );
  await new Promise<void>((resolve) => {
    server = app.listen(0, '127.0.0.1', () => resolve());
  });
  url = `http://127.0.0.1:${(server.address() as { port: number }).port}/api/status`;
});

afterAll(async () => {
  await new Promise<void>((resolve) => server.close(() => resolve()));
  fs.rmSync(home, { recursive: true, force: true });
  for (const [key, name] of [
    ['auth', 'CCS_DASHBOARD_AUTH_ENABLED'],
    ['group', 'AAC_RUST_GROUP_STATUS'],
    ['dir', 'CCS_DIR'],
  ] as const) {
    if (saved[key] === undefined) delete process.env[name];
    else process.env[name] = saved[key];
  }
});

beforeEach(async () => {
  service = new StandIn();
  input = await fixture();
  reads = 0;
});

const pushed = () => service.puts.map((p) => p.source);

describe('Status, fed', () => {
  it('pushes the six sources once, then serves the Rust bytes with the revalidated-view headers', async () => {
    const first = await get();
    expect(first.response.status).toBe(200);
    expect(first.text).toBe(RUST_BYTES);
    expect(first.response.headers.get('etag')).toBe(ETAG);
    expect(first.response.headers.get('cache-control')).toBe('private, no-cache');
    expect(first.response.headers.get('vary')).toContain('Cookie');
    expect(first.response.headers.get('content-type')).toBe('application/json; charset=utf-8');
    expect(pushed()).toEqual(SOURCES);
    // Unchanged inputs: no push, only the clock and the ids cross.
    input = { ...input, now: NOW + 1500 };
    const second = await get();
    expect(second.text).toBe(RUST_BYTES);
    expect(pushed()).toEqual(SOURCES);
    const feed = JSON.parse(service.views[1]['x-aac-feed']);
    expect(Object.keys(feed)).toEqual([
      'now',
      'clock',
      'day',
      'reports',
      'quota',
      'pinned',
      'collected',
      'answers',
      'facts',
    ]);
    expect(feed.now).toBe(NOW + 1500);
    // now's words in the facts' zone, as the envelope writes them.
    const envelope = rustStatusInput(input) as any;
    expect(feed.clock).toBe(envelope.presentation.dates[String(NOW + 1500)].clock);
    expect(feed.day).toBe(envelope.presentation.dates[String(NOW + 1500)].day);
    expect(reads).toBe(2);
    // 304 for a matching validator.
    const revalidated = await get({ 'If-None-Match': ETAG });
    expect(revalidated.response.status).toBe(304);
  });

  it('pushes data and facts that make exactly the envelope of the whole input, less the request clock', async () => {
    input.registry = {
      data: [
        {
          name: 'mail-worker',
          kind: 'codex',
          machine: 'vm',
          about: 'Fixture task',
          lastSeen: new Date(NOW - 60_000).toISOString(),
          readAt: NOW - 30_000,
          problem: null,
        },
      ],
      asOf: NOW,
      error: null,
    };
    await get();
    const envelope = rustStatusInput(input) as any;
    // The envelope without the request's clock: its facts are exactly the sources' (a source may hold the same time).
    const sources = rustStatusInput({ ...input, now: undefined }) as any;
    const data: Record<string, unknown> = { now: input.now };
    let facts: any = null;
    for (const put of service.puts) {
      Object.assign(data, JSON.parse(put.data));
      if (put.source === 'status-facts') facts = JSON.parse(put.facts);
      else expect(put.facts).toBe('{}');
    }
    // The same members (key order aside: Rust reads by key) and the same values.
    expect(Object.keys(data).sort()).toEqual(Object.keys(envelope.data).sort());
    for (const key of Object.keys(envelope.data))
      expect(JSON.stringify(data[key])).toBe(JSON.stringify(envelope.data[key]));
    expect(facts.dates).toEqual(sources.presentation.dates);
    expect(facts.epochs).toEqual(envelope.presentation.epochs);
    expect(facts.order).toEqual(envelope.presentation.order);
    // With now's date added (it crosses per request), the dates are the envelope's.
    expect({
      ...facts.dates,
      [String(input.now)]: envelope.presentation.dates[String(input.now)],
    }).toEqual(envelope.presentation.dates);
  });

  it('pushes only the source whose member changed, and the facts with it', async () => {
    await get();
    service.puts = [];
    // A collected panel is replaced (as StatusService does on every collection).
    input = { ...input, browsers: { data: [], asOf: NOW, error: null } };
    await get();
    expect(pushed()).toEqual(['status-collected', 'status-facts']);
    // A new reports snapshot with the same rows is not a change (the reader's rows are immutable).
    service.puts = [];
    input = { ...input, reports: { ...input.reports, threads: [...input.reports.threads] } };
    await get();
    expect(pushed()).toEqual([]);
    // A replaced row is.
    input = {
      ...input,
      reports: {
        ...input.reports,
        threads: input.reports.threads.map((row: any) => ({ ...row, sha256: 'f'.repeat(64) })),
      },
    };
    await get();
    expect(pushed()).toEqual(['status-reports', 'status-facts']);
    // Answers change in place (delivery steps): their JSON text is the key.
    service.puts = [];
    input.sends = [
      { sendId: 's1', summary: { state: 'pending', at: null, detail: null, attempts: 0 } },
    ];
    await get();
    expect(pushed()).toEqual(['status-answers', 'status-facts']);
    service.puts = [];
    input.sends[0].summary.state = 'done';
    await get();
    expect(pushed()).toEqual(['status-answers', 'status-facts']);
    // The time zone moves every word: the facts.
    service.puts = [];
    input = { ...input, timeZone: 'UTC' };
    await get();
    expect(pushed()).toEqual(['status-facts']);
  });

  it("answers through today's handler, from the same read, whenever the fed path declines", async () => {
    // Rust's own decline.
    service.decline = 'date';
    let reply = await get();
    expect(reply.text).toBe(today());
    expect(reply.response.headers.get('cache-control')).toBe('private, no-cache');
    expect(reads).toBe(1);
    service.decline = null;
    // A member this code does not know.
    input = { ...input, extra: 1 };
    reply = await get();
    expect(reply.text).toBe(today());
    delete input.extra;
    // A value JSON cannot carry exactly.
    input = { ...input, answerHistoryProblem: undefined, sends: [{ sendId: 'x', n: Number.NaN }] };
    const views = service.views.length;
    reply = await get();
    expect(service.views.length).toBe(views);
    expect(reply.text).toBe(today());
    input.sends = [];
    // A push that did not land.
    service.failPush = true;
    input = { ...input, browsers: { data: [], asOf: NOW, error: null } };
    reply = await get();
    expect(reply.text).toBe(today());
    service.failPush = false;
    expect((await get()).text).toBe(RUST_BYTES);
  });

  it("a stale generation is today's answer, and the next request pushes again", async () => {
    await get();
    // The service lost what this front door recorded (a push raced, or it restarted with the same boot).
    service.held.clear();
    service.puts = [];
    const stale = await get();
    expect(stale.text).toBe(today());
    expect(pushed()).toEqual([]);
    const next = await get();
    expect(next.text).toBe(RUST_BYTES);
    expect(pushed()).toEqual(SOURCES);
  });

  it('a new boot holds nothing: the front door pushes everything again', async () => {
    await get();
    service.boot = 'c'.repeat(24);
    service.held.clear();
    service.puts = [];
    expect((await get()).text).toBe(RUST_BYTES);
    expect(pushed()).toEqual(SOURCES);
  });

  it("another root or the group off is today's path", async () => {
    const original = process.env.CCS_DIR;
    process.env.CCS_DIR = path.join(home, 'other');
    expect((await get()).text).toBe(today());
    process.env.CCS_DIR = original;
    expect(service.views.length).toBe(0);
    // The group is read once per service: a service made with it off never offers the fed path.
    process.env.AAC_RUST_GROUP_STATUS = 'off';
    service = new StandIn();
    expect(statusFedService({ locals: { rustService: service } } as any)).toBeNull();
    expect((await get()).text).toBe(today());
    expect(service.views.length).toBe(0);
    process.env.AAC_RUST_GROUP_STATUS = 'on';
  });
});

describe('sameTo', () => {
  it('compares what readInputs builds fresh, to its depth, by identity below', () => {
    const row = { a: 1 };
    expect(sameTo({ rows: [row], n: 1 }, { rows: [row], n: 1 }, 2)).toBe(true);
    expect(sameTo({ rows: [row] }, { rows: [{ a: 1 }] }, 2)).toBe(false);
    expect(sameTo({ rows: [row] }, { rows: [{ a: 1 }] }, 3)).toBe(true);
    expect(sameTo({ a: 1, b: 2 }, { b: 2, a: 1 }, 1)).toBe(false);
    expect(sameTo(new Map([['x', 1]]), new Map([['x', 1]]), 1)).toBe(true);
    expect(sameTo(new Map([['x', 1]]), new Map([['y', 1]]), 1)).toBe(false);
    expect(sameTo([1, 2], [1, 2], 0)).toBe(false);
    expect(sameTo(Number.NaN, Number.NaN, 3)).toBe(false);
  });
});
