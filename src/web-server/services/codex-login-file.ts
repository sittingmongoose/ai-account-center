import { constants } from 'fs';
import fs from 'fs/promises';
import path from 'path';
import {
  decodeCodexActivationIdentity,
  matchesCodexActivationIdentity,
  type CodexActivationIdentity,
} from '../../codex-auth/codex-activation-identity';
import { decodeIdToken, hasStructurallyValidIdToken } from '../../codex-auth/decode-id-token';
import type { CodexProfileMetadata } from '../../codex-auth/types';
import { ConfigError } from '../../errors/error-types';

const MAX_AUTH_BYTES = 1024 * 1024;

export interface CodexLoginIdentity {
  email: string;
  accountId: string | null;
  plan: string | null;
  /**
   * The private workspace and principal binding (Codex's activation identity
   * rules), or null when the login carries no consistent workspace claim.
   * Never part of a public DTO.
   */
  binding: CodexActivationIdentity | null;
}

/** Parsed JSON of a regular, size-limited file that is not a link; throws otherwise. */
async function readAuthFile(authPath: string): Promise<unknown> {
  let handle: Awaited<ReturnType<typeof fs.open>> | undefined;
  try {
    const noFollow = process.platform === 'win32' ? 0 : constants.O_NOFOLLOW;
    handle = await fs.open(authPath, constants.O_RDONLY | noFollow);
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size > MAX_AUTH_BYTES) throw new ConfigError('Not a login file.');
    return JSON.parse(await handle.readFile('utf8')) as unknown;
  } finally {
    await handle?.close().catch(() => undefined);
  }
}

interface AuthTokens {
  tokens?: {
    id_token?: unknown;
    access_token?: unknown;
    refresh_token?: unknown;
    account_id?: unknown;
  };
}

/** A complete Codex login: a structurally valid id token with an email, plus access and refresh tokens. */
export async function readCodexLogin(authPath: string): Promise<CodexLoginIdentity | null> {
  try {
    const parsed = (await readAuthFile(authPath)) as AuthTokens | null;
    const token = parsed?.tokens?.id_token;
    if (typeof token !== 'string' || !hasStructurallyValidIdToken(token)) return null;
    const identity = decodeIdToken(token);
    if (
      !identity.email ||
      typeof parsed?.tokens?.access_token !== 'string' ||
      !parsed.tokens.access_token ||
      typeof parsed.tokens?.refresh_token !== 'string' ||
      !parsed.tokens.refresh_token
    ) {
      return null;
    }
    return {
      email: identity.email,
      accountId: identity.account_id ?? null,
      plan: identity.plan_type ?? null,
      binding: decodeCodexActivationIdentity(token, parsed.tokens.account_id),
    };
  } catch {
    // Parser messages can contain token fragments; nothing is reported.
    return null;
  }
}

export type LiveCodexEmail =
  | { state: 'absent' }
  | { state: 'unverified' }
  | { state: 'email'; email: string };

/**
 * The live native login's email, read fresh (no cache, no registry metadata):
 * `absent` when there is no login file, `unverified` when one exists but its
 * identity cannot be read.
 */
export async function readLiveCodexEmail(authPath: string): Promise<LiveCodexEmail> {
  try {
    await fs.lstat(authPath);
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === 'ENOENT'
      ? { state: 'absent' }
      : { state: 'unverified' };
  }
  try {
    const parsed = (await readAuthFile(authPath)) as AuthTokens | null;
    const token = parsed?.tokens?.id_token;
    if (typeof token !== 'string' || !hasStructurallyValidIdToken(token)) {
      return { state: 'unverified' };
    }
    const email = decodeIdToken(token).email;
    return email ? { state: 'email', email } : { state: 'unverified' };
  } catch {
    return { state: 'unverified' };
  }
}

/** Case-insensitive equality of two present strings (emails). */
export function sameText(a: string | null | undefined, b: string | null | undefined): boolean {
  return typeof a === 'string' && typeof b === 'string' && a.toLowerCase() === b.toLowerCase();
}

/**
 * Whether a new login is a saved profile's own account and person (Codex's
 * activation identity rules): the new login needs a workspace claim, keeps
 * every known ChatGPT account id and any known person (user id, or subject
 * plus issuer), and has the known email. The saved login's binding is the
 * strongest evidence; the registry metadata covers a saved login that can no
 * longer be read.
 */
export function sameCodexIdentity(
  saved: CodexLoginIdentity | null,
  meta: Pick<CodexProfileMetadata, 'email' | 'account_id'>,
  fresh: CodexLoginIdentity
): boolean {
  const binding = fresh.binding;
  if (!binding) return false;
  const knownEmail = saved?.email ?? meta.email ?? null;
  if (knownEmail !== null && !sameText(knownEmail, fresh.email)) return false;
  const knownAccounts = [saved?.binding?.accountId, saved?.accountId, meta.account_id];
  if (knownAccounts.some((value) => typeof value === 'string' && value !== binding.accountId)) {
    return false;
  }
  return !saved?.binding || matchesCodexActivationIdentity(saved.binding, binding);
}

/**
 * True when the live native login in `codexHome` is (or may be) this email,
 * read fresh. A native login whose identity cannot be read is not proof that
 * it is another account.
 */
export async function liveCodexLoginIs(codexHome: string, email: string): Promise<boolean> {
  const live = await readLiveCodexEmail(path.join(codexHome, 'auth.json'));
  if (live.state === 'absent') return false;
  return live.state === 'unverified' || sameText(live.email, email);
}
