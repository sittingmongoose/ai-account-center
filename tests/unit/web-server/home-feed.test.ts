/**
 * R1 (Home, fed): the front door's half of GET /api/views/home from fed generations (views/home-feed.ts). A
 * stand-in service keeps the generations as the real one does (only a newer numeric generation replaces one; a
 * request naming another is `stale`) and answers fixed bytes, so the owner's pushes, the per-request ids and every
 * fallback are observed exactly. The real service runs the same protocol in crates/aac-http/tests/lifecycle.rs and,
 * end to end with the real binary, tests/spec/home-read-parity.mjs.
 */
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import type { Server } from 'http';
import type { Request, Response } from 'express';
import { homeFedService } from '../../../src/web-server/views/offered-views';
import { createHomeViewRouter } from '../../../src/web-server/routes/home-view-routes';
import { RustService } from '../../../src/web-server/rust-service';
import type { RustHomeInputs } from '../../../src/web-server/rust-service-transport';
import { withPassedResetsAt } from '../../../src/web-server/services/account-dashboard-service';
import type { AccountDashboard } from '../../../src/web-server/services/account-dashboard-types';
import { passFaithful, resetEpochs } from '../../../src/web-server/views/home-feed';
import {
  homeZoneAgrees,
  serverZoneFacts,
  HOME_ZONE_DAYS,
} from '../../../src/web-server/views/home-zone';
import type { HomeContext } from '../../../src/web-server/views/home-context';

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
const SCOPE = '/fixture/home/.ccs';
const CONTEXT = {
  now: NOW,
  timeZone: 'America/New_York',
  platform: 'mac',
  intervalSeconds: 60,
  username: 'owner',
  host: '192.0.2.10:3000',
  formatProbe: PROBE,
  zone: serverZoneFacts('America/New_York', NOW),
};
const header = (value: unknown) => encodeURIComponent(JSON.stringify(value));
const RUST_BYTES = '{"schemaVersion":1,"from":"rust"}';
const ETAG = `W/"${'a'.repeat(64)}"`;
const THROWS = Symbol('throws');

let dashboard: AccountDashboard;
let updatedAt: string;
let inventory: unknown;

function fixture(): AccountDashboard {
  return {
    schemaVersion: 1,
    updatedAt: '',
    settings: { refreshIntervalSeconds: 60 },
    accounts: [
      {
        id: 'claude:a',
        provider: 'claude',
        sampledAt: '2026-11-14T10:00:00.000Z',
        windows: [{ key: 'five_hour', resetAt: '2026-11-14T11:00:00.000Z', usedPercent: 40 }],
      },
    ],
    server: { version: '0.0.0-test', commit: null },
  } as unknown as AccountDashboard;
}

/** The real service's generation rules, in memory; `fedView` answers fixed bytes or the decline it is told to. */
class StandIn extends RustService {
  boot = 'b'.repeat(24);
  held = new Map<string, { generation: string; data: string; facts: string }>();
  puts: { source: string; generation: string; data: string; facts: string }[] = [];
  views: Record<string, string>[] = [];
  decline: string | null = null;
  pushGate: Promise<void> | null = null;
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
    if (this.pushGate) await this.pushGate;
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
    expect(route).toBe('/api/views/home');
    this.views.push(headers);
    if (this.decline) {
      this.fedDeclined(route, this.decline);
      return this.decline;
    }
    const feed = JSON.parse(headers['x-aac-feed']) as Record<string, string>;
    const named = [
      [`home-dashboard-${feed.platform}-${feed.secure ? 1 : 0}`, feed.dashboard],
      ['home-opens', feed.opens],
      ['home-antigravity', feed.antigravity],
    ];
    if (named.some(([source, generation]) => this.held.get(source)?.generation !== generation))
      return 'stale';
    return { bytes: Buffer.from(RUST_BYTES), etag: ETAG, view };
  }
  inputsCalls: RustHomeInputs[] = [];
  produceDecline: string | null = null;
  override async putHomeInputs(inputs: RustHomeInputs) {
    this.inputsCalls.push(inputs);
    if (this.produceDecline) return { declined: this.produceDecline };
    const ids = { dashboard: '900', opens: '901', antigravity: '902' };
    this.held.set(`home-dashboard-${inputs.platform}-${inputs.secure ? 1 : 0}`, {
      generation: ids.dashboard,
      data: '',
      facts: '',
    });
    this.held.set('home-opens', { generation: ids.opens, data: '', facts: '' });
    this.held.set('home-antigravity', { generation: ids.antigravity, data: '', facts: '' });
    return ids;
  }
}

let service: StandIn;
let fedServer: Server;
let plainServer: Server;
let fedUrl: string;
let plainUrl: string;
const saved = {
  auth: process.env.CCS_DASHBOARD_AUTH_ENABLED,
  home: process.env.AAC_RUST_HOME_VIEW,
  group: process.env.AAC_RUST_GROUP_HOME,
  producer: process.env.AAC_RUST_HOME_PRODUCER,
};

function app(fed: boolean) {
  const instance = express();
  instance.use((req, res, next) => {
    Object.assign(req, { session: { authenticated: true }, sessionID: 'fixture-session' });
    // The plain server is D7 today: offered, no fed group. Its service has no lease, so its own Rust reads fall back.
    res.locals.rustService = fed ? service : plain;
    next();
  });
  instance.use(
    '/api/views',
    createHomeViewRouter({
      readHomeDashboard: async () => ({
        dashboard: { ...structuredClone(dashboard), updatedAt },
        now: NOW,
      }),
      getDashboard: async () =>
        withPassedResetsAt({ ...structuredClone(dashboard), updatedAt }, NOW),
      readProfiles: async () => [{ id: 'p1', email: 'one@example.invalid', openOperation: null }],
      readInventory: async () => ({ schemaVersion: 1, hostId: 'ubuntu', profiles: [] }),
      runtimeInventory: async () => {
        if (inventory === THROWS) throw new Error('runtime sentinel');
        return inventory;
      },
      scope: () => SCOPE,
    })
  );
  return instance;
}
let plain: RustService;

async function listen(instance: express.Express): Promise<[Server, string]> {
  const server = await new Promise<Server>((resolve) => {
    const s = instance.listen(0, '127.0.0.1', () => resolve(s));
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('No test port');
  return [server, `http://127.0.0.1:${address.port}`];
}

beforeAll(() => {
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
  process.env.AAC_RUST_HOME_VIEW = 'on';
});
afterAll(() => {
  for (const [key, value] of Object.entries({
    CCS_DASHBOARD_AUTH_ENABLED: saved.auth,
    AAC_RUST_HOME_VIEW: saved.home,
    AAC_RUST_GROUP_HOME: saved.group,
    AAC_RUST_HOME_PRODUCER: saved.producer,
  }))
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
});

beforeEach(async () => {
  dashboard = fixture();
  updatedAt = '2026-11-14T11:59:00.000Z';
  inventory = { schemaVersion: 1, hostId: 'ubuntu', profiles: [] };
  // Each service reads its group once (its first request): the fed one with the group on, the plain one off.
  process.env.AAC_RUST_GROUP_HOME = 'on';
  service = new StandIn();
  expect(homeFedService({ locals: { rustService: service } } as unknown as Response)).toBe(service);
  process.env.AAC_RUST_GROUP_HOME = 'off';
  plain = new RustService('on');
  expect(homeFedService({ locals: { rustService: plain } } as unknown as Response)).toBeNull();
  [fedServer, fedUrl] = await listen(app(true));
  [plainServer, plainUrl] = await listen(app(false));
});
afterEach(async () => {
  for (const server of [fedServer, plainServer])
    await new Promise<void>((resolve) => server.close(() => resolve()));
});

const get = (url: string, headers: Record<string, string> = {}) =>
  fetch(`${url}/api/views/home`, {
    headers: { 'x-aac-home-context': header(CONTEXT), ...headers },
  });
const sources = () => service.puts.map((p) => p.source);

describe('GET /api/views/home, fed (R1)', () => {
  it('pushes each source once; then a request carries only its ids, clock and context, and the bytes pass through', async () => {
    for (let i = 0; i < 3; i++) {
      updatedAt = `2026-11-14T11:59:0${i}.000Z`;
      const response = await get(fedUrl);
      expect(response.status).toBe(200);
      expect(await response.text()).toBe(RUST_BYTES);
      expect(response.headers.get('etag')).toBe(ETAG);
      expect(response.headers.get('cache-control')).toBe('no-store');
    }
    // updatedAt is the request's clock, not part of the change key: one push per source for three requests.
    expect(sources()).toEqual(['home-dashboard-mac-1', 'home-opens', 'home-antigravity']);
    expect(service.views).toHaveLength(3);
    expect(Object.keys(service.views[2])).toEqual(['x-aac-feed', 'x-aac-home-context']);
    expect(JSON.parse(service.views[2]['x-aac-feed'])).toEqual({
      platform: 'mac',
      // isSecureTransport: loopback is a secure transport.
      secure: true,
      now: NOW,
      updatedAt: '2026-11-14T11:59:02.000Z',
      dashboard: service.held.get('home-dashboard-mac-1')?.generation,
      opens: service.held.get('home-opens')?.generation,
      antigravity: service.held.get('home-antigravity')?.generation,
    });
    expect(decodeURIComponent(service.views[2]['x-aac-home-context'])).toBe(
      JSON.stringify(CONTEXT)
    );
    // The pushed text is the change key: JSON.stringify of the offered answer before its pass, updatedAt blank.
    const pushed = service.puts[0];
    const offered = {
      ...dashboard,
      updatedAt: '',
      server: { version: '0.0.0-test', commit: null, views: ['home'] },
    };
    expect(pushed.data).toBe(JSON.stringify(offered));
    expect(JSON.parse(pushed.facts)).toEqual({
      order: { 'claude:a': 0 },
      epochs: {
        '2026-11-14T10:00:00.000Z': Date.parse('2026-11-14T10:00:00.000Z'),
        '2026-11-14T11:00:00.000Z': Date.parse('2026-11-14T11:00:00.000Z'),
      },
    });
    expect(service.puts[1].data).toBe('[]');
    expect(JSON.parse(service.puts[2].data).data).toEqual(inventory);
  });

  it('a changed source is pushed once with a newer generation; the others are not', async () => {
    await get(fedUrl);
    const first = service.held.get('home-dashboard-mac-1')?.generation;
    (
      dashboard.accounts[0] as unknown as { windows: { usedPercent: number }[] }
    ).windows[0].usedPercent = 41;
    expect((await get(fedUrl)).status).toBe(200);
    await get(fedUrl);
    expect(sources()).toEqual([
      'home-dashboard-mac-1',
      'home-opens',
      'home-antigravity',
      'home-dashboard-mac-1',
    ]);
    expect(Number(service.held.get('home-dashboard-mac-1')?.generation)).toBeGreaterThan(
      Number(first)
    );
    // Another platform is another generation.
    await fetch(`${fedUrl}/api/views/home?platform=windows`, {
      headers: { 'x-aac-home-context': header({ ...CONTEXT, platform: 'windows' }) },
    });
    expect(sources().at(-1)).toBe('home-dashboard-windows-1');
  });

  it('concurrent requests with the same new content share one push', async () => {
    let open: () => void = () => {};
    service.pushGate = new Promise<void>((resolve) => (open = resolve));
    const replies = Array.from({ length: 5 }, () => get(fedUrl));
    await new Promise((r) => setTimeout(r, 50));
    open();
    for (const reply of await Promise.all(replies)) expect(reply.status).toBe(200);
    expect(sources()).toEqual(['home-dashboard-mac-1', 'home-opens', 'home-antigravity']);
    expect(service.views).toHaveLength(5);
  });

  it('a push during a request gives stale: the original handler answers, and the next request pushes again', async () => {
    await get(fedUrl);
    // Another push lands after this front door recorded its generation (a newer one is held).
    service.held.set('home-dashboard-mac-1', { generation: '999999', data: '{}', facts: '{}' });
    const raced = await get(fedUrl);
    const today = await get(plainUrl);
    expect(raced.status).toBe(200);
    // Exactly the answer of today's handler (D7 here; its own Rust reads fall back): same bytes, its headers.
    expect(await raced.text()).toBe(await today.text());
    expect(raced.headers.get('etag')).toBe(today.headers.get('etag'));
    // The next request pushes the dashboard again (newer than 999999 is not needed: the stand-in only orders).
    const before = service.puts.length;
    service.held.delete('home-dashboard-mac-1');
    expect(await (await get(fedUrl)).text()).toBe(RUST_BYTES);
    expect(service.puts.length).toBeGreaterThan(before);
  });

  it("a front door whose numbers are behind the service's (an earlier process fed it) moves past them", async () => {
    // Another front door pushed a generation far ahead; this one's push loses once, then lands.
    service.held.set('home-dashboard-mac-1', {
      generation: '9000000000000000',
      data: '{}',
      facts: '{}',
    });
    const first = await get(fedUrl);
    expect(await first.text()).toBe(await (await get(plainUrl)).text());
    const second = await get(fedUrl);
    expect(await second.text()).toBe(RUST_BYTES);
    expect(Number(service.held.get('home-dashboard-mac-1')?.generation)).toBeGreaterThan(9e15);
  });

  it('a new service boot holds nothing: every source is pushed again', async () => {
    await get(fedUrl);
    service.boot = 'c'.repeat(24);
    service.held.clear();
    expect(await (await get(fedUrl)).text()).toBe(RUST_BYTES);
    expect(sources()).toEqual([
      'home-dashboard-mac-1',
      'home-opens',
      'home-antigravity',
      'home-dashboard-mac-1',
      'home-opens',
      'home-antigravity',
    ]);
  });

  it("every decline answers today's handler, byte for byte, with its reason counted", async () => {
    const today = async () => (await get(plainUrl)).text();
    const reasons: string[] = [];
    const check = async (reason: string, setup: () => void, undo: () => void) => {
      setup();
      try {
        const reply = await get(fedUrl);
        expect(reply.status).toBe(200);
        expect(await reply.text()).toBe(await today());
        reasons.push(reason);
      } finally {
        undo();
      }
    };
    // A Rust decline (any named reason).
    await check(
      'locale',
      () => (service.decline = 'locale'),
      () => (service.decline = null)
    );
    // A push that does not land (new content, so the dashboard must be pushed).
    const settings = (dashboard as unknown as { settings: { refreshIntervalSeconds: number } })
      .settings;
    await check(
      'feed',
      () => {
        service.failPush = true;
        settings.refreshIntervalSeconds = 61;
      },
      () => (service.failPush = false)
    );
    // An input JSON cannot carry exactly (a Map) is never pushed.
    await check(
      'input',
      () => ((dashboard as unknown as Record<string, unknown>).extra = new Map([['a', 1]])),
      () => delete (dashboard as unknown as Record<string, unknown>).extra
    );
    // A page zone this engine does not agree with.
    const shifted = {
      ...CONTEXT,
      zone: { ...CONTEXT.zone, offsets: CONTEXT.zone?.offsets.map(([s, m]) => [s, m + 60]) },
    };
    const zoned = await get(fedUrl, { 'x-aac-home-context': header(shifted) });
    expect(await zoned.text()).toBe(
      await (await get(plainUrl, { 'x-aac-home-context': header(shifted) })).text()
    );
    // The Antigravity runtime failed: today's handler reads it again.
    const was = inventory;
    await check(
      'input',
      () => (inventory = THROWS),
      () => (inventory = was)
    );
    const counted = (
      service.diagnostics().routes as Record<string, { unavailable: Record<string, number> }>
    )['/api/views/home'];
    for (const reason of ['locale', 'feed', 'input', 'zone'])
      expect(counted.unavailable[reason]).toBeGreaterThan(0);
    expect(reasons).toEqual(['locale', 'feed', 'input', 'input']);
  });

  it("a dashboard the per-response pass throws on is today's 500, not an unhandled error", async () => {
    // Not a list: TS declines before any push (a null window is the Rust service's decline, lifecycle.rs).
    (dashboard as unknown as Record<string, unknown>).accounts = 'not a list';
    const fed = await get(fedUrl);
    const today = await get(plainUrl);
    expect([fed.status, today.status]).toEqual([500, 500]);
    expect(await fed.text()).toBe(await today.text());
    expect(service.puts).toEqual([]);
  });

  it("review L1: a window whose JSON is not what the pass reads goes to today's handler", async () => {
    const window = () =>
      (dashboard.accounts[0] as unknown as { windows: Record<string, unknown>[] }).windows[0];
    for (const [name, setup] of [
      // TS's spread keeps this member's place; the JSON text has none, so Rust would append the flag.
      ['resetPassed: undefined', () => (window().resetPassed = undefined)],
      // The pass reads a Date as "not a string" (never passed); its JSON is an ISO string.
      ['a Date resetAt', () => (window().resetAt = new Date('2026-11-14T11:00:00.000Z'))],
      // `??` keeps NaN; JSON writes null, which falls back to the account's sample.
      ['a NaN sampledAt', () => (window().sampledAt = Number.NaN)],
    ] as const) {
      dashboard = fixture();
      setup();
      const before = service.puts.length;
      const reply = await get(fedUrl);
      expect([name, await reply.text()]).toEqual([name, await (await get(plainUrl)).text()]);
      expect(service.puts.length).toBe(before);
    }
    const counted = (
      service.diagnostics().routes as Record<string, { unavailable: Record<string, number> }>
    )['/api/views/home'];
    expect(counted.unavailable.input).toBe(3);
  });

  it('review L4: a dashboard too large to push is refused once per change, not rebuilt each request', async () => {
    (dashboard as unknown as Record<string, unknown>).note = 'x'.repeat(4 * 1024 * 1024);
    for (let i = 0; i < 3; i++) {
      const reply = await get(fedUrl);
      expect(reply.status).toBe(200);
      expect(reply.headers.get('etag')).not.toBe(ETAG);
    }
    expect(sources()).not.toContain('home-dashboard-mac-1');
    const counted = (
      service.diagnostics().routes as Record<string, { unavailable: Record<string, number> }>
    )['/api/views/home'];
    expect(counted.unavailable.cap).toBe(3);
  });

  it('another root stays with the TS readers', async () => {
    const other = new StandIn();
    other.feedBoot = () => null;
    service.feedBoot = other.feedBoot;
    const reply = await get(fedUrl);
    expect(await reply.text()).toBe(await (await get(plainUrl)).text());
    expect(service.puts).toEqual([]);
  });

  it('the ETag revalidates: a matching If-None-Match is a 304 with no body', async () => {
    const first = await get(fedUrl);
    expect(first.headers.get('etag')).toBe(ETAG);
    const again = await get(fedUrl, { 'if-none-match': ETAG });
    expect(again.status).toBe(304);
    expect(await again.text()).toBe('');
    expect((await get(fedUrl, { 'if-none-match': 'W/"other"' })).status).toBe(200);
  });

  it('without a readable page context the request still goes fed, with no context header (no view, as today)', async () => {
    const reply = await fetch(`${fedUrl}/api/views/home`);
    expect(await reply.text()).toBe(RUST_BYTES);
    expect(Object.keys(service.views.at(-1) ?? {})).toEqual(['x-aac-feed']);
  });

  it('with the group off the handler is D7 unchanged: no push, no fed request', async () => {
    const before = service.puts.length;
    await get(plainUrl);
    expect(service.puts.length).toBe(before);
  });
});

describe('fed facts', () => {
  it('passFaithful admits plain JSON members and refuses what JSON would change or drop', () => {
    const with_ = (window: Record<string, unknown>, account: Record<string, unknown> = {}) =>
      ({ accounts: [{ ...account, windows: [window] }] }) as unknown as AccountDashboard;
    expect(
      passFaithful(
        with_({ resetAt: '2026-11-14T11:00:00.000Z', sampledAt: null, resetPassed: false })
      )
    ).toBe(true);
    expect(
      passFaithful(with_({ resetAt: 5, sampledAt: 'x', resetPassed: true }, { sampledAt: 1 }))
    ).toBe(true);
    expect(
      passFaithful({
        accounts: [null, 3, { windows: 'x' }, { windows: [null, 1, []] }],
      } as unknown as AccountDashboard)
    ).toBe(true);
    for (const bad of [
      with_({ resetPassed: undefined }),
      with_({ resetAt: new Date(0) }),
      with_({ sampledAt: Number.POSITIVE_INFINITY }),
      with_({ resetAt: { at: 1 } }),
      with_({}, { sampledAt: undefined }),
      with_(Object.create({ resetAt: '2026-11-14T11:00:00.000Z' })),
      with_(Object.defineProperty({}, 'resetPassed', { value: true, enumerable: false })),
    ])
      expect(passFaithful(bad)).toBe(false);
  });

  it('the epochs are every string markPassedResets hands to Date.parse', () => {
    const epochs = resetEpochs({
      accounts: [
        {
          sampledAt: '2026-11-14T10:00:00.000Z',
          windows: [{ resetAt: 'never' }, { sampledAt: 5 }, null, 3],
        },
        { windows: 'x' },
        null,
      ],
    } as unknown as AccountDashboard);
    expect(epochs).toEqual({
      '2026-11-14T10:00:00.000Z': Date.parse('2026-11-14T10:00:00.000Z'),
      never: null,
    });
  });

  it("the server's zone facts are the page's own algorithm and window (view-model.mjs homeZoneFacts)", async () => {
    const page = await import('../../../web-dashboard/public/view-model.mjs');
    expect(page.HOME_ZONE_DAYS).toEqual(HOME_ZONE_DAYS);
    for (const zone of [
      'America/New_York',
      'Europe/London',
      'Asia/Kolkata',
      'Australia/Lord_Howe',
      'UTC',
      'Africa/Casablanca',
    ])
      for (const at of [NOW, Date.parse('2027-03-14T06:59:59.000Z')]) {
        const facts = serverZoneFacts(zone, at);
        expect(facts).toEqual(page.homeZoneFacts(zone, at));
        expect(
          homeZoneAgrees({ ...CONTEXT, timeZone: zone, now: at, zone: facts } as HomeContext)
        ).toBe(true);
        const shifted = {
          ...facts!,
          offsets: facts!.offsets.map(([s, m]) => [s, m + 30] as [number, number]),
        };
        expect(
          homeZoneAgrees({ ...CONTEXT, timeZone: zone, now: at, zone: shifted } as HomeContext)
        ).toBe(false);
      }
    expect(serverZoneFacts('Mars/Olympus', NOW)).toBeNull();
  });
});

describe('GET /api/views/home, the Rust feed producer (G17)', () => {
  it('producer on: the raw inputs go to Rust, its generations name the fed request, and TS pushes nothing', async () => {
    process.env.AAC_RUST_HOME_PRODUCER = 'on';
    try {
      const response = await get(fedUrl);
      expect(response.status).toBe(200);
      expect(await response.text()).toBe(RUST_BYTES);
      expect(service.puts).toHaveLength(0);
      expect(service.inputsCalls).toHaveLength(1);
      const inputs = service.inputsCalls[0];
      // The dashboard goes as read (updatedAt present, server.views offered), not as the blanked change key.
      expect((inputs.dashboard as { updatedAt: string }).updatedAt).toBe(updatedAt);
      expect((inputs.dashboard as { server: { views: string[] } }).server.views).toEqual(['home']);
      expect(inputs.antigravity).toEqual(inventory);
      expect(inputs.platform).toBe('mac');
      const feed = JSON.parse(service.views[0]['x-aac-feed']) as Record<string, string>;
      expect([feed.dashboard, feed.opens, feed.antigravity]).toEqual(['900', '901', '902']);
    } finally {
      delete process.env.AAC_RUST_HOME_PRODUCER;
    }
  });

  it("a producer decline answers from today's handler", async () => {
    process.env.AAC_RUST_HOME_PRODUCER = 'on';
    service.produceDecline = 'collation';
    try {
      const response = await get(fedUrl);
      expect(response.status).toBe(200);
      expect(await response.text()).not.toBe(RUST_BYTES);
      expect(service.inputsCalls).toHaveLength(1);
      expect(service.views).toHaveLength(0);
    } finally {
      delete process.env.AAC_RUST_HOME_PRODUCER;
    }
  });

  it('producer off (the default): the TS producer pushes as before', async () => {
    delete process.env.AAC_RUST_HOME_PRODUCER;
    const response = await get(fedUrl);
    expect(response.status).toBe(200);
    expect(service.inputsCalls).toHaveLength(0);
    expect(sources()).toEqual(['home-dashboard-mac-1', 'home-opens', 'home-antigravity']);
  });
});
