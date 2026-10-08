/** Reason codes, states and plain ASCII messages for saved-login renewal. No secrets. */

export type CodexRenewalReason =
  // ok
  | 'fresh'
  | 'renewed'
  // due / renewing
  | 'due'
  | 'renewing'
  // retrying: transient, AAC tries again with backoff
  | 'transient'
  | 'invalid_response'
  | 'write_failed'
  // failed: OpenAI rejected the saved login; Sign in again restores it
  | 'dead'
  | 'identity_mismatch'
  // skipped
  | 'disabled'
  | 'no_login'
  | 'unsafe_file'
  | 'live'
  | 'live_unverifiable'
  | 'unverifiable'
  | 'process_scan_failed'
  | 'in_use'
  | 'shared_family'
  | 'family_unverifiable'
  | 'activation_busy'
  | 'changed';

export type CodexRenewalState = 'ok' | 'due' | 'renewing' | 'retrying' | 'failed' | 'skipped';

export const CODEX_RENEWAL_DEAD_REASONS: ReadonlySet<CodexRenewalReason> = new Set([
  'dead',
  'identity_mismatch',
]);

export const CODEX_RENEWAL_RETRY_REASONS: ReadonlySet<CodexRenewalReason> = new Set([
  'transient',
  'invalid_response',
  'write_failed',
]);

export function codexRenewalState(reason: CodexRenewalReason): CodexRenewalState {
  if (reason === 'fresh' || reason === 'renewed') return 'ok';
  if (reason === 'due' || reason === 'renewing') return reason;
  if (CODEX_RENEWAL_RETRY_REASONS.has(reason)) return 'retrying';
  if (CODEX_RENEWAL_DEAD_REASONS.has(reason)) return 'failed';
  return 'skipped';
}

export const CODEX_RENEWAL_MESSAGES: Record<CodexRenewalReason, string> = {
  fresh: 'The saved login is current. AAC renews it about 4 days before it expires.',
  renewed: 'AAC renewed the saved login.',
  due: 'The saved login is due for renewal at the next check.',
  renewing: 'AAC is renewing the saved login now.',
  transient: 'OpenAI could not be reached or was busy. AAC will try again automatically.',
  invalid_response: 'OpenAI sent an unexpected answer. AAC will try again automatically.',
  write_failed:
    'The renewed login could not be saved. AAC will try again; if OpenAI then rejects it, use Sign in again.',
  dead: 'OpenAI rejected this saved login. Use Sign in again to restore it.',
  identity_mismatch:
    'OpenAI answered for a different account, so nothing was saved. Use Sign in again to restore this login.',
  disabled: 'Automatic renewal of saved Codex logins is turned off (CCS_CODEX_RENEWAL=0).',
  no_login: 'This profile has no saved Codex login with a renewal token.',
  unsafe_file:
    'The saved login file is a link, too large or unreadable, so AAC does not rewrite it.',
  live: 'This is the login Codex is using now. Codex renews it itself.',
  live_unverifiable:
    'The current Codex login could not be read, so AAC cannot tell which saved login is live.',
  unverifiable:
    'This saved login carries no session details, so AAC cannot check it for copies elsewhere.',
  process_scan_failed: 'Running Codex apps could not be checked safely.',
  in_use: 'A running Codex app uses this saved profile folder directly and renews it itself.',
  shared_family:
    'Another Codex login on this machine is a copy of this saved login. Renewing one copy would sign the other out, so AAC leaves both alone. Sign in again on one of them to separate them.',
  family_unverifiable:
    'Another Codex login on this machine could not be checked, so AAC cannot rule out a shared copy.',
  activation_busy:
    'A Codex account switch, sign-in or removal was running. AAC will try again shortly.',
  changed: 'The saved login changed while AAC was renewing it, so AAC kept the new file.',
};

/** `CCS_CODEX_RENEWAL=0` (or false/off/no) turns renewal off. */
export function isCodexProfileRenewalEnabled(env: NodeJS.ProcessEnv = process.env): boolean {
  const value = env.CCS_CODEX_RENEWAL?.trim().toLowerCase();
  return !(value === '0' || value === 'false' || value === 'off' || value === 'no');
}
