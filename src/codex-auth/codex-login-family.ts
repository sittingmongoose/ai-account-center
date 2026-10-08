/**
 * Login-family fingerprints for saved-login renewal.
 *
 * Codex refresh tokens rotate, and the provider may revoke a whole login
 * family when a rotated token is used again. A saved login may only be renewed
 * when no other Codex login this machine holds can belong to its family. Every
 * login AAC can see is reduced here to an in-memory fingerprint: a digest of
 * the refresh token, the session ids (`session_id` in the access token, `sid`
 * in the id token), the original sign-in times (`auth_time`) and the account
 * binding. Fingerprints never leave the process; callers report only paths
 * and match labels.
 */
import * as fs from 'fs';
import * as path from 'path';
import { createHash } from 'crypto';
import { decodeIdToken, decodeJwtPayload } from './decode-id-token';
import {
  decodeCodexActivationIdentity,
  matchesCodexActivationIdentity,
  type CodexActivationIdentity,
} from './codex-activation-identity';
import {
  codexProcessHome,
  readProcesses,
  type CodexProcessSnapshot,
} from './codex-activation-runtime';

const MAX_AUTH_BYTES = 1024 * 1024;
const MAX_DIRECTORY_ENTRIES = 1000;
const OPENAI_AUTH_CLAIM = 'https://api.openai.com/auth';

/** Private and in-memory only. */
export interface CodexLoginFingerprint {
  refreshDigest: string | null;
  sessionIds: ReadonlySet<string>;
  authTimes: ReadonlySet<number>;
  binding: CodexActivationIdentity | null;
  email: string | null;
  /** Account id claimed by the access token, when it carries one. */
  accessAccountId: string | null;
  accessExpiresAt: number | null;
  lastRefreshAt: number | null;
  hasAccessToken: boolean;
}

export type CodexLoginSourceKind =
  | 'live'
  | 'profile'
  | 'codex_home'
  | 'process_home'
  | 'cliproxy'
  | 'extra';

export interface CodexLoginSource {
  path: string;
  realPath: string | null;
  kind: CodexLoginSourceKind;
  /** `none`: readable but holds no token login (an API-key file, another provider). */
  state: 'ok' | 'none' | 'unreadable';
  fingerprint: CodexLoginFingerprint | null;
}

export type CodexFamilyMatch = 'same_file' | 'refresh_token' | 'session' | 'auth_time';

export interface CodexProcessScan {
  state: 'ok' | 'unavailable' | 'failed';
  homes: { home: string; realHome: string | null }[];
}

function text(value: unknown): string | null {
  return typeof value === 'string' && value.length > 0 ? value : null;
}

function seconds(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : null;
}

function claimsOf(token: string | null): Record<string, unknown> | null {
  return token ? (decodeJwtPayload(token) as Record<string, unknown> | null) : null;
}

function collectMarkers(
  claims: Record<string, unknown> | null,
  sessionIds: Set<string>,
  authTimes: Set<number>
): void {
  if (!claims) return;
  const nested = claims[OPENAI_AUTH_CLAIM];
  const scopes = [claims, nested && typeof nested === 'object' ? nested : {}] as Record<
    string,
    unknown
  >[];
  for (const scope of scopes) {
    for (const key of ['session_id', 'sid']) {
      const value = text(scope[key]);
      if (value) sessionIds.add(value);
    }
    const authTime = seconds(scope.auth_time);
    if (authTime !== null) authTimes.add(authTime);
  }
}

/**
 * Fingerprint a parsed auth file: Codex's `{ tokens: {...} }` shape or a flat
 * token file (CLIProxy). Null when it holds no refresh or access token.
 */
export function fingerprintCodexLogin(parsed: unknown): CodexLoginFingerprint | null {
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return null;
  const root = parsed as Record<string, unknown>;
  const nested = root.tokens;
  const tokens = (
    nested && typeof nested === 'object' && !Array.isArray(nested) ? nested : root
  ) as Record<string, unknown>;
  const refreshToken = text(tokens.refresh_token);
  const accessToken = text(tokens.access_token);
  if (!refreshToken && !accessToken) return null;
  const idToken = text(tokens.id_token);
  const idClaims = claimsOf(idToken);
  const accessClaims = claimsOf(accessToken);
  const sessionIds = new Set<string>();
  const authTimes = new Set<number>();
  collectMarkers(idClaims, sessionIds, authTimes);
  collectMarkers(accessClaims, sessionIds, authTimes);
  const accessAuth = accessClaims?.[OPENAI_AUTH_CLAIM] as Record<string, unknown> | undefined;
  const exp = seconds(accessClaims?.exp);
  const lastRefresh = typeof root.last_refresh === 'string' ? Date.parse(root.last_refresh) : NaN;
  return {
    refreshDigest: refreshToken ? createHash('sha256').update(refreshToken).digest('hex') : null,
    sessionIds,
    authTimes,
    binding: idToken ? decodeCodexActivationIdentity(idToken, tokens.account_id) : null,
    email: (idToken ? decodeIdToken(idToken).email : undefined) ?? null,
    accessAccountId:
      accessAuth && typeof accessAuth === 'object' ? text(accessAuth.chatgpt_account_id) : null,
    accessExpiresAt: exp === null ? null : exp * 1000,
    lastRefreshAt: Number.isFinite(lastRefresh) ? lastRefresh : null,
    hasAccessToken: accessToken !== null,
  };
}

/** Bounded read of a regular file (links followed). ENOENT -> null; other failures -> 'unreadable'. */
export function readAuthBytes(file: string): Buffer | 'unreadable' | null {
  try {
    const stat = fs.statSync(file);
    if (!stat.isFile() || stat.size > MAX_AUTH_BYTES) return 'unreadable';
    return fs.readFileSync(file);
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    return code === 'ENOENT' || code === 'ENOTDIR' ? null : 'unreadable';
  }
}

export function parseAuthJson(content: Buffer): unknown {
  try {
    return JSON.parse(content.toString('utf8')) as unknown;
  } catch {
    // Parser messages can embed token text; never propagate them.
    return undefined;
  }
}

function realPathOf(file: string): string | null {
  try {
    return fs.realpathSync(file);
  } catch {
    return null;
  }
}

export function readCodexLoginSource(
  file: string,
  kind: CodexLoginSourceKind
): CodexLoginSource | null {
  const content = readAuthBytes(file);
  if (content === null) return null;
  const base = { path: file, realPath: realPathOf(file), kind };
  if (content === 'unreadable') return { ...base, state: 'unreadable', fingerprint: null };
  const parsed = parseAuthJson(content);
  if (parsed === undefined) return { ...base, state: 'unreadable', fingerprint: null };
  const fingerprint = fingerprintCodexLogin(parsed);
  return { ...base, state: fingerprint ? 'ok' : 'none', fingerprint };
}

function listDirectory(directory: string): fs.Dirent[] {
  try {
    return fs.readdirSync(directory, { withFileTypes: true }).slice(0, MAX_DIRECTORY_ENTRIES);
  } catch {
    return [];
  }
}

function isDirectory(entry: fs.Dirent, parent: string): boolean {
  if (entry.isDirectory()) return true;
  if (!entry.isSymbolicLink()) return false;
  try {
    return fs.statSync(path.join(parent, entry.name)).isDirectory();
  } catch {
    return false;
  }
}

/** `<home>/.codex*` homes and their direct child homes (such as `~/.codex-t3/<name>`). */
export function findHomeCodexAuthFiles(homeDir: string): string[] {
  const files: string[] = [];
  for (const entry of listDirectory(homeDir)) {
    if (!entry.name.startsWith('.codex') || !isDirectory(entry, homeDir)) continue;
    const home = path.join(homeDir, entry.name);
    files.push(path.join(home, 'auth.json'));
    for (const child of listDirectory(home)) {
      if (isDirectory(child, home)) files.push(path.join(home, child.name, 'auth.json'));
    }
  }
  return files;
}

/** Every `<instances>/<dir>/auth.json`, registered or not (staging, trash). */
export function findProfileAuthFiles(instancesDir: string): string[] {
  return listDirectory(instancesDir)
    .filter((entry) => isDirectory(entry, instancesDir))
    .map((entry) => path.join(instancesDir, entry.name, 'auth.json'));
}

/** CLIProxy Codex token files; other providers' files are not Codex logins. */
export function readCliproxyCodexSources(ccsDir: string): CodexLoginSource[] {
  const sources: CodexLoginSource[] = [];
  for (const folder of ['auth', 'auth-paused']) {
    const directory = path.join(ccsDir, 'cliproxy', folder);
    for (const entry of listDirectory(directory)) {
      if (!entry.name.endsWith('.json')) continue;
      const file = path.join(directory, entry.name);
      const content = readAuthBytes(file);
      if (content === null) continue;
      const parsed = content === 'unreadable' ? undefined : parseAuthJson(content);
      const named = entry.name.toLowerCase().startsWith('codex');
      if (parsed === undefined) {
        if (named) {
          sources.push({
            path: file,
            realPath: null,
            kind: 'cliproxy',
            state: 'unreadable',
            fingerprint: null,
          });
        }
        continue;
      }
      const type = (parsed as { type?: unknown } | null)?.type;
      if (type !== 'codex' && !(type === undefined && named)) continue;
      const fingerprint = fingerprintCodexLogin(parsed);
      sources.push({
        path: file,
        realPath: realPathOf(file),
        kind: 'cliproxy',
        state: fingerprint ? 'ok' : 'none',
        fingerprint,
      });
    }
  }
  return sources;
}

/**
 * Homes of running Codex CLI/desktop processes. Linux reads /proc; elsewhere
 * the scan is unavailable and the file sources stand alone.
 */
export function scanCodexProcessHomes(
  scan?: () => CodexProcessSnapshot[] | null
): CodexProcessScan {
  if (!scan && process.platform !== 'linux') return { state: 'unavailable', homes: [] };
  let processes: CodexProcessSnapshot[] | null;
  try {
    processes = (scan ?? readProcesses)();
  } catch {
    return { state: 'failed', homes: [] };
  }
  if (processes === null) return { state: 'unavailable', homes: [] };
  const seen = new Set<string>();
  const homes: CodexProcessScan['homes'] = [];
  for (const snapshot of processes) {
    const home = codexProcessHome(snapshot);
    if (!home || seen.has(home)) continue;
    seen.add(home);
    homes.push({ home, realHome: realPathOf(home) });
  }
  return { state: 'ok', homes };
}

function intersects<T>(left: ReadonlySet<T>, right: ReadonlySet<T>): boolean {
  for (const value of left) if (right.has(value)) return true;
  return false;
}

/** Same account, or unknown on either side (an unknown account is never ruled out). */
export function sameAccountOrUnknown(
  left: CodexLoginFingerprint,
  right: CodexLoginFingerprint
): boolean {
  if (left.binding && right.binding) {
    return (
      matchesCodexActivationIdentity(left.binding, right.binding) ||
      matchesCodexActivationIdentity(right.binding, left.binding)
    );
  }
  if (left.email && right.email) return left.email.toLowerCase() === right.email.toLowerCase();
  return true;
}

/**
 * Whether `other` may hold a login of the target's family: the same file, the
 * same refresh token, a shared session id, or the same account signed in at the
 * same time. `unverifiable` when `other` cannot be read, or may be the same
 * account but carries no family markers to tell the logins apart.
 */
export function compareLoginFamily(
  target: { fingerprint: CodexLoginFingerprint; realPath: string | null },
  other: CodexLoginSource
): CodexFamilyMatch[] | 'unverifiable' | null {
  if (target.realPath && other.realPath === target.realPath) return ['same_file'];
  if (other.state === 'unreadable') return 'unverifiable';
  const theirs = other.fingerprint;
  // Without a refresh token a copy can never rotate or replay the family.
  if (!theirs?.refreshDigest) return null;
  const ours = target.fingerprint;
  const matches: CodexFamilyMatch[] = [];
  if (ours.refreshDigest && ours.refreshDigest === theirs.refreshDigest) {
    matches.push('refresh_token');
  }
  if (intersects(ours.sessionIds, theirs.sessionIds)) matches.push('session');
  const related = sameAccountOrUnknown(ours, theirs);
  if (related && intersects(ours.authTimes, theirs.authTimes)) matches.push('auth_time');
  if (matches.length > 0) return matches;
  if (related && theirs.sessionIds.size === 0 && theirs.authTimes.size === 0) {
    return 'unverifiable';
  }
  return null;
}
