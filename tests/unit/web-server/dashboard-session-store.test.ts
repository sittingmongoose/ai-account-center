import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import type session from 'express-session';
import {
  DashboardSessionStore,
  sessionsFile,
} from '../../../src/web-server/middleware/dashboard-session-store';
import { DEFAULT_SESSION_LIFETIME_DAYS } from '../../../src/config/schemas/auth';
import {
  countOtherSessions,
  resetDashboardAuthStateForTests,
} from '../../../src/web-server/services/dashboard-auth-state';

const DAY = 24 * 60 * 60 * 1000;
const NOW = Date.parse('2026-10-15T12:00:00.000Z');
const SID = 'synthetic-session-id-that-is-a-bearer-secret';

let home = '';
let previous: Record<'CCS_HOME' | 'CCS_DIR', string | undefined> = {
  CCS_HOME: undefined,
  CCS_DIR: undefined,
};
const open: DashboardSessionStore[] = [];

beforeEach(() => {
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-session-store-'));
  previous = { CCS_HOME: process.env.CCS_HOME, CCS_DIR: process.env.CCS_DIR };
  delete process.env.CCS_DIR;
  process.env.CCS_HOME = home;
  // The dashboard creates its CCS folder at startup; the store never creates it.
  fs.mkdirSync(path.join(home, '.ccs'));
  resetDashboardAuthStateForTests();
});

afterEach(async () => {
  for (const store of open.splice(0)) await store.close();
  for (const [name, value] of Object.entries(previous)) {
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
  fs.rmSync(home, { recursive: true, force: true });
  resetDashboardAuthStateForTests();
});

function store(options: Partial<ConstructorParameters<typeof DashboardSessionStore>[0]> = {}) {
  const created = new DashboardSessionStore({ now: () => NOW, idleMs: 7 * DAY, ...options });
  open.push(created);
  return created;
}

function signedIn(expires: number | null, epoch = 3): session.SessionData {
  return {
    cookie: {
      originalMaxAge: expires === null ? null : 7 * DAY,
      expires: expires === null ? null : new Date(expires).toISOString(),
      httpOnly: true,
      path: '/',
      sameSite: 'strict',
    },
    authenticated: true,
    username: 'person',
    epoch,
  } as unknown as session.SessionData;
}

function call<T>(run: (done: (error: unknown, value?: T) => void) => void): Promise<T | undefined> {
  return new Promise((resolve, reject) =>
    run((error, value) => (error ? reject(error) : resolve(value)))
  );
}

describe('dashboard session store', () => {
  it('keeps a sign-in across a restart, written at once and without the session id', async () => {
    const first = store();
    await call((done) => first.set(SID, signedIn(NOW + 7 * DAY), done));
    // No explicit save: the sign-in code waits for the write that set() started.
    await first.whenSaved();
    const text = fs.readFileSync(sessionsFile(), 'utf8');
    expect(text).not.toContain(SID);
    expect(text).toContain('person');
    if (process.platform !== 'win32') expect(fs.statSync(sessionsFile()).mode & 0o077).toBe(0);
    const restarted = store({ now: () => NOW + DAY });
    const loaded = await call<session.SessionData | null>((done) => restarted.get(SID, done));
    expect(loaded).toMatchObject({ authenticated: true, username: 'person', epoch: 3 });
    expect(
      await call<session.SessionData | null>((done) => restarted.get('another-id', done))
    ).toBeNull();
  });

  it('drops an expired session and never brings back a signed-out one', async () => {
    const first = store();
    await call((done) => first.set(SID, signedIn(NOW + DAY), done));
    await call((done) => first.set('signed-out', signedIn(NOW + 7 * DAY), done));
    // The sign-out is on disk once destroy() answers: a restart right after cannot revive it.
    await call((done) => first.destroy('signed-out', done));
    const later = store({ now: () => NOW + 2 * DAY });
    expect(await call<session.SessionData | null>((done) => later.get(SID, done))).toBeNull();
    expect(
      await call<session.SessionData | null>((done) => later.get('signed-out', done))
    ).toBeNull();
    expect(await call<number>((done) => later.length(done))).toBe(0);
  });

  it('extends a session in use and keeps a Remember-me-off session for the idle lifetime set now', async () => {
    let now = NOW;
    let idle = 7 * DAY;
    const first = store({ now: () => now, idleMs: () => idle });
    await call((done) => first.set(SID, signedIn(NOW + DAY), done));
    now = NOW + 12 * 60 * 60 * 1000;
    await call((done) => first.touch(SID, signedIn(now + DAY), done));
    // The lifetime changed in Settings after startup: a browser-session sign-in follows it.
    idle = 30 * DAY;
    await call((done) => first.set('browser-only', signedIn(null), done));
    // The rolling extension is batched; the shutdown save writes it.
    await first.close();
    const restarted = store({ now: () => NOW + 30 * 60 * 60 * 1000 });
    expect(
      await call<session.SessionData | null>((done) => restarted.get(SID, done))
    ).not.toBeNull();
    const muchLater = store({ now: () => NOW + 20 * DAY });
    expect(
      await call<session.SessionData | null>((done) => muchLater.get('browser-only', done))
    ).not.toBeNull();
  });

  it('keeps a Remember-me-off session for the lifetime at its sign-in', async () => {
    let idle = 30 * DAY;
    const first = store({ idleMs: () => idle });
    const own = { ...signedIn(null), idleLifetimeMs: DAY } as unknown as session.SessionData;
    await call((done) => first.set(SID, own, done));
    idle = 365 * DAY;
    await first.close();
    const later = store({ now: () => NOW + 2 * DAY });
    expect(await call<session.SessionData | null>((done) => later.get(SID, done))).toBeNull();
  });

  it('ignores an unsafe or malformed file instead of failing sign-in', async () => {
    fs.mkdirSync(path.dirname(sessionsFile()), { recursive: true, mode: 0o700 });
    fs.writeFileSync(
      sessionsFile(),
      '{"version":1,"sessions":[{"key":"nothex","data":"{}","expires":1}]}',
      { mode: 0o600 }
    );
    const first = store();
    expect(await call<number>((done) => first.length(done))).toBe(0);
    await call((done) => first.set(SID, signedIn(NOW + DAY), done));
    expect(await call<session.SessionData | null>((done) => first.get(SID, done))).not.toBeNull();
  });

  it('counts kept sign-ins as other browsers right after a restart', async () => {
    const first = store();
    await call((done) => first.set(SID, signedIn(NOW + 7 * DAY, 0), done));
    await call((done) => first.set('other-browser', signedIn(NOW + 7 * DAY, 0), done));
    await first.close();
    resetDashboardAuthStateForTests();
    const restarted = store();
    await call<number>((done) => restarted.length(done));
    expect(countOtherSessions(SID)).toBe(1);
  });

  it('never writes after it closed or once its home is gone', async () => {
    const first = store();
    await call((done) => first.set(SID, signedIn(NOW + DAY), done));
    await call((done) => first.touch(SID, signedIn(NOW + 2 * DAY), done));
    await first.close();
    fs.rmSync(path.join(home, '.ccs'), { recursive: true, force: true });
    await call((done) => first.set('late', signedIn(NOW + DAY), done));
    const gone = store();
    await call((done) => gone.set('late-too', signedIn(NOW + DAY), done));
    await gone.close();
    expect(fs.existsSync(path.join(home, '.ccs'))).toBe(false);
  });

  it('a second dashboard on the same home reads the sign-ins but leaves the file to the first', async () => {
    const first = store();
    await call((done) => first.set(SID, signedIn(NOW + 7 * DAY), done));
    // Another live process on this computer holds the writer claim.
    fs.writeFileSync(
      path.join(path.dirname(sessionsFile()), 'sessions.owner'),
      JSON.stringify({ pid: process.ppid, host: os.hostname() })
    );
    const second = store();
    expect(await call<session.SessionData | null>((done) => second.get(SID, done))).not.toBeNull();
    expect(second.isWriter).toBe(false);
    const before = fs.readFileSync(sessionsFile(), 'utf8');
    await call((done) => second.set('second-browser', signedIn(NOW + DAY), done));
    await call((done) => second.destroy(SID, done));
    expect(fs.readFileSync(sessionsFile(), 'utf8')).toBe(before);
    expect(await call<session.SessionData | null>((done) => second.get(SID, done))).toBeNull();
  });

  it('defaults new setups to a 7-day session', () => {
    expect(DEFAULT_SESSION_LIFETIME_DAYS).toBe(7);
  });
});
