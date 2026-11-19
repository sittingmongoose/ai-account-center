/** L1 bridge floor, pure rules: breaker, restart budget, route counters, socket layout, build. */
import { describe, expect, it } from 'bun:test';
import {
  BREAKER_OPEN_MS,
  RESTART_DELAYS_MS,
  RestartBudget,
  RouteBreakers,
  RouteStats,
  summarize,
} from '../../../src/web-server/rust-service-health';
import {
  buildMatches,
  parseBuild,
  RUST_SERVICE_PROTOCOL,
  shortName,
  SOCKET_PATH_LIMIT,
  socketLayout,
} from '../../../src/web-server/rust-service-lifecycle';
import routes from '../../../src/web-server/rust-service-routes.json';

function clock(start = 1_000_000) {
  let now = start;
  return { now: () => now, advance: (ms: number) => (now += ms) };
}

describe('route breaker', () => {
  it('opens after three transport failures in a row, then lets exactly one probe through', () => {
    const time = clock();
    const breakers = new RouteBreakers(time.now);
    const route = '/api/accounts/settings';
    expect(breakers.allow(route)).toBe(true);
    expect(breakers.failure(route)).toBe(false);
    expect(breakers.failure(route)).toBe(false);
    expect(breakers.failure(route)).toBe(true);
    expect(breakers.state(route)).toBe('open');
    expect(breakers.allow(route)).toBe(false);
    // Other routes are unaffected.
    expect(breakers.allow('/api/health')).toBe(true);
    time.advance(BREAKER_OPEN_MS - 1);
    expect(breakers.allow(route)).toBe(false);
    time.advance(1);
    expect(breakers.state(route)).toBe('half-open');
    expect(breakers.allow(route)).toBe(true);
    expect(breakers.allow(route)).toBe(false);
    // A failed probe reopens for another 30 s.
    expect(breakers.failure(route)).toBe(true);
    expect(breakers.allow(route)).toBe(false);
    time.advance(BREAKER_OPEN_MS);
    expect(breakers.allow(route)).toBe(true);
    breakers.success(route);
    expect(breakers.state(route)).toBe('closed');
    expect(breakers.allow(route)).toBe(true);
  });

  it('checks admission without reserving the half-open probe, and a new boot starts closed', () => {
    const time = clock();
    const breakers = new RouteBreakers(time.now);
    const route = '/api/status';
    for (let i = 0; i < 3; i++) breakers.failure(route);
    expect(breakers.blocked(route)).toBe(true);
    time.advance(BREAKER_OPEN_MS);
    // Admission may ask any number of times; only dispatch reserves the probe.
    expect(breakers.blocked(route)).toBe(false);
    expect(breakers.blocked(route)).toBe(false);
    expect(breakers.allow(route)).toBe(true);
    expect(breakers.blocked(route)).toBe(true);
    breakers.reset();
    expect(breakers.blocked(route)).toBe(false);
    expect(breakers.state(route)).toBe('closed');
  });

  it('does not count declines: any reply resets the run of failures', () => {
    const breakers = new RouteBreakers(clock().now);
    const route = '/api/status';
    breakers.failure(route);
    breakers.failure(route);
    breakers.success(route);
    expect(breakers.failure(route)).toBe(false);
    expect(breakers.failure(route)).toBe(false);
    expect(breakers.state(route)).toBe('closed');
  });
});

describe('restart budget', () => {
  it('backs off 1 s, 5 s, 30 s, 2 min, 10 min and stops at five an hour', () => {
    const time = clock();
    const budget = new RestartBudget(time.now);
    const delays: (number | null)[] = [];
    for (let i = 0; i < 6; i++) {
      const delay = budget.next();
      delays.push(delay);
      if (delay !== null) time.advance(delay);
    }
    expect(delays).toEqual([...RESTART_DELAYS_MS, null]);
    expect(RESTART_DELAYS_MS).toEqual([1_000, 5_000, 30_000, 120_000, 600_000]);
    // An hour after the first restarts, the budget comes back.
    time.advance(3_600_000);
    expect(budget.next()).toBe(1_000);
  });
});

describe('route counters', () => {
  it('counts per route and reason, and logs only changes of state', () => {
    const lines: [string, string, string, number][] = [];
    const stats = new RouteStats((route, state, previous, count) =>
      lines.push([route, state, previous, count])
    );
    const route = '/api/accounts/settings';
    stats.record(route, { kind: 'served' });
    stats.record(route, { kind: 'served' });
    stats.record(route, { kind: 'declined', reason: 'number' });
    stats.record(route, { kind: 'declined', reason: 'number' });
    stats.record(route, { kind: 'declined', reason: 'number' });
    stats.record(route, { kind: 'declined', reason: 'shape' });
    stats.record(route, { kind: 'skipped' });
    stats.record(route, { kind: 'failed', reason: 'deadline' });
    stats.record(route, { kind: 'served' });
    stats.record('/api/health', { kind: 'unavailable', reason: 'breaker' });
    expect(lines).toEqual([
      [route, 'serving', 'new', 1],
      [route, 'declined:number', 'serving', 2],
      [route, 'declined:shape', 'declined:number', 3],
      [route, 'failed:deadline', 'declined:shape', 1],
      [route, 'serving', 'failed:deadline', 1],
      ['/api/health', 'unavailable:breaker', 'new', 1],
    ]);
    const snapshot = stats.snapshot() as Record<string, Record<string, unknown>>;
    expect(snapshot[route]).toMatchObject({
      state: 'serving',
      served: 3,
      skipped: 1,
      declined: { number: 3, shape: 1 },
      failed: { deadline: 1 },
    });
    expect(Object.keys(snapshot)).toEqual(['/api/accounts/settings', '/api/health']);
  });

  it('keeps paired timings and summarizes them', () => {
    const stats = new RouteStats(() => {});
    for (let i = 1; i <= 100; i++) stats.pair('/api/status', i, i / 2);
    stats.pair('/api/status', null, 7);
    const paired = (stats.snapshot()['/api/status'] as { paired: Record<string, unknown> }).paired;
    expect(paired).toEqual({
      ts: { samples: 100, p50Ms: 50, p95Ms: 95 },
      rust: { samples: 100, p50Ms: 25, p95Ms: 47.5 },
    });
    expect(summarize([])).toEqual({ samples: 0, p50Ms: null, p95Ms: null });
  });
});

describe('socket layout (Fable L2)', () => {
  const id = 'a'.repeat(24);
  it('keeps today’s path while it fits, and binds a short private folder otherwise', () => {
    const short = '/home/fixture/.ccs';
    expect(socketLayout(short, id, {})).toEqual({
      folders: [`${short}/run/aac-http-${id}`],
      socketPath: `${short}/run/aac-http-${id}/http.sock`,
    });
    const long = '/home/fixture/' + 'u'.repeat(150) + '/.ccs';
    const hash = shortName(long);
    expect(socketLayout(long, id, { XDG_RUNTIME_DIR: '/run/user/1000' })).toEqual({
      folders: [`/run/user/1000/aac-${hash}`, `/run/user/1000/aac-${hash}/aac-http-${id}`],
      socketPath: `/run/user/1000/aac-${hash}/aac-http-${id}/http.sock`,
    });
    expect(
      socketLayout(long, id, { AAC_RUST_RUN_DIR: '/run/aac', XDG_RUNTIME_DIR: '/x' })?.socketPath
    ).toBe(`/run/aac/aac-${hash}/aac-http-${id}/http.sock`);
    const uid = process.getuid?.() ?? -1;
    expect(socketLayout(long, id, {})).toEqual({
      folders: [
        `/tmp/aac-${uid}`,
        `/tmp/aac-${uid}/${hash}`,
        `/tmp/aac-${uid}/${hash}/aac-http-${id}`,
      ],
      socketPath: `/tmp/aac-${uid}/${hash}/aac-http-${id}/http.sock`,
    });
    // A relative or over-long runtime folder is skipped.
    expect(socketLayout(long, id, { XDG_RUNTIME_DIR: 'run' })?.socketPath.startsWith('/tmp/')).toBe(
      true
    );
    expect(
      socketLayout(long, id, { XDG_RUNTIME_DIR: '/' + 'x'.repeat(90) })?.socketPath.startsWith(
        '/tmp/'
      )
    ).toBe(true);
    for (const root of [short, long, '/' + 'r'.repeat(200)]) {
      const layout = socketLayout(root, id, {});
      expect(layout && Buffer.byteLength(layout.socketPath) <= SOCKET_PATH_LIMIT).toBe(true);
    }
  });

  it('names the short folder as aac-http does (first 12 hex of SHA-256 of the root)', () => {
    // crates/aac-http/src/unix.rs short_name_matches_the_front_door
    expect(shortName('/home/fixture/.ccs')).toBe('0a2271d2066d');
  });
});

describe('build check (Fable L5)', () => {
  const sha = 'b'.repeat(64);
  const build = {
    commit: '7bc46ca90',
    sha256: sha,
    protocol: RUST_SERVICE_PROTOCOL,
    storeSchema: 2,
  };
  it('reads only the exact identity header', () => {
    expect(parseBuild(JSON.stringify(build))).toEqual(build);
    for (const bad of [
      null,
      '',
      '{}',
      JSON.stringify({ ...build, extra: 1 }),
      JSON.stringify({ ...build, sha256: 'B'.repeat(64) }),
      JSON.stringify({ ...build, commit: 'main' }),
      JSON.stringify({ ...build, protocol: '2' }),
      'x'.repeat(600),
    ])
      expect(parseBuild(bad)).toBeNull();
    expect(parseBuild(JSON.stringify({ ...build, commit: 'unknown' }))?.commit).toBe('unknown');
  });

  it('attaches only to the binary this front door would start, speaking its protocol', () => {
    expect(buildMatches(build, sha)).toBe(true);
    expect(buildMatches(build, 'c'.repeat(64))).toBe(false);
    expect(buildMatches({ ...build, protocol: 1 }, sha)).toBe(false);
    expect(buildMatches(null, sha)).toBe(false);
  });
});

describe('route table (L1)', () => {
  it('names a group, roles, a budget and a handler on every Rust row', () => {
    const owned = routes.filter((r) => r.owner === 'rust') as unknown as Record<string, unknown>[];
    expect(owned.length).toBeGreaterThan(0);
    for (const row of owned) {
      expect(Object.keys(row).sort()).toEqual(
        ['budgetMs', 'group', 'handler', 'method', 'owner', 'path', 'roles'].sort()
      );
      expect(Array.isArray(row.roles) && row.roles.length > 0).toBe(true);
      const budget =
        row.handler === 'agent-control'
          ? 2500
          : ['events', 'agent', 'store', 'status-answers'].includes(String(row.handler))
            ? 0
            : 400;
      expect(row.budgetMs).toBe(budget);
      if (String(row.handler).startsWith('agent')) expect(row.method).toBe('POST');
      const internal = String(row.path).startsWith('/internal/');
      expect((row.roles as string[]).includes('service')).toBe(internal);
    }
    expect(routes[routes.length - 1]).toEqual({ method: '*', path: '*', owner: 'ts-fallback' });
  });
});
