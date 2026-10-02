import { createHash } from 'crypto';
import {
  authFile,
  authFileStamp,
  authNow,
  readAuthJsonSync,
  withAuthWriteGate,
  writeAuthJson,
} from './dashboard-auth-files';

/**
 * The session epoch behind "sign out other browsers" (CONTRACT-auth-devices
 * section 3): `~/.ccs/auth/state.json` holds `{ version: 1, sessionEpoch }`.
 * Login, setup and every session rotation write the current epoch into the
 * session; the request guard turns a session from an older epoch into a
 * signed-out one (401 `session_revoked`). It works with any session store and
 * never has to list sessions.
 *
 * A small in-memory index of live sessions (a SHA-256 of the session id,
 * never the id) answers "how many other browsers are signed in".
 */
interface EpochCache {
  stamp: string | null;
  /** null: the file is unreadable, so no session is current until it is rewritten. */
  epoch: number | null;
}

const epochCache = new Map<string, EpochCache>();

function parseEpoch(value: unknown): number | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  const epoch = record.sessionEpoch;
  return record.version === 1 && typeof epoch === 'number' && Number.isSafeInteger(epoch)
    ? epoch
    : null;
}

/** The current epoch: 0 when no file exists yet, null when the file cannot be trusted. */
export function currentSessionEpoch(): number | null {
  const file = authFile('state.json');
  const stamp = authFileStamp(file);
  const cached = epochCache.get(file);
  if (cached && cached.stamp === stamp) return cached.epoch;
  const read = readAuthJsonSync(file);
  const epoch = read.state === 'absent' ? 0 : read.state === 'ok' ? parseEpoch(read.value) : null;
  epochCache.set(file, { stamp, epoch });
  return epoch;
}

/** Whether a signed-in session belongs to the current epoch (a session without one counts as 0). */
export function isSessionEpochCurrent(sessionEpoch: unknown): boolean {
  const current = currentSessionEpoch();
  if (current === null) return false;
  const own = typeof sessionEpoch === 'number' ? sessionEpoch : 0;
  return own === current;
}

async function writeEpoch(epoch: number): Promise<number> {
  const file = authFile('state.json');
  await writeAuthJson(file, { version: 1, sessionEpoch: epoch });
  epochCache.set(file, { stamp: authFileStamp(file), epoch });
  return epoch;
}

/**
 * The epoch a new session is written with. A state file that cannot be read is
 * replaced with a fresh epoch no earlier session can carry (fail closed).
 */
export function ensureSessionEpoch(): Promise<number> {
  const current = currentSessionEpoch();
  if (current !== null) return Promise.resolve(current);
  return withAuthWriteGate(async () => {
    const again = currentSessionEpoch();
    if (again !== null) return again;
    return writeEpoch(Math.max(1, Math.floor(authNow() / 1000)));
  });
}

/** Sign out every session of the current epoch; returns the new epoch. */
export function bumpSessionEpoch(): Promise<number> {
  return withAuthWriteGate(async () => {
    const current = currentSessionEpoch();
    const next = current === null ? Math.max(1, Math.floor(authNow() / 1000)) : current + 1;
    const epoch = await writeEpoch(next);
    dropOtherSessions(null);
    return epoch;
  });
}

/* ------------------------------------------------------------------------ */
/* Live session index (in memory only).                                      */
/* ------------------------------------------------------------------------ */

interface SessionEntry {
  scope: string;
  epoch: number;
  createdAt: number;
  lastSeenAt: number;
  expiresAt: number | null;
}

const sessions = new Map<string, SessionEntry>();

export function sessionKey(sessionId: string): string {
  return createHash('sha256').update(sessionId, 'utf8').digest('hex');
}

function scope(): string {
  return authFile('state.json');
}

function prune(now: number): void {
  for (const [key, entry] of sessions) {
    if (entry.expiresAt !== null && entry.expiresAt <= now) sessions.delete(key);
  }
}

/** Record or refresh a signed-in session (by id hash) in the current scope. */
export function noteSession(sessionId: string, epoch: number, expiresAt: Date | null): void {
  const now = authNow();
  const key = sessionKey(sessionId);
  const existing = sessions.get(key);
  sessions.set(key, {
    scope: scope(),
    epoch,
    createdAt: existing?.createdAt ?? now,
    lastSeenAt: now,
    expiresAt: expiresAt ? expiresAt.getTime() : null,
  });
}

export function forgetSession(sessionId: string | undefined): void {
  if (sessionId) sessions.delete(sessionKey(sessionId));
}

/** Signed-in sessions of the current epoch in this scope, other than `sessionId`. */
export function countOtherSessions(sessionId: string | undefined): number {
  const now = authNow();
  prune(now);
  const epoch = currentSessionEpoch();
  const own = sessionId ? sessionKey(sessionId) : null;
  const here = scope();
  let count = 0;
  for (const [key, entry] of sessions) {
    if (key !== own && entry.scope === here && entry.epoch === epoch) count += 1;
  }
  return count;
}

/** After an epoch bump: drop every entry of this scope except `keepSessionId`. */
function dropOtherSessions(keepSessionId: string | null): void {
  const keep = keepSessionId ? sessionKey(keepSessionId) : null;
  const here = scope();
  for (const [key, entry] of sessions) {
    if (key !== keep && entry.scope === here) sessions.delete(key);
  }
}

/** Tests only. */
export function resetDashboardAuthStateForTests(): void {
  epochCache.clear();
  sessions.clear();
}
