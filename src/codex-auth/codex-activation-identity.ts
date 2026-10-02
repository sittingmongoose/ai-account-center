import { decodeIdToken, hasStructurallyValidIdToken } from './decode-id-token';

/** Private, local auth-file binding. Never include these principal fields in public DTOs. */
export interface CodexActivationIdentity {
  accountId: string;
  email: string;
  userId?: string;
  subject?: string;
  issuer?: string;
}

function canonicalId(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0 && value.trim() === value;
}

function optionalId(value: unknown): value is string | null | undefined {
  return value === undefined || value === null || canonicalId(value);
}

/**
 * Decode existing ChatGPT auth.json claims without a provider request. The
 * workspace claim is required; the legacy JWT-only workspace shape is accepted.
 * An explicit tokens.account_id must agree with that claim. As with the existing
 * decoder, this checks local consistency, not JWT signature or token validity.
 */
export function decodeCodexActivationIdentity(
  idToken: string,
  storedAccountId: unknown
): CodexActivationIdentity | null {
  if (!hasStructurallyValidIdToken(idToken)) return null;
  try {
    const payload = JSON.parse(
      Buffer.from(idToken.split('.')[1], 'base64url').toString('utf8')
    ) as {
      sub?: unknown;
      iss?: unknown;
      'https://api.openai.com/auth'?: unknown;
    };
    const auth = payload['https://api.openai.com/auth'];
    if (!auth || typeof auth !== 'object' || Array.isArray(auth)) return null;
    const claims = auth as {
      chatgpt_account_id?: unknown;
      chatgpt_user_id?: unknown;
      user_id?: unknown;
    };
    const accountId = claims.chatgpt_account_id;
    if (!canonicalId(accountId) || !optionalId(storedAccountId)) return null;
    if (storedAccountId !== undefined && storedAccountId !== null && storedAccountId !== accountId)
      return null;
    if (
      !optionalId(claims.chatgpt_user_id) ||
      !optionalId(claims.user_id) ||
      !optionalId(payload.sub) ||
      !optionalId(payload.iss)
    )
      return null;
    if (
      claims.chatgpt_user_id !== undefined &&
      claims.chatgpt_user_id !== null &&
      claims.user_id !== undefined &&
      claims.user_id !== null &&
      claims.chatgpt_user_id !== claims.user_id
    )
      return null;
    const email = decodeIdToken(idToken).email;
    if (!email) return null;
    const userId = claims.chatgpt_user_id ?? claims.user_id;
    return {
      accountId,
      email,
      ...(userId ? { userId } : {}),
      ...(payload.sub ? { subject: payload.sub } : {}),
      ...(payload.iss ? { issuer: payload.iss } : {}),
    };
  } catch {
    return null;
  }
}

/**
 * Match a known binding to its reread/alias without downgrading fields already
 * known. A workspace never substitutes for a person: known user/subject/issuer
 * must survive and agree. Legacy bindings still require workspace + exact email;
 * a retained user or issuer-bound subject permits a changed display email.
 */
export function matchesCodexActivationIdentity(
  expected: CodexActivationIdentity,
  actual: CodexActivationIdentity
): boolean {
  if (expected.accountId !== actual.accountId) return false;
  if (expected.userId && expected.userId !== actual.userId) return false;
  if (expected.subject && expected.subject !== actual.subject) return false;
  if (expected.issuer && expected.issuer !== actual.issuer) return false;
  if (expected.userId || (expected.subject && expected.issuer)) return true;
  return expected.email === actual.email;
}
