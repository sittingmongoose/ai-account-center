/** Token handling for one renewal: account checks on the new tokens and the merged file. */
import {
  decodeCodexActivationIdentity,
  matchesCodexActivationIdentity,
} from './codex-activation-identity';
import { fingerprintCodexLogin, type CodexLoginFingerprint } from './codex-login-family';
import type { CodexRenewalProfileSnapshot } from './codex-renewal-planner';
import type { CodexRenewalReason } from './codex-renewal-types';
import { decodeIdToken, hasStructurallyValidIdToken } from './decode-id-token';

export function intersects<T>(left: ReadonlySet<T>, right: ReadonlySet<T>): boolean {
  for (const value of left) if (right.has(value)) return true;
  return false;
}

/** Merge the new tokens into the parsed file; every other key is preserved. */
export function mergeRefreshedTokens(
  parsed: Record<string, unknown>,
  tokens: { idToken?: string; accessToken: string; refreshToken?: string },
  now: number
): Buffer {
  const saved = parsed.tokens as Record<string, unknown>;
  const merged = {
    ...parsed,
    tokens: {
      ...saved,
      ...(tokens.idToken ? { id_token: tokens.idToken } : {}),
      access_token: tokens.accessToken,
      ...(tokens.refreshToken ? { refresh_token: tokens.refreshToken } : {}),
    },
    last_refresh: new Date(now).toISOString(),
  };
  return Buffer.from(JSON.stringify(merged, null, 2), 'utf8');
}

/** The new login must stay the saved account: same binding, same access-token workspace. */
export function checkRefreshedIdentity(
  profile: CodexRenewalProfileSnapshot,
  tokens: { idToken?: string; accessToken: string },
  now: number
): { reason: CodexRenewalReason | null; fingerprint: CodexLoginFingerprint | null } {
  const saved = profile.parsed?.tokens as Record<string, unknown>;
  const before = profile.fingerprint as CodexLoginFingerprint;
  const binding = before.binding;
  if (!binding) return { reason: 'identity_mismatch', fingerprint: null };
  if (tokens.idToken) {
    const fresh = decodeCodexActivationIdentity(tokens.idToken, saved.account_id);
    if (
      !hasStructurallyValidIdToken(tokens.idToken) ||
      !decodeIdToken(tokens.idToken).email ||
      !fresh ||
      !matchesCodexActivationIdentity(binding, fresh)
    ) {
      return { reason: 'identity_mismatch', fingerprint: null };
    }
  }
  const after = fingerprintCodexLogin({
    tokens: {
      id_token: tokens.idToken ?? saved.id_token,
      access_token: tokens.accessToken,
      account_id: saved.account_id,
    },
  });
  if (!after) return { reason: 'invalid_response', fingerprint: null };
  if (after.accessAccountId !== null && after.accessAccountId !== binding.accountId) {
    return { reason: 'identity_mismatch', fingerprint: null };
  }
  if (after.accessExpiresAt !== null && after.accessExpiresAt <= now) {
    return { reason: 'invalid_response', fingerprint: null };
  }
  return { reason: null, fingerprint: after };
}
