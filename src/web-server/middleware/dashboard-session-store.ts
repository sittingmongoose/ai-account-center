import { createHash } from 'crypto';
import path from 'path';
import session from 'express-session';
import { authDirectory, withAuthWriteGate, writeAuthJson } from '../services/dashboard-auth-files';
import { readPrivateJsonFile } from '../services/private-json-store';

/**
 * Dashboard sign-ins that survive a restart. express-session's default MemoryStore lost every
 * session whenever the dashboard restarted (a deploy, a reboot, a crash), so phones and browsers
 * were signed out long before the configured session lifetime.
 *
 * Sessions live in memory and are saved to `~/.ccs/auth/sessions.json` (folder 0700, file 0600,
 * atomic, through the auth write gate). Each is keyed by a digest of its session id, so the file
 * never holds a cookie value that could be replayed. Expired sessions are dropped when read and on
 * every save, and at most MAX_SESSIONS are kept (the ones that expire last). A sign-in or sign-out
 * is saved within a second; the rolling expiry of a session in use is saved at most once a minute,
 * so a crash can lose at most a minute of an expiry extension, never a sign-in.
 *
 * Every other check is unchanged: a loaded session still carries its epoch, so "Sign out other
 * browsers", a password change and the transport rules apply to it exactly as before a restart.
 */
export const SESSIONS_FILE = 'sessions.json';
const MAX_SESSIONS = 500;
const MAX_FILE_BYTES = 4 * 1024 * 1024;
const MAX_SESSION_BYTES = 8 * 1024;
const SAVE_SOON_MS = 1_000;
const SAVE_LATER_MS = 60_000;

interface StoredSession {
  /** The session as express-session serialises it (cookie, username, epoch, transport). */
  data: string;
  /** Epoch milliseconds after which the session is gone. */
  expires: number;
}

type Callback = (error?: unknown) => void;
type GetCallback = (error: unknown, value?: session.SessionData | null) => void;

export function sessionsFile(): string {
  return path.join(authDirectory(), SESSIONS_FILE);
}

function digest(sid: string): string {
  return createHash('sha256').update(`aac-dashboard-session\0${sid}`, 'utf8').digest('hex');
}

export interface DashboardSessionStoreOptions {
  file?: string;
  now?: () => number;
  /** How long a session whose cookie has no expiry (Remember me off) is kept without use. */
  idleMs: number;
}

export class DashboardSessionStore extends session.Store {
  private readonly file: string;
  private readonly now: () => number;
  private readonly idleMs: number;
  private readonly sessions = new Map<string, StoredSession>();
  private readonly ready: Promise<void>;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private timerAt = 0;
  private saving: Promise<void> = Promise.resolve();

  constructor(options: DashboardSessionStoreOptions) {
    super();
    this.file = options.file ?? sessionsFile();
    this.now = options.now ?? Date.now;
    this.idleMs = options.idleMs;
    this.ready = this.loadFile();
  }

  private async loadFile(): Promise<void> {
    const read = await readPrivateJsonFile(this.file, MAX_FILE_BYTES).catch(() => null);
    if (!read || read.state !== 'ok') return;
    const value = read.value as { version?: unknown; sessions?: unknown } | null;
    if (!value || value.version !== 1 || !Array.isArray(value.sessions)) return;
    const now = this.now();
    for (const entry of value.sessions.slice(0, MAX_SESSIONS)) {
      if (!entry || typeof entry !== 'object') continue;
      const { key, data, expires } = entry as Record<string, unknown>;
      if (typeof key !== 'string' || !/^[0-9a-f]{64}$/.test(key)) continue;
      if (typeof data !== 'string' || data.length > MAX_SESSION_BYTES) continue;
      if (typeof expires !== 'number' || !Number.isFinite(expires) || expires <= now) continue;
      // A session written by a sign-in after this load started wins over the saved copy.
      if (!this.sessions.has(key)) this.sessions.set(key, { data, expires });
    }
  }

  private expiryOf(value: session.SessionData): number {
    const raw = (value.cookie as { expires?: unknown } | undefined)?.expires;
    const expires =
      raw instanceof Date ? raw.getTime() : typeof raw === 'string' ? Date.parse(raw) : NaN;
    return Number.isFinite(expires) ? expires : this.now() + this.idleMs;
  }

  /** Saves within `delay`; an earlier pending save is kept. */
  private schedule(delay: number): void {
    const at = this.now() + delay;
    if (this.timer && this.timerAt <= at) return;
    if (this.timer) clearTimeout(this.timer);
    this.timerAt = at;
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.save();
    }, delay);
    this.timer.unref?.();
  }

  private prune(): void {
    const now = this.now();
    for (const [key, entry] of this.sessions) if (entry.expires <= now) this.sessions.delete(key);
    if (this.sessions.size <= MAX_SESSIONS) return;
    const keep = [...this.sessions.entries()]
      .sort((a, b) => b[1].expires - a[1].expires)
      .slice(0, MAX_SESSIONS);
    this.sessions.clear();
    for (const [key, entry] of keep) this.sessions.set(key, entry);
  }

  /** Writes the current sessions now (also used at shutdown and by tests). */
  save(): Promise<void> {
    this.saving = this.saving.then(async () => {
      await this.ready;
      this.prune();
      const sessions = [...this.sessions.entries()].map(([key, entry]) => ({ key, ...entry }));
      try {
        await withAuthWriteGate(() => writeAuthJson(this.file, { version: 1, sessions }));
      } catch {
        /* A failed save keeps the sessions in memory; the next change saves again. */
      }
    });
    return this.saving;
  }

  override get(sid: string, callback: GetCallback): void {
    void this.ready.then(() => {
      const key = digest(sid);
      const entry = this.sessions.get(key);
      if (!entry) return callback(null, null);
      if (entry.expires <= this.now()) {
        this.sessions.delete(key);
        this.schedule(SAVE_LATER_MS);
        return callback(null, null);
      }
      try {
        callback(null, JSON.parse(entry.data) as session.SessionData);
      } catch {
        this.sessions.delete(key);
        callback(null, null);
      }
    }, callback);
  }

  override set(sid: string, value: session.SessionData, callback?: Callback): void {
    void this.ready.then(() => {
      const data = JSON.stringify(value);
      if (data.length > MAX_SESSION_BYTES) return callback?.(new Error('session too large'));
      this.sessions.set(digest(sid), { data, expires: this.expiryOf(value) });
      this.schedule(SAVE_SOON_MS);
      callback?.();
    }, callback);
  }

  override touch(sid: string, value: session.SessionData, callback?: Callback): void {
    void this.ready.then(() => {
      const key = digest(sid);
      if (this.sessions.has(key)) {
        this.sessions.set(key, { data: JSON.stringify(value), expires: this.expiryOf(value) });
        this.schedule(SAVE_LATER_MS);
      }
      callback?.();
    }, callback);
  }

  override destroy(sid: string, callback?: Callback): void {
    void this.ready.then(() => {
      if (this.sessions.delete(digest(sid))) this.schedule(SAVE_SOON_MS);
      callback?.();
    }, callback);
  }

  override length(callback: (error: unknown, length?: number) => void): void {
    void this.ready.then(() => {
      this.prune();
      callback(null, this.sessions.size);
    }, callback);
  }

  override clear(callback?: Callback): void {
    void this.ready.then(() => {
      this.sessions.clear();
      this.schedule(SAVE_SOON_MS);
      callback?.();
    }, callback);
  }
}
