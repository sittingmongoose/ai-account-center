import { constants } from 'fs';
import fs from 'fs/promises';
import { createHash } from 'crypto';
import { getCcsDir } from '../../utils/config-manager';
import { signInCallback } from '../../shared/private-file-lease';
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

/** Bytes of a regular, size-limited file that is not a link; throws otherwise. */
async function readAuthFile(authPath: string): Promise<Buffer> {
  let handle: Awaited<ReturnType<typeof fs.open>> | undefined;
  try {
    const noFollow = process.platform === 'win32' ? 0 : constants.O_NOFOLLOW;
    handle = await fs.open(authPath, constants.O_RDONLY | noFollow);
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size > MAX_AUTH_BYTES) throw new ConfigError('Not a login file.');
    const bytes = await handle.readFile();
    if (bytes.length > MAX_AUTH_BYTES) throw new ConfigError('Not a login file.');
    return bytes;
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
  return (await readLogin(authPath, true))?.identity ?? null;
}

/**
 * Whose login a file holds, from its id token alone: an expired or half-written
 * login that needs signing in again still names its account. Null when the id
 * token cannot be read.
 */
export async function readCodexLoginIdentity(authPath: string): Promise<CodexLoginIdentity | null> {
  return (await readLogin(authPath, false))?.identity ?? null;
}

export function readCodexLoginSnapshot(authPath: string) {
  return readLogin(authPath, true);
}

async function readLogin(
  authPath: string,
  complete: boolean
): Promise<{ identity: CodexLoginIdentity; bytes: Buffer } | null> {
  try {
    const bytes = await readAuthFile(authPath);
    const native = signInCallback(getCcsDir(), { action: 'codex-login', path: authPath, complete });
    if (
      native?.status === 'ok' &&
      typeof native.sha256 === 'string' &&
      native.identity &&
      typeof native.identity === 'object'
    ) {
      const digest = createHash('sha256').update(bytes).digest('hex');
      if (digest === native.sha256) {
        return { identity: native.identity as CodexLoginIdentity, bytes };
      }
    }
    const parsed = JSON.parse(bytes.toString('utf8')) as AuthTokens | null;
    const token = parsed?.tokens?.id_token;
    if (typeof token !== 'string' || !hasStructurallyValidIdToken(token)) return null;
    const identity = decodeIdToken(token);
    if (!identity.email) return null;
    if (
      complete &&
      (typeof parsed?.tokens?.access_token !== 'string' ||
        !parsed.tokens.access_token ||
        typeof parsed.tokens?.refresh_token !== 'string' ||
        !parsed.tokens.refresh_token)
    ) {
      return null;
    }
    return {
      identity: {
        email: identity.email,
        accountId: identity.account_id ?? null,
        plan: identity.plan_type ?? null,
        binding: decodeCodexActivationIdentity(token, parsed?.tokens?.account_id),
      },
      bytes,
    };
  } catch {
    // Parser messages can contain token fragments; nothing is reported.
    return null;
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
  const native = signInCallback(getCcsDir(), {
    action: 'codex-same-identity',
    saved,
    metaEmail: meta.email ?? null,
    metaAccount: meta.account_id ?? null,
    fresh,
  });
  if (native?.status === 'ok' && typeof native.same === 'boolean') return native.same;
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
