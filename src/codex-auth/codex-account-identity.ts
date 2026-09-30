import * as fs from 'fs';
import { createLogger } from '../services/logging';
import { decodeIdToken } from './decode-id-token';
import type { CodexAccountIdentity } from './types';

const logger = createLogger('codex-auth:identity');

interface AuthJson {
  tokens?: {
    id_token?: string;
  };
}

/**
 * Read auth.json from disk and extract display-safe identity fields.
 * Returns {} on any error (missing file, bad JSON, missing token, decode failure).
 * Never throws.
 */
export function decodeAccountIdentity(authJsonPath: string): CodexAccountIdentity {
  try {
    const raw = fs.readFileSync(authJsonPath, 'utf8');
    const parsed = JSON.parse(raw) as AuthJson;
    const idToken = parsed?.tokens?.id_token;
    if (typeof idToken !== 'string' || idToken.length === 0) {
      return {};
    }
    return decodeIdToken(idToken);
  } catch (err) {
    // Parser messages can contain excerpts of auth.json, including tokens.
    const code = (err as NodeJS.ErrnoException | undefined)?.code;
    const msg = typeof code === 'string' && /^[A-Z_]+$/.test(code) ? code : 'invalid auth data';
    logger.warn(
      'codex-auth.identity.decode-failed',
      `Failed to decode account identity from ${authJsonPath}: ${msg}`
    );
    return {};
  }
}
