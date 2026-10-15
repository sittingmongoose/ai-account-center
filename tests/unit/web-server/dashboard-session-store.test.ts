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

const DAY = 24 * 60 * 60 * 1000;
const NOW = Date.parse('2026-10-15T12:00:00.000Z');
const SID = 'synthetic-session-id-that-is-a-bearer-secret';

let home = '';
let previous: Record<'CCS_HOME' | 'CCS_DIR', string | undefined> = {
  CCS_HOME: undefined,
  CCS_DIR: undefined,
};

beforeEach(() => {
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-session-store-'));
  previous = { CCS_HOME: process.env.CCS_HOME, CCS_DIR: process.env.CCS_DIR };
  delete process.env.CCS_DIR;
  process.env.CCS_HOME = home;
});

afterEach(() => {
  for (const [name, value] of Object.entries(previous)) {
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
  fs.rmSync(home, { recursive: true, force: true });
});

function signedIn(expires: number | null): session.SessionData {
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
    epoch: 3,
  } as unknown as session.SessionData;
}

function call<T>(run: (done: (error: unknown, value?: T) => void) => void): Promise<T | undefined> {
  return new Promise((resolve, reject) =>
    run((error, value) => (error ? reject(error) : resolve(value)))
  );
}

describe('dashboard session store', () => {
  it('keeps a sign-in across a restart, without the session id in the file', async () => {
    const first = new DashboardSessionStore({ now: () => NOW, idleMs: 7 * DAY });
    await call((done) => first.set(SID, signedIn(NOW + 7 * DAY), done));
    await first.save();
    const text = fs.readFileSync(sessionsFile(), 'utf8');
    expect(text).not.toContain(SID);
    expect(text).toContain('person');
    if (process.platform !== 'win32') expect(fs.statSync(sessionsFile()).mode & 0o077).toBe(0);
    const restarted = new DashboardSessionStore({ now: () => NOW + DAY, idleMs: 7 * DAY });
    const loaded = await call<session.SessionData | null>((done) => restarted.get(SID, done));
    expect(loaded).toMatchObject({ authenticated: true, username: 'person', epoch: 3 });
    expect(
      await call<session.SessionData | null>((done) => restarted.get('another-id', done))
    ).toBeNull();
  });

  it('drops an expired session and forgets a signed-out one after a restart', async () => {
    const store = new DashboardSessionStore({ now: () => NOW, idleMs: 7 * DAY });
    await call((done) => store.set(SID, signedIn(NOW + DAY), done));
    await call((done) => store.set('signed-out', signedIn(NOW + 7 * DAY), done));
    await call((done) => store.destroy('signed-out', done));
    await store.save();
    const later = new DashboardSessionStore({ now: () => NOW + 2 * DAY, idleMs: 7 * DAY });
    expect(await call<session.SessionData | null>((done) => later.get(SID, done))).toBeNull();
    expect(
      await call<session.SessionData | null>((done) => later.get('signed-out', done))
    ).toBeNull();
    expect(await call<number>((done) => later.length(done))).toBe(0);
  });

  it('extends a session in use and keeps a Remember-me-off session for the idle lifetime', async () => {
    let now = NOW;
    const store = new DashboardSessionStore({ now: () => now, idleMs: 7 * DAY });
    await call((done) => store.set(SID, signedIn(NOW + DAY), done));
    now = NOW + 12 * 60 * 60 * 1000;
    await call((done) => store.touch(SID, signedIn(now + DAY), done));
    await call((done) => store.set('browser-only', signedIn(null), done));
    await store.save();
    const restarted = new DashboardSessionStore({
      now: () => NOW + 30 * 60 * 60 * 1000,
      idleMs: 7 * DAY,
    });
    expect(
      await call<session.SessionData | null>((done) => restarted.get(SID, done))
    ).not.toBeNull();
    expect(
      await call<session.SessionData | null>((done) => restarted.get('browser-only', done))
    ).not.toBeNull();
  });

  it('ignores an unsafe or malformed file instead of failing sign-in', async () => {
    fs.mkdirSync(path.dirname(sessionsFile()), { recursive: true, mode: 0o700 });
    fs.writeFileSync(
      sessionsFile(),
      '{"version":1,"sessions":[{"key":"nothex","data":"{}","expires":1}]}',
      { mode: 0o600 }
    );
    const store = new DashboardSessionStore({ now: () => NOW, idleMs: 7 * DAY });
    expect(await call<number>((done) => store.length(done))).toBe(0);
    await call((done) => store.set(SID, signedIn(NOW + DAY), done));
    expect(await call<session.SessionData | null>((done) => store.get(SID, done))).not.toBeNull();
  });

  it('defaults new setups to a 7-day session', () => {
    expect(DEFAULT_SESSION_LIFETIME_DAYS).toBe(7);
  });
});
