import { getCcsDir } from '../../utils/config-manager';
import type { ClaudeDesktopProfile } from './claude-desktop-profile-service';
import { SshClaudeHostTransport, type ClaudeHostTransport } from './claude-host-transport';

/**
 * Whether a Claude profile on the Mac is signed in, read before any Open so
 * the dashboard and trays can say "Sign-in needed on Mac" up front.
 *
 * It runs the existing `session` host step (claude-host-transport.ts): the
 * fixed Mac program reads only the profile's plaintext sign-in marker (the
 * account uuid and whether an encrypted token cache is present) and answers
 * one boolean. Nothing is decrypted, printed or changed, and nothing is
 * signed in or out. Windows needs no such call: the usage helper that already
 * runs for each Windows profile reports a missing sign-in itself.
 *
 * Answers are cached per profile so a dashboard refresh costs no extra ssh
 * call most of the time. A failed check (host asleep, rebooting, unreachable)
 * answers null, which callers treat as "unknown" and never as signed out.
 */
export type ClaudeSignInState = 'signed-in' | 'signed-out';

const ANSWER_TTL_MS = 5 * 60_000;
const FAILURE_TTL_MS = 60_000;
const FORCED_MIN_AGE_MS = 30_000;
const MAX_ENTRIES = 64;

interface Entry {
  at: number;
  ttl: number;
  promise: Promise<ClaudeSignInState | null>;
}

const cache = new Map<string, Entry>();

export function invalidateClaudeMacSignInStateCache(): void {
  cache.clear();
}

/** The Mac launcher the `session` step needs, or null when the manifest lacks one. */
function macLauncher(profile: ClaudeDesktopProfile) {
  const mac = profile.mac;
  if (!profile.id || !mac?.sshHost || !mac.profilePath?.startsWith('/')) return null;
  return { launcherName: mac.launcherName, profilePath: mac.profilePath, sshHost: mac.sshHost };
}

export async function getClaudeMacSignInState(
  profile: ClaudeDesktopProfile,
  options: { refresh?: boolean; transport?: ClaudeHostTransport; now?: () => number } = {}
): Promise<ClaudeSignInState | null> {
  const launcher = macLauncher(profile);
  if (!launcher) return null;
  const now = options.now ?? Date.now;
  const key = JSON.stringify([getCcsDir(), profile.id, launcher]);
  const existing = cache.get(key);
  const age = existing ? now() - existing.at : Infinity;
  if (existing && age < existing.ttl && !(options.refresh === true && age >= FORCED_MIN_AGE_MS)) {
    return existing.promise;
  }
  const transport = options.transport ?? new SshClaudeHostTransport();
  const entry: Entry = { at: now(), ttl: ANSWER_TTL_MS, promise: Promise.resolve(null) };
  entry.promise = transport.sessionState('mac', { launcher }).then(
    (state) => state,
    () => {
      entry.ttl = FAILURE_TTL_MS;
      return null;
    }
  );
  cache.delete(key);
  cache.set(key, entry);
  while (cache.size > MAX_ENTRIES) {
    const oldest = cache.keys().next().value;
    if (oldest === undefined) break;
    cache.delete(oldest);
  }
  return entry.promise;
}
