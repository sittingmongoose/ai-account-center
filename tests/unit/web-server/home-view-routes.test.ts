/**
 * D7: GET /api/views/home (the one read Home makes when the server builds its view) on the shared M3 pieces: the
 * presentation envelope (`homeReadInput` over `rustReadInput`) and the `compose()` bridge. Synthetic data; a fake
 * private socket stands in for the Rust service, so the production envelope, transport, caps and fallback run as
 * they are. The real service runs the same route in crates/aac-http/tests/socket.rs and the case table in
 * crates/aac-views; the stored table's envelopes are checked here against the front door's own.
 */
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import type { Server } from 'http';
import type { Request, Response } from 'express';
import { createHomeViewRouter } from '../../../src/web-server/routes/home-view-routes';
import { createAccountDashboardRouter } from '../../../src/web-server/routes/account-dashboard-routes';
import { RustService } from '../../../src/web-server/rust-service';
import {
  openPairs,
  readAntigravityInventory,
  readClaudeDesktopProfiles,
  runtimeInventory,
} from '../../../src/web-server/routes/home-view-routes';

import { getCcsDir } from '../../../src/utils/config-manager';
import {
  homeReadInput,
  isHomeView,
  parseHomeContext,
  type HomeContext,
} from '../../../src/web-server/views/home-context';
import type { AccountDashboard } from '../../../src/web-server/services/account-dashboard-types';

const NOW = Date.parse('2026-11-14T12:00:00.000Z');
const PROBE = [
  '-1,234.57',
  '1.23M',
  '-$1,234.50',
  'Jan 5, 12:07 AM',
  '1:07 PM',
  'Mon, Jan 5, 1:07 PM',
  'Jan 5',
  '_abB',
];
const DAY = 86_400_000;
const FROM = Math.floor(NOW / DAY) * DAY - 120 * DAY;
// The page's New York offsets (view-model.mjs homeZoneFacts): EST from Nov 1, 2026, EDT from Mar 14, 2027 ...
const ZONE = {
  from: FROM,
  to: FROM + 520 * DAY,
  offsets: [
    [FROM, -240],
    [1793512800000, -300],
    [1805007600000, -240],
  ],
};
const CONTEXT = {
  now: NOW,
  timeZone: 'America/New_York',
  platform: 'mac',
  intervalSeconds: 60,
  username: 'owner',
  host: '192.0.2.10:3000',
  formatProbe: PROBE,
  zone: ZONE,
};
const VIEW = {
  version: 3,
  sections: [],
  cards: [],
  registry: [],
  chrome: { statusLead: 'Updated' },
};
const header = (value: unknown) => encodeURIComponent(JSON.stringify(value));

const dashboard: AccountDashboard = {
  schemaVersion: 1,
  updatedAt: '2026-11-14T11:59:00.000Z',
  accounts: [],
  server: { version: '0.0.0-test', commit: null },
} as unknown as AccountDashboard;

type Envelope = {
  data: Record<string, unknown>;
  presentation: {
    dates: Record<string, Record<string, string>>;
    epochs: Record<string, number>;
    order: Record<string, number>;
  };
};

let server: Server;
let baseUrl: string;
let built: Envelope[];
let answer: unknown;
let failDashboard = false;
let failProfiles = false;
let offered = true;
const saved = {
  auth: process.env.CCS_DASHBOARD_AUTH_ENABLED,
  home: process.env.AAC_RUST_HOME_VIEW,
  group: process.env.AAC_RUST_GROUP_HOME,
};

/** A service in on mode whose compose() records the envelope and answers `answer` the way the bridge does. */
function offeringService(): RustService {
  const service = new RustService('on');
  Object.assign(service, {
    compose: async (
      _req: Request,
      route: string,
      input: unknown,
      legacy: () => unknown,
      accepts: (body: unknown) => boolean
    ) => {
      expect(route).toBe('/api/views/home');
      built.push((typeof input === 'function' ? (input as () => unknown)() : input) as Envelope);
      return accepts(answer) ? answer : legacy();
    },
  });
  return service;
}

beforeAll(() => {
  // requireDashboardSession checks the session only when sign-in is on (no config file is read).
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
  process.env.AAC_RUST_HOME_VIEW = 'on';
  // This file proves D7's envelope wiring with stubbed reads; the fed group (default-ready, on when unset)
  // would answer from the real dashboard read first. home-feed.test.ts proves the fed path.
  process.env.AAC_RUST_GROUP_HOME = 'off';
});
afterAll(() => {
  for (const [key, value] of [
    ['CCS_DASHBOARD_AUTH_ENABLED', saved.auth],
    ['AAC_RUST_HOME_VIEW', saved.home],
    ['AAC_RUST_GROUP_HOME', saved.group],
  ] as const)
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
});

beforeEach(async () => {
  built = [];
  answer = VIEW;
  failDashboard = false;
  failProfiles = false;
  offered = true;
  const app = express();
  app.use((req, res, next) => {
    if (req.headers['x-test-session'] === 'true')
      Object.assign(req, { session: { authenticated: true }, sessionID: 'fixture-session' });
    if (offered) res.locals.rustService = offeringService();
    next();
  });
  app.use(
    '/api/views',
    createHomeViewRouter({
      getDashboard: async () => {
        if (failDashboard) throw new Error('raw upstream sentinel');
        return structuredClone(dashboard);
      },
      readProfiles: async () => {
        if (failProfiles) throw new Error('profiles sentinel');
        return [{ id: 'a', name: 'A', openOperation: null }];
      },
      readInventory: async () => ({ schemaVersion: 1, hostId: 'ubuntu', profiles: [] }),
    })
  );
  app.use(
    '/api/accounts',
    createAccountDashboardRouter({ getDashboard: async () => structuredClone(dashboard) })
  );
  server = await new Promise<Server>((resolve) => {
    const instance = app.listen(0, '127.0.0.1', () => resolve(instance));
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('No test port');
  baseUrl = `http://127.0.0.1:${address.port}`;
});

afterEach(async () => {
  await new Promise<void>((resolve) => server.close(() => resolve()));
});

function home(query = '', { session = true, context = header(CONTEXT) as string | null } = {}) {
  const headers: Record<string, string> = {};
  if (session) headers['x-test-session'] = 'true';
  if (context !== null) headers['x-aac-home-context'] = context;
  return fetch(`${baseUrl}/api/views/home${query}`, { headers });
}

describe('GET /api/views/home', () => {
  it('is not offered with the switch off: the request goes on to the next handler (404 here)', async () => {
    offered = false;
    expect((await home()).status).toBe(404);
    const plain = await (
      await fetch(`${baseUrl}/api/accounts/dashboard`, { headers: { 'x-test-session': 'true' } })
    ).json();
    expect(plain.server).toEqual({ version: '0.0.0-test', commit: null });
  });

  it('is not offered with the service on but both Home switches off', async () => {
    process.env.AAC_RUST_HOME_VIEW = 'off';
    try {
      expect((await home()).status).toBe(404);
    } finally {
      process.env.AAC_RUST_HOME_VIEW = 'on';
    }
  });

  it('default-ready: the group unset offers Home with the service on even with AAC_RUST_HOME_VIEW off', async () => {
    process.env.AAC_RUST_HOME_VIEW = 'off';
    delete process.env.AAC_RUST_GROUP_HOME;
    try {
      // The dashboard answer names the offer; no fed read runs on this route.
      const plain = await (
        await fetch(`${baseUrl}/api/accounts/dashboard`, { headers: { 'x-test-session': 'true' } })
      ).json();
      expect(plain.server.views).toEqual(['home']);
    } finally {
      process.env.AAC_RUST_HOME_VIEW = 'on';
      process.env.AAC_RUST_GROUP_HOME = 'off';
    }
  });

  it('offered: the dashboard answer names it, and the one read carries the data and the Rust view', async () => {
    const plain = await (
      await fetch(`${baseUrl}/api/accounts/dashboard`, { headers: { 'x-test-session': 'true' } })
    ).json();
    expect(plain.server.views).toEqual(['home']);
    const response = await home('?platform=windows&refresh=true', {
      context: header({ ...CONTEXT, platform: 'windows' }),
    });
    expect(response.status).toBe(200);
    expect(response.headers.get('cache-control')).toBe('no-store');
    const body = await response.json();
    expect(Object.keys(body)).toEqual([
      'schemaVersion',
      'dashboard',
      'profiles',
      'antigravity',
      'view',
    ]);
    expect(body.dashboard.server.views).toEqual(['home']);
    expect(body.profiles).toEqual([{ id: 'a', name: 'A', openOperation: null }]);
    expect(body.view).toEqual(VIEW);
    expect(built).toHaveLength(1);
    // the shared M3 envelope: the page's context and the three answers as data, this engine's facts beside them
    const sent = built[0];
    expect(Object.keys(sent)).toEqual(['data', 'presentation']);
    expect(Object.keys(sent.data)).toEqual(['ctx', 'dashboard', 'profiles', 'antigravity']);
    expect(sent.data.ctx).toEqual({ ...CONTEXT, platform: 'windows' } as never);
    expect(sent.presentation.dates[String(NOW)]).toEqual({
      iso: '2026-11-14T12:00:00.000Z',
      clock: '7:00 AM',
      day: 'Nov 14',
    });
    expect(sent.presentation.epochs['2026-11-14T11:59:00.000Z']).toBe(
      Date.parse('2026-11-14T11:59:00.000Z')
    );
  });

  it('a context the server cannot read: the data comes back and the page builds its own view', async () => {
    for (const context of [
      null,
      'not json',
      header({ ...CONTEXT, extra: 1 }),
      header({ ...CONTEXT, platform: 'windows' }),
      header({ ...CONTEXT, timeZone: 'Mars/Olympus' }),
      header({ ...CONTEXT, intervalSeconds: 20 }),
      header({ ...CONTEXT, formatProbe: PROBE.slice(1) }),
      header({ ...CONTEXT, username: 'x'.repeat(257) }),
      header({ ...CONTEXT, now: 1.5 }),
      // the zone facts' shape (their meaning is the Rust builder's to check)
      header({ ...CONTEXT, zone: undefined }),
      header({ ...CONTEXT, zone: { ...ZONE, extra: 1 } }),
      header({ ...CONTEXT, zone: { ...ZONE, offsets: [] } }),
      header({ ...CONTEXT, zone: { ...ZONE, offsets: [[FROM, 1.5]] } }),
      header({ ...CONTEXT, zone: { ...ZONE, offsets: [[FROM]] } }),
      header({ ...CONTEXT, zone: { ...ZONE, offsets: Array(65).fill([FROM, 0]) } }),
      header({ ...CONTEXT, zone: { ...ZONE, to: '1' } }),
    ]) {
      const response = await home('', { context });
      expect(response.status).toBe(200);
      const body = await response.json();
      expect(body.view).toBeNull();
      expect(body.dashboard.schemaVersion).toBe(1);
    }
    expect(built).toEqual([]);
  });

  it('without the profiles the page keeps its own (Rust does not know them): no view', async () => {
    failProfiles = true;
    const body = await (await home()).json();
    expect(body.profiles).toBeNull();
    expect(body.view).toBeNull();
    expect(built).toEqual([]);
  });

  it('a refused, failed or malformed Rust answer is the fallback: view null, the data is still the answer', async () => {
    for (const reply of [null, { ...VIEW, version: 2 }, { ...VIEW, registry: null }, []]) {
      answer = reply;
      const body = await (await home()).json();
      expect(body.view).toBeNull();
      expect(body.dashboard.schemaVersion).toBe(1);
    }
  });

  it("keeps the dashboard route's refusals: session, query and a failed read", async () => {
    expect((await home('', { session: false })).status).toBe(401);
    for (const query of ['?platform=linux', '?refresh=yes', '?platform=mac&extra=1'])
      expect((await home(query)).status).toBe(400);
    failDashboard = true;
    const failed = await home();
    expect(failed.status).toBe(500);
    expect(await failed.text()).not.toContain('sentinel');
    expect(built).toEqual([]);
  });
});

describe('home context and envelope', () => {
  it('parses only the exact context the page sends', () => {
    expect(parseHomeContext(header(CONTEXT), 'mac')).toEqual(CONTEXT as never);
    expect(parseHomeContext(header(CONTEXT), 'windows')).toBeNull();
    expect(parseHomeContext('%E0%A4%A', 'mac')).toBeNull();
    expect(parseHomeContext(header([CONTEXT]), 'mac')).toBeNull();
    expect(parseHomeContext('x'.repeat(4097), 'mac')).toBeNull();
  });

  it('carries the display-zone words of every instant and ranks only the words Home sorts by', () => {
    const data = {
      ...dashboard,
      accounts: [{ id: 'cursor:b' }, { id: 'cursor:A' }, { id: 7 }],
      claudeSignIns: { signIns: [{ profile: 'work' }, { profile: 2 }, {}] },
      note: 'not ranked',
    };
    const envelope = homeReadInput(CONTEXT as HomeContext, data, [], null) as Envelope;
    // 2026-03-08T07:30Z is 3:30 AM EDT, just after the change
    const change = homeReadInput(
      CONTEXT as HomeContext,
      { at: '2026-03-08T07:30:00.000Z' },
      [],
      null
    ) as Envelope;
    expect(change.presentation.dates[String(Date.parse('2026-03-08T07:30:00.000Z'))]).toEqual({
      iso: '2026-03-08T07:30:00.000Z',
      clock: '3:30 AM',
      day: 'Mar 8',
    });
    expect(Object.keys(envelope.presentation.order).sort()).toEqual(
      ['', '2', 'cursor:A', 'cursor:b', 'undefined', 'work'].sort()
    );
    expect(envelope.presentation.order['cursor:A']).toBeLessThan(
      envelope.presentation.order['cursor:b']
    );
    expect(envelope.presentation.order['not ranked']).toBeUndefined();
  });

  it('isHomeView admits only a version 3 view', () => {
    expect(isHomeView(VIEW)).toBe(true);
    for (const value of [
      null,
      [],
      { ...VIEW, version: 2 },
      { ...VIEW, cards: {} },
      { ...VIEW, chrome: [] },
    ])
      expect(isHomeView(value)).toBe(false);
  });

  it('the stored spec envelopes are what this front door computes (tests/spec/views/home/cases.json)', () => {
    const table = JSON.parse(
      fs.readFileSync(path.join(import.meta.dir, '../../spec/views/home/cases.json'), 'utf8')
    ) as {
      cases: Array<{
        name: string;
        ctx: HomeContext;
        dashboard: unknown;
        profiles: unknown;
        antigravity: unknown;
        presentation: unknown;
      }>;
    };
    expect(table.cases.length).toBeGreaterThan(150);
    for (const c of table.cases) {
      const { presentation } = homeReadInput(
        c.ctx,
        c.dashboard,
        c.profiles,
        c.antigravity
      ) as Envelope;
      const want = c.name === 'decline/facts' ? { ...presentation, dates: {} } : presentation;
      expect(JSON.stringify(c.presentation)).toBe(JSON.stringify(want));
    }
  });
});

describe('compose() for /api/views/home over a private socket', () => {
  const dir = fs.mkdtempSync(path.join(process.env.TMPDIR || os.tmpdir(), 'home-view-'));
  const socketPath = path.join(dir, 'home.sock');
  const secret = 'e'.repeat(64);
  const bootId = 'd'.repeat(24);
  let reply: { status: number; body: string } = { status: 200, body: JSON.stringify(VIEW) };
  /** The native owner's answer to GET /api/antigravity/profiles with no TS input (the raw read). */
  let inventoryReply: { status: number; body: string } | null = null;
  let seen: {
    method?: string;
    url?: string;
    principal?: unknown;
    cookie?: unknown;
    body: string;
  }[] = [];
  const fake = http.createServer((req, res) => {
    let body = '';
    req.on('data', (chunk) => (body += chunk));
    req.on('end', () => {
      const principal = JSON.parse(String(req.headers['x-aac-principal']));
      seen.push({ method: req.method, url: req.url, principal, cookie: req.headers.cookie, body });
      res.setHeader('x-aac-service-boot', bootId);
      // The owner's own read: an empty body is the native inventory (`profile_reads`).
      const answer =
        inventoryReply && req.url === '/api/antigravity/profiles' && body === ''
          ? inventoryReply
          : reply;
      // as the real service: Home is for a verified dashboard session only
      res.statusCode = principal.kind === 'session' ? answer.status : 401;
      res.end(principal.kind === 'session' ? answer.body : '{"error":"Authentication required"}');
    });
  });
  const req = {
    method: 'GET',
    sessionID: 'fixture-session',
    session: { authenticated: true },
    headers: { cookie: 'must-not-forward' },
  } as unknown as Request;
  const envelope = homeReadInput(CONTEXT as HomeContext, dashboard, [], null);
  const compose = (s: RustService, request = req, input: unknown = () => envelope) =>
    s.compose(request, '/api/views/home', input, () => null, isHomeView);
  function service(mode = 'on') {
    const s = new RustService(mode);
    Object.assign(s, { lease: { connection: { socketPath, secret, bootId }, close: () => {} } });
    return s;
  }

  beforeAll(async () => {
    await new Promise<void>((resolve, reject) => {
      fake.once('error', reject);
      fake.listen(socketPath, resolve);
    });
  });
  afterAll(async () => {
    await new Promise<void>((resolve) => fake.close(() => resolve()));
    fs.rmSync(dir, { recursive: true, force: true });
  });
  beforeEach(() => {
    seen = [];
    reply = { status: 200, body: JSON.stringify(VIEW) };
    inventoryReply = null;
  });

  it('sends the envelope once as the read body, with a hashed session principal, and serves a valid view', async () => {
    const s = service();
    expect(await compose(s)).toEqual(VIEW);
    expect(seen).toHaveLength(1);
    expect(seen[0].method).toBe('GET');
    expect(seen[0].url).toBe('/api/views/home');
    expect(seen[0].cookie).toBeUndefined();
    expect((seen[0].principal as { kind: string }).kind).toBe('session');
    expect((seen[0].principal as { id: string }).id).not.toBe('fixture-session');
    expect(JSON.parse(seen[0].body)).toEqual(JSON.parse(JSON.stringify(envelope)));
    expect(s.stats.served).toBe(1);
  });

  it('refusals, faults and wrong shapes take the logged fallback: no view', async () => {
    const s = service();
    for (const [status, body] of [
      [503, JSON.stringify({ error: 'TypeScript read fallback required' })],
      [200, JSON.stringify({ ...VIEW, version: 2 })],
      [200, JSON.stringify({ ...VIEW, registry: null })],
      [200, '[]'],
      [200, '{'],
      [500, '{}'],
      [200, ' '.repeat(2 * 1024 * 1024 + 1)],
    ] as const) {
      reply = { status, body };
      expect(await compose(s)).toBeNull();
    }
    expect(s.stats.served).toBe(0);
    expect(s.stats.fallbacks).toBe(7);
  });

  it('a device gets no view; the socket is never called off or for an input over the 4 MiB cap', async () => {
    expect(await compose(service('off'))).toBeNull();
    expect(seen).toEqual([]);
    const device = {
      method: 'GET',
      headers: {},
      auth: { kind: 'device', deviceId: 'dev_fixture' },
    } as unknown as Request;
    expect(await compose(service(), device)).toBeNull();
    // L1: TS gates from the route table's roles too, so a device never reaches the socket here.
    expect(seen).toEqual([]);
    const big = service();
    expect(
      await compose(big, req, { data: { pad: 'x'.repeat(4 * 1024 * 1024) }, presentation: {} })
    ).toBeNull();
    expect(big.stats.fallbacks).toBe(1);
    expect(seen).toEqual([]);
  });
  it("Home's profiles come from part 4's capped Rust manifest read, with each Open added by TS", async () => {
    const s = service();
    const res = { locals: { rustService: s } } as unknown as Response;
    // the service reads its own boot root only: another root is the TS reader (no manifest in this test home)
    expect(await readClaudeDesktopProfiles(req, res)).toEqual([]);
    expect(seen).toEqual([]);
    Object.assign((s as unknown as { lease: { connection: object } }).lease.connection, {
      ccsDir: getCcsDir(),
    });
    reply = {
      status: 200,
      body: JSON.stringify({
        profiles: [{ id: 'a', email: 'a@example.test' }, { email: 'b@example.test' }],
      }),
    };
    expect(await readClaudeDesktopProfiles(req, res)).toEqual([
      // No Rust Open owner is bound here, so no operation can exist and each id-bearing row says so.
      { id: 'a', email: 'a@example.test', openOperation: null },
      { email: 'b@example.test' },
    ]);
    // A row without the field makes the Open state unknown; a row carrying null means "none running".
    expect(openPairs([{ id: 'a' }])).toBeNull();
    expect(openPairs([{ id: 'a', openOperation: null }])).toEqual([]);
    expect(openPairs([{ id: 'a', openOperation: { id: 'op_1' } }])).toEqual([
      ['a', { id: 'op_1' }],
    ]);
    expect(seen.map((r) => [r.url, r.body])).toEqual([['/api/claude/desktop-profiles', '']]);
    expect(s.stats.served).toBe(1);
    // a reply that is not the list is the TS reader of the same manifest
    reply = { status: 200, body: JSON.stringify({ profiles: {} }) };
    expect(await readClaudeDesktopProfiles(req, res)).toEqual([]);
    expect(s.stats.served).toBe(1);
  });

  it("Home's Antigravity copy is composed as GET /api/antigravity/profiles composes it", async () => {
    const s = service();
    const res = { locals: { rustService: s } } as unknown as Response;
    // The service reads its own boot root only; this test home is the worker's.
    Object.assign((s as unknown as { lease: { connection: object } }).lease.connection, {
      ccsDir: getCcsDir(),
    });
    // The native owner's read: one saved profile, as profile_reads answers with no TS input.
    const raw = {
      schemaVersion: 1,
      hostId: 'ubuntu',
      profiles: [
        {
          id: 'gmail',
          email: 'gmail@example.test',
          plan: 'Google AI Pro',
          selected: true,
          available: true,
          runtimeVerified: true,
          identityVerified: true,
          credentialRevision: 'f'.repeat(64),
          savedAt: '2026-10-01T16:40:00.000Z',
        },
      ],
    };
    inventoryReply = { status: 200, body: JSON.stringify(raw) };
    // The composed copy the same route answers for that read.
    reply = { status: 200, body: JSON.stringify(raw) };
    expect(await runtimeInventory(req, res)).toEqual(raw);
    expect(seen.at(-1)?.url).toBe('/api/antigravity/profiles');
    expect(seen.at(-1)?.body).toBe('');
    expect(await readAntigravityInventory(req, res)).toEqual(raw);
    expect(seen.filter((r) => r.url === '/api/antigravity/profiles')).toHaveLength(3);
    // An owner that cannot answer is unavailable (null), never an empty inventory.
    inventoryReply = {
      status: 503,
      body: JSON.stringify({ error: 'TypeScript read fallback required' }),
    };
    expect(await readAntigravityInventory(req, res)).toBeNull();
  });

  it('an active Open on the profiles route reaches Home, and an unreadable owner stays unavailable', async () => {
    const s = service();
    Object.assign((s as unknown as { lease: { connection: object } }).lease.connection, {
      ccsDir: getCcsDir(),
    });
    const operation = {
      id: 'op_1',
      platform: 'mac',
      state: 'opening',
      confirmedCount: 0,
      totalCount: 2,
      message: null,
    };
    // The public profiles route serves each profile's live Open progress (Profiles::open).
    reply = {
      status: 200,
      body: JSON.stringify({
        profiles: [{ id: 'p1', email: 'one@example.test', openOperation: operation }],
      }),
    };
    const res = { locals: { rustService: s } } as unknown as Response;
    expect(await readClaudeDesktopProfiles(req, res)).toEqual([
      { id: 'p1', email: 'one@example.test', openOperation: operation },
    ]);
    // Retained Home answers with the same rows (production profiles read, the group off so no fed answer).
    const instance = express();
    instance.use((rq, rs, next) => {
      Object.assign(rq, { session: { authenticated: true }, sessionID: 'fixture-session' });
      rs.locals.rustService = s;
      next();
    });
    instance.use(
      '/api/views',
      createHomeViewRouter({
        getDashboard: async () => structuredClone(dashboard),
        scope: () => getCcsDir(),
      })
    );
    const own = await new Promise<Server>((resolve) => {
      const started = instance.listen(0, '127.0.0.1', () => resolve(started));
    });
    const address = own.address();
    if (!address || typeof address === 'string') throw new Error('No test port');
    const url = `http://127.0.0.1:${address.port}/api/views/home?platform=mac`;
    try {
      const body = (await (
        await fetch(url, { headers: { 'x-aac-home-context': header(CONTEXT) } })
      ).json()) as { profiles?: { openOperation?: unknown }[] };
      expect(body.profiles?.[0].openOperation).toEqual(operation);
      // An owner that cannot answer leaves the Open state unknown: the TS manifest rows carry no
      // field, so the fed path declines instead of feeding an empty list (home-feed.test.ts).
      reply = { status: 503, body: JSON.stringify({ error: 'TypeScript read fallback required' }) };
      expect(await readClaudeDesktopProfiles(req, res)).toEqual([]);
    } finally {
      await new Promise<void>((resolve) => own.close(() => resolve()));
    }
  });

  it('Home and /api/antigravity/profiles agree on a non-empty saved registry (the shipping switch unset)', async () => {
    const s = service();
    Object.assign((s as unknown as { lease: { connection: object } }).lease.connection, {
      ccsDir: getCcsDir(),
    });
    // One saved profile, as the native owner reads the registry.
    const raw = {
      schemaVersion: 1,
      hostId: 'ubuntu',
      profiles: [
        {
          id: 'gmail',
          email: 'gmail@example.test',
          plan: 'Google AI Pro',
          selected: true,
          available: true,
          runtimeVerified: true,
          identityVerified: true,
          credentialRevision: 'f'.repeat(64),
          savedAt: '2026-10-01T16:40:00.000Z',
        },
      ],
    };
    inventoryReply = { status: 200, body: JSON.stringify(raw) };
    reply = { status: 200, body: JSON.stringify(raw) };
    // The retained Home path: production inventory deps (no injection), the group is off so the fed path declines.
    const instance = express();
    instance.use((rq, rs, next) => {
      Object.assign(rq, { session: { authenticated: true }, sessionID: 'fixture-session' });
      rs.locals.rustService = s;
      next();
    });
    instance.use(
      '/api/views',
      createHomeViewRouter({
        getDashboard: async () => structuredClone(dashboard),
        readProfiles: async () => [{ id: 'a', name: 'A', openOperation: null }],
        scope: () => getCcsDir(),
      })
    );
    instance.get('/api/antigravity/profiles', async (rq, rs) => {
      rs.json(await readAntigravityInventory(rq, rs));
    });
    const own = await new Promise<Server>((resolve) => {
      const started = instance.listen(0, '127.0.0.1', () => resolve(started));
    });
    const address = own.address();
    if (!address || typeof address === 'string') throw new Error('No test port');
    const homeUrl = `http://127.0.0.1:${address.port}/api/views/home?platform=mac`;
    const profilesUrl = `http://127.0.0.1:${address.port}/api/antigravity/profiles`;
    try {
      const homeReply = await (
        await fetch(homeUrl, { headers: { 'x-aac-home-context': header(CONTEXT) } })
      ).json();
      const profilesReply = await (await fetch(profilesUrl)).json();
      // The same saved profile reaches both answers.
      expect(homeReply.antigravity).toEqual(profilesReply);
      expect(homeReply.antigravity.profiles).toHaveLength(1);
      expect(homeReply.antigravity.profiles[0].id).toBe('gmail');
      // An unavailable read is unavailable on both, never an empty inventory.
      inventoryReply = {
        status: 503,
        body: JSON.stringify({ error: 'TypeScript read fallback required' }),
      };
      expect(
        (
          await (
            await fetch(homeUrl, { headers: { 'x-aac-home-context': header(CONTEXT) } })
          ).json()
        ).antigravity
      ).toBeNull();
      expect(await (await fetch(profilesUrl)).json()).toBeNull();
    } finally {
      await new Promise<void>((resolve) => own.close(() => resolve()));
    }
  });
});
