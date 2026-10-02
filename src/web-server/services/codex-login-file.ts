import { constants } from 'fs';
import fs from 'fs/promises';
import { decodeIdToken, hasStructurallyValidIdToken } from '../../codex-auth/decode-id-token';

const MAX_AUTH_BYTES = 1024 * 1024;

export interface CodexLoginIdentity {
  email: string;
  accountId: string | null;
  plan: string | null;
}

/** A complete Codex login: a structurally valid id token with an email, plus access and refresh tokens. */
export async function readCodexLogin(authPath: string): Promise<CodexLoginIdentity | null> {
  let handle: Awaited<ReturnType<typeof fs.open>> | undefined;
  try {
    const noFollow = process.platform === 'win32' ? 0 : constants.O_NOFOLLOW;
    handle = await fs.open(authPath, constants.O_RDONLY | noFollow);
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size > MAX_AUTH_BYTES) return null;
    const parsed = JSON.parse(await handle.readFile('utf8')) as {
      tokens?: { id_token?: unknown; access_token?: unknown; refresh_token?: unknown };
    };
    const token = parsed?.tokens?.id_token;
    if (typeof token !== 'string' || !hasStructurallyValidIdToken(token)) return null;
    const identity = decodeIdToken(token);
    if (
      !identity.email ||
      typeof parsed.tokens?.access_token !== 'string' ||
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
    };
  } catch {
    // Parser messages can contain token fragments; nothing is reported.
    return null;
  } finally {
    await handle?.close().catch(() => undefined);
  }
}
