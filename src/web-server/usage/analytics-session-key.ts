import { createHash } from 'crypto';
import type { AccountAnalyticsActivityProvider } from '../services/account-analytics-types';

/**
 * The key one analytics session is counted and published under: a truncated digest of the tool and its raw session
 * id, so a session groups across hosts and readers while no id, path or directory reaches the page or the snapshot
 * on disk. The remote helper derives the same value on the host it reads
 * (`scripts/analytics-remote/analytics_usage_remote.py` `_session_key`), which is why a session copied between the
 * Mac and Windows counts once; keep the two in step.
 */
const SESSION_KEY_NAMESPACE = 'aac-session-v1';
const SESSION_KEY_LENGTH = 16;
const SESSION_KEY_SHAPE = /^[0-9a-f]{16}$/;

export function analyticsSessionKey(
  tool: AccountAnalyticsActivityProvider,
  sessionId: string
): string {
  return createHash('sha256')
    .update(`${SESSION_KEY_NAMESPACE}:${tool}:${sessionId}`)
    .digest('hex')
    .slice(0, SESSION_KEY_LENGTH);
}

/**
 * The key a session row is published under. The activity readers hash at ingest, so a row normally arrives already
 * keyed and passes through unchanged; anything else (a legacy retained row, a hand-built one) is hashed here, so no
 * raw session id can reach the page whichever reader produced the row.
 */
export function publishedSessionKey(
  tool: AccountAnalyticsActivityProvider,
  sessionId: string
): string {
  return SESSION_KEY_SHAPE.test(sessionId) ? sessionId : analyticsSessionKey(tool, sessionId);
}
