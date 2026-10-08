import type { CodexProfileRenewalProfileStatus } from '../../../src/codex-auth/codex-profile-renewal';
import {
  CODEX_RENEWAL_MESSAGES,
  type CodexRenewalReason,
  type CodexRenewalState,
} from '../../../src/codex-auth/codex-renewal-types';

/** A renewal entry as the renewal service reports it: fixed message, no secrets. */
export function renewalEntry(
  name: string,
  state: CodexRenewalState,
  reason: CodexRenewalReason,
  overrides: Partial<CodexProfileRenewalProfileStatus> = {}
): CodexProfileRenewalProfileStatus {
  return {
    name,
    state,
    reason,
    message: CODEX_RENEWAL_MESSAGES[reason],
    lastRenewedAt: null,
    lastAttemptAt: null,
    lastOutcome: null,
    nextAttemptAt: null,
    accessExpiresAt: null,
    ...overrides,
  };
}
