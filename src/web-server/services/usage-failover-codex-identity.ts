import { createHash } from 'crypto';
import { decodeCodexActivationIdentity } from '../../codex-auth/codex-activation-identity';
import { decodeJwtPayload } from '../../codex-auth/decode-id-token';
import { isRecord, usageEmail, type UsageIdentity } from './usage-failover-contract';

/** Match Python json.dumps(..., ensure_ascii=True, separators=(',', ':')). */
function asciiJson(value: unknown): string {
  return JSON.stringify(value).replace(
    /[\u007f-\uffff]/g,
    (character) => `\\u${character.charCodeAt(0).toString(16).padStart(4, '0')}`
  );
}

/** Exact account/person binding shared with failover_core.py, never an email-only join. */
export function codexUsageIdentity(tokens: unknown): UsageIdentity | null {
  if (!isRecord(tokens) || typeof tokens.id_token !== 'string') return null;
  const decoded = decodeCodexActivationIdentity(tokens.id_token, tokens.account_id);
  if (!decoded || !usageEmail(decoded.email)) return null;
  const access = typeof tokens.access_token === 'string' ? tokens.access_token.trim() : '';
  if (!access || access.length > 16384 || /[\u0000-\u0020\u007f]/.test(access)) return null;
  const payload = decodeJwtPayload(access);
  const claims = isRecord(payload?.['https://api.openai.com/auth'])
    ? payload['https://api.openai.com/auth']
    : {};
  for (const [key, expected] of [
    ['chatgpt_account_id', decoded.accountId],
    ['chatgpt_user_id', decoded.userId],
    ['user_id', decoded.userId],
  ] as const) {
    if (expected && key in claims && claims[key] !== expected) return null;
  }
  for (const [key, expected] of [
    ['sub', decoded.subject],
    ['iss', decoded.issuer],
  ] as const) {
    if (expected && payload && key in payload && payload[key] !== expected) return null;
  }
  const binding = [
    decoded.accountId,
    decoded.userId ?? null,
    decoded.subject ?? null,
    decoded.issuer ?? null,
    decoded.userId || (decoded.subject && decoded.issuer) ? null : decoded.email,
  ];
  return {
    kind: 'codex-principal-v1',
    key: createHash('sha256')
      .update(asciiJson(['codex', binding]))
      .digest('hex'),
  };
}
