import { randomBytes } from 'crypto';
import fs from 'fs';
import path from 'path';
import {
  HOLDER_RECORD_BYTES,
  holderStaleReason,
  ownHolderRecord,
  parseHolderRecord,
} from './holder-record';
import { AntigravityError } from './errors';
import { ANTIGRAVITY_SIGNIN_DIR, signInRoot } from './signin-sandbox';

/**
 * `<ccs>/antigravity-signin/<profile>.running`: a terminal sign-in for one
 * profile is running. The marker names its process (pid, process start time
 * and boot), so a sign-in that died is recognised at once; a live one past
 * the sign-in's own 15-minute limit plus its provider check also stops
 * counting. Remove and Sign in again refuse `signin_running` while it counts.
 */
const PROFILE_NAME = /^[a-z][a-z0-9_-]{0,47}$/;
const MARKER_NAME = /^([a-z][a-z0-9_-]{0,47})\.running$/;
const TEMP_NAME = /^\.marker-[a-f0-9]{16}$/;
const MARKER_SILENT_MS = 30 * 60_000;
const TEMP_MAX_AGE_MS = 60 * 60_000;

export interface SignInMarker {
  /** Remove the marker, only while it is still this one. */
  release(): void;
}

function markerFile(ccsDir: string, profileId: string): string {
  if (!PROFILE_NAME.test(profileId)) throw new AntigravityError('antigravity-signin-bad-profile');
  return path.join(path.resolve(ccsDir), ANTIGRAVITY_SIGNIN_DIR, `${profileId}.running`);
}

/** Why the marker no longer counts, or null while its sign-in runs; 'absent' when there is none. */
function markerStaleReason(file: string, now: number): string | null | 'absent' {
  let before: fs.Stats;
  try {
    before = fs.lstatSync(file);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return 'absent';
    throw error;
  }
  if (!before.isFile() || before.isSymbolicLink() || before.size > HOLDER_RECORD_BYTES)
    return now - before.mtimeMs > MARKER_SILENT_MS ? 'unreadable and old' : null;
  let record = null;
  try {
    const fd = fs.openSync(file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
    try {
      record = parseHolderRecord(fs.readFileSync(fd, 'utf8'), fs.fstatSync(fd).mtimeMs);
    } finally {
      fs.closeSync(fd);
    }
  } catch {
    record = null;
  }
  if (!record) return now - before.mtimeMs > MARKER_SILENT_MS ? 'unreadable and old' : null;
  return holderStaleReason(record, now, MARKER_SILENT_MS);
}

/** True while a terminal sign-in for this profile runs. */
export function antigravitySignInRunning(
  ccsDir: string,
  profileId: string,
  now: number = Date.now()
): boolean {
  return markerStaleReason(markerFile(ccsDir, profileId), now) === null;
}

/** Unlink `file` only while it is still the file judged. */
function unlinkIfSame(file: string, judged: fs.Stats): void {
  try {
    const current = fs.lstatSync(file);
    if (current.dev === judged.dev && current.ino === judged.ino) fs.unlinkSync(file);
  } catch {
    /* Gone already. */
  }
}

/**
 * Mark a terminal sign-in for `profileId` as running. Returns null when a
 * live sign-in for the profile already holds the marker; a marker whose
 * sign-in is gone is replaced.
 */
export function claimAntigravitySignInMarker(
  ccsDir: string,
  profileId: string
): SignInMarker | null {
  const file = markerFile(ccsDir, profileId);
  const root = signInRoot(ccsDir);
  const temp = path.join(root, `.marker-${randomBytes(8).toString('hex')}`);
  fs.writeFileSync(temp, ownHolderRecord(), { flag: 'wx', mode: 0o600 });
  try {
    for (let attempt = 0; attempt < 2; attempt += 1) {
      try {
        // link(2) publishes the complete record at once and fails if one exists.
        fs.linkSync(temp, file);
        const own = fs.lstatSync(file);
        return { release: () => unlinkIfSame(file, own) };
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
      }
      let judged: fs.Stats;
      try {
        judged = fs.lstatSync(file);
      } catch {
        continue;
      }
      if (markerStaleReason(file, Date.now()) === null) return null;
      unlinkIfSame(file, judged);
    }
    return null;
  } finally {
    try {
      fs.unlinkSync(temp);
    } catch {
      /* Already gone. */
    }
  }
}

/** Maintenance: markers whose sign-in is gone, and stray marker drafts older than an hour. */
export function sweepAntigravitySignInMarkers(ccsDir: string, now: number = Date.now()): number {
  const root = path.join(path.resolve(ccsDir), ANTIGRAVITY_SIGNIN_DIR);
  let names: string[];
  try {
    names = fs.readdirSync(root);
  } catch {
    return 0;
  }
  let removed = 0;
  for (const name of names) {
    const file = path.join(root, name);
    try {
      const judged = fs.lstatSync(file);
      const stale = MARKER_NAME.test(name)
        ? markerStaleReason(file, now) !== null
        : TEMP_NAME.test(name) && judged.isFile() && now - judged.mtimeMs > TEMP_MAX_AGE_MS;
      if (!stale) continue;
      unlinkIfSame(file, judged);
      removed += 1;
    } catch {
      /* Retried at the next sweep. */
    }
  }
  return removed;
}
