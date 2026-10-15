import { existsSync } from 'fs';
import fs from 'fs/promises';
import os from 'os';
import path from 'path';
import session from 'express-session';
import { AuthError } from '../../errors/error-types';
import { createLogger } from '../../services/logging';
import {
  authDirectory,
  ensureAuthDirectory,
  withAuthWriteGate,
} from '../services/dashboard-auth-files';
import { noteStoredSession, sessionKey } from '../services/dashboard-auth-state';
import { readPrivateJsonFile, writePrivateJsonFile } from '../services/private-json-store';

/**
 * Dashboard sign-ins that survive a restart. express-session's default MemoryStore lost every
 * session whenever the dashboard restarted (a deploy, a reboot, a crash), so phones and browsers
 * were signed out long before the configured session lifetime.
 *
 * Sessions live in memory and are saved to `~/.ccs/auth/sessions.json` (folder 0700, file 0600,
 * atomic, through the auth write gate). Each is keyed by the SHA-256 of its session id (the same
 * key as the "other browsers" index), so the file never holds a cookie value that could be
 * replayed. Expired sessions are dropped when read and on every save, and at most MAX_SESSIONS are
 * kept (the ones that expire last).
 *
 * A sign-in is written at once and the sign-in code answers only after the file holds it
 * (`whenSaved`); a failed save keeps the session in memory (it then lasts until the next restart,
 * as before this store). A sign-out is answered only after the file no longer holds it, and a
 * failed sign-out save fails the sign-out, so a signed-out session can never come back after a
 * restart.
 * The rolling expiry of a session in use is saved at most once a minute, and once more at
 * shutdown (`close`).
 *
 * One dashboard writes the file; it claims that role with an exclusive create, so two dashboards
 * starting together cannot both win. Another dashboard process on the same `~/.ccs` (for example
 * `bar serve` beside `dashboard`) keeps its own sign-ins in memory only, as before this store, and
 * does not honour the writer's: a sign-out on either can then never be undone by the other. When
 * the writer is gone, the other takes over the file.
 *
 * Saved sessions are honoured only after the startup credential check succeeded; if it failed,
 * every kept session signs in again (fail closed).
 *
 * Every other check is unchanged: a loaded session still carries its epoch and credential stamp,
 * so "Sign out other browsers", a password change and the transport rules apply to it exactly as
 * before a restart.
 */
export const SESSIONS_FILE = 'sessions.json';
const OWNER_FILE = 'sessions.owner';
const MAX_SESSIONS = 500;
const MAX_FILE_BYTES = 4 * 1024 * 1024;
const MAX_SESSION_BYTES = 8 * 1024;
const SAVE_LATER_MS = 60_000;
/**
 * The writer refreshes its claim this often. A claim is free when its process is gone, when it was
 * made before this computer last started, or when it was not refreshed for OWNER_STALE_MS (a reused
 * process id); a live writer is never displaced sooner.
 */
const OWNER_BEAT_MS = 60_000;
const OWNER_STALE_MS = 60 * OWNER_BEAT_MS;

const logger = createLogger('dashboard-auth');

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

export interface DashboardSessionStoreOptions {
  file?: string;
  now?: () => number;
  /**
   * How long a session whose cookie has no expiry (Remember me off) is kept without use, when the
   * session does not carry its own `idleLifetimeMs` from its sign-in.
   */
  idleMs: number | (() => number);
  /** Settles first (the startup credential check), so no request reads a session before it. */
  after?: Promise<unknown>;
}

export class DashboardSessionStore extends session.Store {
  private readonly file: string;
  private readonly ownerFile: string;
  private readonly now: () => number;
  private readonly idleMs: () => number;
  private readonly sessions = new Map<string, StoredSession>();
  /** Sessions this process ended while another process wrote the file. */
  private readonly ended = new Set<string>();
  private readonly ready: Promise<void>;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private beat: ReturnType<typeof setInterval> | null = null;
  private saving: Promise<void> = Promise.resolve();
  private writer = false;
  /** The startup credential check succeeded, so saved sessions may be honoured. */
  private trusted = false;
  private closed = false;
  private warned = false;

  constructor(options: DashboardSessionStoreOptions) {
    super();
    this.file = options.file ?? sessionsFile();
    this.ownerFile = path.join(path.dirname(this.file), OWNER_FILE);
    this.now = options.now ?? Date.now;
    const idle = options.idleMs;
    this.idleMs = typeof idle === 'function' ? idle : () => idle;
    this.ready = (options.after ?? Promise.resolve())
      .then(
        () => true,
        (error: unknown) => {
          this.warn('load', error);
          return false;
        }
      )
      .then(async (trusted) => {
        this.trusted = trusted;
        this.writer = await this.claim().catch(() => false);
        if (this.writer && this.trusted) await this.loadFile();
        if (this.closed) return;
        this.beat = setInterval(() => void this.heartbeat(), OWNER_BEAT_MS);
        this.beat.unref?.();
      });
  }

  /** Whether this process writes the file (another dashboard on the same home may). */
  get isWriter(): boolean {
    return this.writer;
  }

  /** The folder that holds the CCS home; a store never creates it again once it is gone. */
  private homeExists(): boolean {
    return existsSync(path.dirname(path.dirname(this.file)));
  }

  private claimText(): string {
    return JSON.stringify({ pid: process.pid, host: os.hostname(), boot: bootMinute() });
  }

  /**
   * Claims the writer role. The claim is created exclusively, so of two dashboards starting
   * together only one wins. A claim held by a gone process, or not refreshed for OWNER_STALE_MS, is
   * taken over by one process at a time (an atomic mkdir guards the takeover).
   */
  private async claim(): Promise<boolean> {
    if (!this.homeExists()) return false;
    const mine = this.claimText();
    await ensureAuthDirectory(path.dirname(this.ownerFile));
    if (await this.createClaim(mine)) return true;
    const held = await fs.readFile(this.ownerFile, 'utf8').catch(() => null);
    if (held !== null && normalClaim(held) === mine) return true;
    if (held !== null && !(await this.claimIsStale(held))) return false;
    const lock = `${this.ownerFile}.lock`;
    const lockStat = await fs.stat(lock).catch(() => null);
    // A takeover that crashed half way leaves its lock; it never takes a minute.
    if (lockStat && Date.now() - lockStat.mtimeMs > OWNER_BEAT_MS) {
      await fs.rmdir(lock).catch(() => undefined);
    }
    try {
      await fs.mkdir(lock);
    } catch {
      return false;
    }
    try {
      const again = await fs.readFile(this.ownerFile, 'utf8').catch(() => null);
      // Another process changed the claim meanwhile: it is theirs.
      if (again !== held) return again === null ? await this.createClaim(mine) : false;
      await fs.unlink(this.ownerFile).catch(() => undefined);
      return await this.createClaim(mine);
    } finally {
      await fs.rmdir(lock).catch(() => undefined);
    }
  }

  /** Creates the claim only if no claim exists (O_EXCL). */
  private async createClaim(text: string): Promise<boolean> {
    try {
      await fs.writeFile(this.ownerFile, text, { flag: 'wx', mode: 0o600 });
      return true;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'EEXIST') return false;
      throw error;
    }
  }

  private async claimIsStale(held: string): Promise<boolean> {
    const stat = await fs.stat(this.ownerFile).catch(() => null);
    let owner: { pid?: unknown; host?: unknown; boot?: unknown } = {};
    try {
      owner = JSON.parse(held) as typeof owner;
    } catch {
      /* An unreadable claim is free. */
    }
    const fresh = stat !== null && Date.now() - stat.mtimeMs < OWNER_STALE_MS;
    // A claim from before this computer last started belongs to a process that is gone, whatever
    // now runs under its process id.
    const sameBoot = typeof owner.boot === 'number' && Math.abs(owner.boot - bootMinute()) <= 2;
    const alive =
      typeof owner.pid === 'number' &&
      owner.host === os.hostname() &&
      sameBoot &&
      pidAlive(owner.pid);
    return !(fresh && alive);
  }

  /** Whether the claim on disk is still this process's (another may have taken over a stale one). */
  private async stillWriter(): Promise<boolean> {
    const held = await fs.readFile(this.ownerFile, 'utf8').catch(() => null);
    return held !== null && normalClaim(held) === this.claimText();
  }

  /** The writer refreshes its claim; another process takes over a claim that went stale. */
  private async heartbeat(): Promise<void> {
    if (this.closed) return;
    if (this.writer) {
      if (!(await this.stillWriter())) {
        this.displaced();
        return;
      }
      const at = new Date();
      await fs.utimes(this.ownerFile, at, at).catch(() => undefined);
      return;
    }
    if (!(await this.claim().catch(() => false)) || this.closed) return;
    this.writer = true;
    // The previous writer is gone: keep what it saved (once trusted), minus what this process ended.
    if (this.trusted) await this.loadFile();
    this.ended.clear();
    await this.write().catch((error: unknown) => this.warn('save', error));
  }

  private async loadFile(): Promise<void> {
    let read: Awaited<ReturnType<typeof readPrivateJsonFile>> | null = null;
    try {
      read = await readPrivateJsonFile(this.file, MAX_FILE_BYTES);
    } catch (error) {
      this.warn('load', error);
    }
    if (!read || read.state !== 'ok') {
      if (read && read.state !== 'absent') this.warn('load', new Error(read.state));
      return;
    }
    const value = read.value as { version?: unknown; sessions?: unknown } | null;
    if (!value || value.version !== 1 || !Array.isArray(value.sessions)) return;
    const now = this.now();
    const scope = path.join(path.dirname(this.file), 'state.json');
    for (const entry of value.sessions.slice(0, MAX_SESSIONS)) {
      if (!entry || typeof entry !== 'object') continue;
      const { key, data, expires } = entry as Record<string, unknown>;
      if (typeof key !== 'string' || !/^[0-9a-f]{64}$/.test(key)) continue;
      if (typeof data !== 'string' || data.length > MAX_SESSION_BYTES) continue;
      if (typeof expires !== 'number' || !Number.isFinite(expires) || expires <= now) continue;
      // A session written by a sign-in after this load started, or ended here, wins over the file.
      if (this.sessions.has(key) || this.ended.has(key)) continue;
      this.sessions.set(key, { data, expires });
      try {
        const parsed = JSON.parse(data) as {
          authenticated?: unknown;
          epoch?: unknown;
          cookie?: unknown;
        };
        if (parsed.authenticated === true) {
          const epoch = typeof parsed.epoch === 'number' ? parsed.epoch : 0;
          noteStoredSession(key, scope, epoch, expiryOfCookie(parsed));
        }
      } catch {
        /* Unreadable data is dropped by get(). */
      }
    }
  }

  private expiryOf(value: session.SessionData): number {
    const own = (value as { idleLifetimeMs?: unknown }).idleLifetimeMs;
    const idle = typeof own === 'number' && Number.isFinite(own) && own > 0 ? own : this.idleMs();
    return expiryOfCookie(value) ?? this.now() + idle;
  }

  private warn(action: 'load' | 'save', error: unknown): void {
    if (this.warned) return;
    this.warned = true;
    try {
      logger.warn(
        `auth.sessions.${action}_failed`,
        action === 'load'
          ? 'Saved dashboard sign-ins could not be read; sign-ins start fresh'
          : 'Dashboard sign-ins could not be saved; they last until the next restart',
        { reason: error instanceof Error ? error.message : String(error) }
      );
    } catch {
      /* Logging is best effort. */
    }
  }

  /** Saves the rolling expiries within SAVE_LATER_MS; an earlier pending save is kept. */
  private scheduleLater(): void {
    if (this.timer || this.closed || !this.writer) return;
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.write().catch((error: unknown) => this.warn('save', error));
    }, SAVE_LATER_MS);
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

  /**
   * Another dashboard took the file over (this one was gone or frozen too long). The sessions this
   * process held from the file are now the other one's to keep or end, and a sign-out here could no
   * longer reach them, so they are dropped here: those browsers sign in again on this dashboard.
   */
  private displaced(): void {
    if (!this.writer) return;
    this.writer = false;
    this.sessions.clear();
    this.warn('save', new Error('another dashboard took over the saved sign-ins'));
  }

  /**
   * Writes the current sessions; resolves once the file holds them and rejects when the write
   * failed. A reader, a closed store and a home that is gone write nothing; a writer that finds
   * another dashboard holds the file now rejects (a sign-out there was not saved).
   */
  private write(): Promise<void> {
    const run = this.saving.then(async () => {
      await this.ready;
      if (!this.writer || this.closed || !this.homeExists()) return;
      if (!(await this.stillWriter())) {
        this.displaced();
        throw new AuthError('Another dashboard holds the saved sign-ins.');
      }
      this.prune();
      const sessions = [...this.sessions.entries()].map(([key, entry]) => ({ key, ...entry }));
      await withAuthWriteGate(async () => {
        await ensureAuthDirectory(path.dirname(this.file));
        await writePrivateJsonFile(this.file, { version: 1, sessions });
      });
    });
    this.saving = run.catch(() => undefined);
    return run;
  }

  /** Writes now (the shutdown save, and tests); a failure is logged, never thrown. */
  async save(): Promise<void> {
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    await this.write().catch((error: unknown) => this.warn('save', error));
  }

  /** The shutdown save, then no more writes or timers; a writer gives up its claim. */
  async close(): Promise<void> {
    await this.ready.catch(() => undefined);
    await this.save();
    this.closed = true;
    if (this.beat) clearInterval(this.beat);
    this.beat = null;
    if (!this.writer) return;
    const mine = this.claimText();
    await withAuthWriteGate(async () => {
      const held = await fs.readFile(this.ownerFile, 'utf8').catch(() => null);
      if (held !== null && normalClaim(held) === mine) {
        await fs.unlink(this.ownerFile).catch(() => undefined);
      }
    }).catch(() => undefined);
  }

  override get(sid: string, callback: GetCallback): void {
    void this.ready.then(() => {
      const key = sessionKey(sid);
      const entry = this.sessions.get(key);
      if (!entry) return callback(null, null);
      if (entry.expires <= this.now()) {
        this.sessions.delete(key);
        this.scheduleLater();
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
      const key = sessionKey(sid);
      this.sessions.set(key, { data, expires: this.expiryOf(value) });
      this.ended.delete(key);
      // Written now and answered at once: express-session also saves from inside res.end, where a
      // late answer splits the response around it. The sign-in code waits with whenSaved().
      this.write().catch((error: unknown) => this.warn('save', error));
      callback?.();
    }, callback);
  }

  /** Resolves once every write started so far has finished (a failed one is logged). */
  whenSaved(): Promise<void> {
    return this.saving;
  }

  override touch(sid: string, value: session.SessionData, callback?: Callback): void {
    void this.ready.then(() => {
      const key = sessionKey(sid);
      if (this.sessions.has(key)) {
        this.sessions.set(key, { data: JSON.stringify(value), expires: this.expiryOf(value) });
        this.scheduleLater();
      }
      callback?.();
    }, callback);
  }

  override destroy(sid: string, callback?: Callback): void {
    void this.ready.then(() => {
      const key = sessionKey(sid);
      this.ended.add(key);
      this.sessions.delete(key);
      // A sign-out is answered only once the file no longer holds the session. It writes even when
      // the session is already gone from memory: an earlier sign-out whose save failed is retried.
      this.write().then(
        () => callback?.(),
        (error: unknown) => callback?.(error)
      );
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
      for (const key of this.sessions.keys()) this.ended.add(key);
      this.sessions.clear();
      this.write().then(
        () => callback?.(),
        (error: unknown) => callback?.(error)
      );
    }, callback);
  }
}

/** A claim's text in one canonical form, so a rewritten claim compares equal. */
function normalClaim(text: string): string {
  try {
    const value = JSON.parse(text) as { pid?: unknown; host?: unknown; boot?: unknown };
    return JSON.stringify({ pid: value.pid, host: value.host, boot: value.boot });
  } catch {
    return text;
  }
}

/** When this computer last started, in whole minutes since the epoch (stable within a boot). */
function bootMinute(): number {
  return Math.round((Date.now() - os.uptime() * 1000) / 60_000);
}

/** The cookie's expiry in epoch milliseconds, or null for a browser-session cookie. */
function expiryOfCookie(value: { cookie?: unknown }): number | null {
  const raw = (value.cookie as { expires?: unknown } | undefined)?.expires;
  const expires =
    raw instanceof Date ? raw.getTime() : typeof raw === 'string' ? Date.parse(raw) : NaN;
  return Number.isFinite(expires) ? expires : null;
}

function pidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'EPERM';
  }
}
