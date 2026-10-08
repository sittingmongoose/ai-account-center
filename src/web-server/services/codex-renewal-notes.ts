import type { CodexProfileRenewalProfileStatus } from '../../codex-auth/codex-profile-renewal';
import type { CodexRenewalReason } from '../../codex-auth/codex-renewal-types';
import type { BarSummaryRow } from '../routes/bar-routes';

const HOUR_MS = 60 * 60_000;
/** A retrying saved login is named on its row once its access token is this close to expiry. */
const RETRY_NOTE_WINDOW_MS = 48 * HOUR_MS;
/** Renewal lead is four days; a login it cannot touch is named from then on. */
const STUCK_NOTE_WINDOW_MS = 96 * HOUR_MS;
/** Skips that AAC cannot clear by itself, so the row says why. */
const STUCK_RENEWAL_REASONS: ReadonlySet<CodexRenewalReason> = new Set([
  'shared_family',
  'family_unverifiable',
  'unverifiable',
  'in_use',
  'process_scan_failed',
  'live_unverifiable',
  'unsafe_file',
]);

export interface CodexRenewalNote {
  /** Set only when OpenAI rejected the saved login. */
  status?: 'needs_sign_in';
  message: string;
}

function isoTime(value: string | null): { iso: string; ms: number } | null {
  const ms =
    typeof value === 'string' && /^\d{4}-\d{2}-\d{2}T/.test(value) ? Date.parse(value) : NaN;
  return Number.isFinite(ms) ? { iso: new Date(ms).toISOString(), ms } : null;
}

/** `YYYY-MM-DD HH:MM UTC` for an ISO timestamp. */
function utcMinute(iso: string): string {
  return `${iso.slice(0, 10)} ${iso.slice(11, 16)} UTC`;
}

/**
 * The note a saved login's renewal state adds to its Codex row, or null to leave
 * the row as it was. Messages are fixed strings; only the expiry is formatted.
 */
export function codexRenewalNote(
  row: BarSummaryRow | undefined,
  entry: CodexProfileRenewalProfileStatus
): CodexRenewalNote | null {
  if (entry.state === 'failed') return { status: 'needs_sign_in', message: entry.message };
  if (row?.needsReauth && (entry.state === 'due' || entry.state === 'renewing')) {
    return { message: 'The saved login expired. AAC is renewing it automatically.' };
  }
  const expires = isoTime(entry.accessExpiresAt);
  if (expires === null) return null;
  const untilExpiry = expires.ms - Date.now();
  if (entry.state === 'retrying' && untilExpiry <= RETRY_NOTE_WINDOW_MS) {
    return { message: `${entry.message} The saved login expires ${utcMinute(expires.iso)}.` };
  }
  if (
    entry.state === 'skipped' &&
    STUCK_RENEWAL_REASONS.has(entry.reason) &&
    untilExpiry <= STUCK_NOTE_WINDOW_MS
  ) {
    return { message: entry.message };
  }
  return null;
}
