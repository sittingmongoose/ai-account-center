/**
 * The snapshot's view types and the rendering of `usage.md`: pure text production from an
 * `AgentUsageSnapshot`, with the publish-time `signInNow` as its only clock input. The rendered
 * files are a live external contract other agents read every minute -- their bytes are pinned
 * by the golden test -- so wording changes happen here and cannot touch the publish loop that
 * lives in agent-usage-snapshot.ts.
 */
import type { AgentUsageAccount, AgentUsageWindow } from './agent-usage-projection';
import type { BurnRate } from './agent-usage-history';

export interface SnapshotWindow extends AgentUsageWindow {
  burn: BurnRate | null;
}

export interface SnapshotAccount extends Omit<AgentUsageAccount, 'windows'> {
  windows: SnapshotWindow[];
}

/** One machine's Claude sign-in expiry report (see scripts/account-usage/signin_expiry.py). */
export interface SnapshotSignInProfile {
  name: string;
  provider: string;
  platform: string;
  /** The machine this report came from; set by the health service. */
  machine?: string;
  configDir?: string;
  accessExpiresAt?: string | null;
  refreshExpiresAt?: string | null;
  /** The first access expiry that falls after the refresh expiry; after this the login is dead. */
  loggedOutAt?: string | null;
  state: 'ok' | 'warn' | 'critical' | 'logged_out' | 'unavailable' | 'unsupported';
  fixCommand: string;
  /** Why a machine's report is unavailable; absent otherwise. */
  error?: string;
  /** Key NAMES a codex store exposes; values are never read. */
  fieldsAvailable?: string[];
  /** The account this sign-in belongs to (from the renewal runner). */
  email?: string | null;
  /** Renewal runner fields: whether it renews by itself, from when, and how the last one went. */
  autoRenew?: boolean;
  renewFrom?: string | null;
  lastRenewAt?: string | null;
  lastResult?: string | null;
  nextVerifyAt?: string | null;
  reverts?: number;
}

/** The renewal runner's summary: who it is, whether it is current and what needs a person. */
export interface SnapshotSignInRenewal {
  runner: string;
  generatedAt: string;
  stale: boolean;
  needsYou: Array<{ email: string; reason: string; how: string; since: string | null }>;
}

export interface AgentUsageSnapshot {
  version: 1;
  source: 'AI Account Center';
  generatedAt: string;
  /** How often the dashboard samples, so a reader can tell a late snapshot. */
  refreshSeconds: number;
  /** Older than this, the dashboard has stopped writing: treat the readings as stale. */
  staleAfterSeconds: number;
  accounts: SnapshotAccount[];
  /** Claude sign-in expiry per machine, worst first in the markdown; absent until first collected. */
  signIn?: SnapshotSignInProfile[];
  /** The sign-in renewal runner's summary; absent when no runner publishes. */
  signInRenewal?: SnapshotSignInRenewal;
}

function utc(iso: string): string {
  return iso.replace('T', ' ').replace(/:\d\d(\.\d+)?Z$/, ' UTC');
}

/** "2h 35m", "3d 4h", "45s"; for a time in the past, "now". */
export function span(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return 'now';
  const days = Math.floor(seconds / 86_400);
  const hours = Math.floor((seconds % 86_400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m`;
  return `${Math.floor(seconds)}s`;
}

function amount(value: number): string {
  return Number.isInteger(value) ? String(value) : value.toFixed(2);
}

function burnText(
  burn: BurnRate | null,
  now: number,
  usedPercent: number | null = null
): string | null {
  if (!burn || burn.pace === null || burn.paceMinutes === null) return null;
  const over =
    burn.paceMinutes < 60 ? `${burn.paceMinutes} min` : `${Math.round(burn.paceMinutes / 60)} h`;
  const hour =
    burn.paceMinutes < 60 && burn.lastHour !== null ? `; ${amount(burn.lastHour)}%/h over 1 h` : '';
  const pace = `${amount(burn.pace)}%/h over the last ${over}${hour}`;
  if (burn.pace < 0) return `usage falling ${pace}`;
  if (!burn.projectedFullAt) return `burning ${pace}`;
  if (Date.parse(burn.projectedFullAt) <= now) {
    // "At its limit" is a measured 100%; a projection that has just come due is not.
    if (usedPercent !== null && usedPercent >= 100)
      return `at its limit${burn.runsOutBeforeReset === true ? ' until the reset' : ''} (${pace})`;
    return `reaching its limit about now at this pace (${pace})`;
  }
  const full = `full in ${span((Date.parse(burn.projectedFullAt) - now) / 1000)}`;
  const reset =
    burn.runsOutBeforeReset === true
      ? ' (before the reset)'
      : burn.runsOutBeforeReset === false
        ? ' (after the reset)'
        : '';
  return `burning ${pace}, ${full}${reset}`;
}

function windowText(window: SnapshotWindow, now: number): string {
  const parts: string[] = [];
  if (window.unlimited === true) parts.push('unlimited');
  else if (window.usedPercent !== null) {
    const left =
      window.remainingPercent !== null ? `, ${amount(window.remainingPercent)}% left` : '';
    parts.push(`${amount(window.usedPercent)}% used${left}`);
  }
  const unit = window.unit ? ` ${window.unit}` : '';
  if (window.remaining !== null && window.limit !== null)
    parts.push(`${amount(window.remaining)} of ${amount(window.limit)}${unit} left`);
  else if (window.remaining !== null) parts.push(`${amount(window.remaining)}${unit} left`);
  else if (window.used !== null && window.limit !== null)
    parts.push(`${amount(window.used)} of ${amount(window.limit)}${unit} used`);
  else if (window.used !== null) parts.push(`${amount(window.used)}${unit} used`);
  if (window.resetAt)
    parts.push(
      window.resetPassed
        ? `reset at ${utc(window.resetAt)} already passed, new reading pending`
        : `resets ${utc(window.resetAt)} (in ${span((Date.parse(window.resetAt) - now) / 1000)})`
    );
  if (window.expiresAt) parts.push(`expires ${utc(window.expiresAt)}`);
  if (!window.resetPassed) {
    const burn = burnText(window.burn, now, window.usedPercent);
    if (burn) parts.push(burn);
  }
  if (parts.length === 0) parts.push('no number reported');
  if (window.cached)
    parts.push(
      window.sampledAt
        ? `earlier reading from ${span((now - Date.parse(window.sampledAt)) / 1000)} ago`
        : 'earlier reading, time unknown'
    );
  const pool = window.pool ? ` [${window.pool}]` : '';
  return `${window.label}${pool}: ${parts.join(', ')}`;
}

const STATUS_WORDS: Record<AgentUsageAccount['status'], string> = {
  ok: 'current',
  cached: 'cached earlier reading',
  unavailable: 'no current reading',
  error: 'reading failed',
  needs_sign_in: 'needs sign-in',
};

function hostName(account: AgentUsageAccount): string {
  return { ubuntu: 'Ubuntu VM', nas1: 'Nas1', mac: 'Mac', windows: 'Windows' }[account.host];
}

/** The same snapshot in plain words, one provider section and one line per window. */
const SIGNIN_STATE_WORDS: Record<string, string> = {
  ok: 'healthy',
  logged_out: 'already logged out',
  unavailable: 'not readable on this machine',
  unsupported: 'no expiry published',
};

/** The sign-in section, worst state first; a machine with nothing readable still names itself. */
function renderSignInMarkdown(
  machines: { machines?: SnapshotSignInProfile[] },
  now: number
): string[] {
  const lines: string[] = [];
  const profiles = (machines.machines ?? []).map((row) => ({
    ...row,
    machine: row.machine ?? row.platform ?? 'unknown',
    // Last line before UTF-8 bytes: production rows are screened at the
    // publish boundary, but a command carrying an unpaired surrogate
    // would be rewritten to U+FFFD here and aim at another directory.
    fixCommand:
      row.fixCommand &&
      /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/.test(row.fixCommand)
        ? ''
        : row.fixCommand,
  }));
  const rank: Record<string, number> = {
    logged_out: 0,
    critical: 1,
    warn: 2,
    unavailable: 3,
    unsupported: 4,
    ok: 5,
  };
  profiles.sort((a, b) => (rank[a.state] ?? 9) - (rank[b.state] ?? 9));
  for (const row of profiles) {
    // The words carry the actual known time and never a threshold: a
    // nonrenewable access token can be critical with eight hours left, and
    // a refresh-bound warning can sit thirty hours out. Severity only
    // decides the worst-first order. A logged-out row never names a future
    // end: the stored times of emptied tokens are remnants, not events.
    const endedAt =
      row.loggedOutAt !== null && row.loggedOutAt !== undefined && row.loggedOutAt !== ''
        ? Date.parse(row.loggedOutAt)
        : NaN;
    const namesEnd = !Number.isNaN(endedAt) && row.state !== 'logged_out';
    // The same crossed-deadline recheck the CLI reader makes: a live row
    // rendered after its deadline is a past reading awaiting its next
    // sample, never a healthy future end.
    const live = row.state === 'ok' || row.state === 'warn' || row.state === 'critical';
    const refreshAt =
      !row.loggedOutAt && row.refreshExpiresAt ? Date.parse(row.refreshExpiresAt) : NaN;
    const crossedEnd = live && !Number.isNaN(endedAt) && endedAt <= now;
    const crossedRefresh = live && !Number.isNaN(refreshAt) && refreshAt <= now;
    let word = SIGNIN_STATE_WORDS[row.state] ?? row.state;
    if (crossedEnd)
      word = `ended ${utc(row.loggedOutAt as string)} (past its deadline; awaiting recheck)`;
    else if (crossedRefresh)
      word = `refresh expired ${utc(row.refreshExpiresAt as string)} (awaiting recheck)`;
    else if (row.state === 'warn' || row.state === 'critical') {
      if (namesEnd) word = `ends ${utc(row.loggedOutAt as string)}`;
      else if (row.refreshExpiresAt) word = `refresh expires ${utc(row.refreshExpiresAt)}`;
      else if (
        row.accessExpiresAt === null ||
        row.accessExpiresAt === undefined ||
        row.accessExpiresAt === ''
      )
        word = 'no access token in the store; the next run renews';
      else word = 'exact end unknown';
    }
    const detail = (): string => {
      if (row.state === 'logged_out' && row.autoRenew)
        return 'logged out; the renewal runner signs it in again on its next run';
      if (row.state === 'logged_out')
        return (
          `${row.loggedOutAt && !Number.isNaN(endedAt) && endedAt <= now ? `sign-in ended ${utc(row.loggedOutAt)}` : 'tokens are gone or expired'}` +
          (row.fixCommand ? `; re-login: \`${row.fixCommand}\`` : '')
        );
      if (row.state === 'unavailable')
        // The state word already says "not readable": the detail only adds
        // the cause.
        return row.error ? `(${row.error})` : 'reason unknown';
      if (row.state === 'unsupported') return `no login end time is published by ${row.provider}`;
      if (crossedEnd || crossedRefresh)
        return row.fixCommand ? `re-login: \`${row.fixCommand}\`` : '';
      if (row.autoRenew)
        return [
          row.refreshExpiresAt
            ? `sign-in ends ${utc(row.refreshExpiresAt)}`
            : 'sign-in end not published',
          row.renewFrom ? `renews by itself from ${utc(row.renewFrom)}` : 'renews by itself',
          row.lastResult === 'renewed' && row.lastRenewAt
            ? `last renewed ${utc(row.lastRenewAt)}`
            : '',
          row.lastResult && row.lastResult !== 'renewed'
            ? `last attempt: ${row.lastResult.replace(/_/g, ' ')}`
            : '',
        ]
          .filter(Boolean)
          .join('; ');
      if (row.state === 'ok')
        // Healthy rows state the end without prescribing a re-login; the
        // warn/critical/logged-out words carry the fix command.
        return namesEnd
          ? `sign-in ends ${utc(row.loggedOutAt as string)}`
          : row.refreshExpiresAt
            ? `refresh expires ${utc(row.refreshExpiresAt)}`
            : 'no exact end published yet';
      // warn/critical: the time is already in the word; the detail only
      // advises, and a missing access token renews on the next run rather
      // than needing a re-login.
      if (
        row.state === 'warn' &&
        (row.accessExpiresAt === null ||
          row.accessExpiresAt === undefined ||
          row.accessExpiresAt === '')
      )
        return 'the next run renews this login';
      if (row.fixCommand)
        return namesEnd
          ? `re-login before then: \`${row.fixCommand}\``
          : `re-login when the window ends: \`${row.fixCommand}\``;
      return namesEnd ? 're-login before then' : 're-login when the window ends';
    };
    const detailText = detail();
    const who = row.email ? ` (${row.email})` : '';
    lines.push(
      `- **${row.provider} · ${row.machine}/${row.name}**${who}: ${word}.${detailText ? ` ${detailText}` : ''}`
    );
  }
  return lines;
}

/** Who keeps the sign-ins alive, whether it is running, and what only a person can do. */
function renderRenewalMarkdown(renewal: SnapshotSignInRenewal | undefined, now: number): string[] {
  if (!renewal) return [];
  const lines: string[] = [];
  if (renewal.stale)
    lines.push(
      `**The sign-in renewal runner on ${renewal.runner} last reported ${utc(renewal.generatedAt)}; ` +
        'renewals are not happening until it runs again.**',
      ''
    );
  else
    lines.push(
      `Claude Code sign-ins renew by themselves (runner on ${renewal.runner}, ` +
        `last run ${span(Math.max(0, Math.round((now - Date.parse(renewal.generatedAt)) / 1000)))} ago).`,
      ''
    );
  for (const need of renewal.needsYou)
    lines.push(
      `- **NEEDS YOU · ${need.email}**: ${need.how || need.reason.replace(/_/g, ' ')}` +
        (need.since ? ` (since ${utc(need.since)})` : '')
    );
  if (renewal.needsYou.length > 0) lines.push('');
  return lines;
}

export function renderSnapshotMarkdown(
  snapshot: AgentUsageSnapshot,
  signInNow: number = Date.parse(snapshot.generatedAt)
): string {
  const now = Date.parse(snapshot.generatedAt);
  const lines = [
    '# AI Account Center usage',
    '',
    `Snapshot ${utc(snapshot.generatedAt)}. The AI Account Center dashboard rewrites it about every ` +
      `${span(snapshot.refreshSeconds)}; if it is more than ${span(snapshot.staleAfterSeconds)} old, ` +
      "the dashboard has stopped updating it. Percentages and amounts are the providers' own; a " +
      'missing number was not reported, not zero. Burn rates come from measured readings since ' +
      'the window last reset. Machine-readable: usage.json, 7-day readings: history.json, or run ' +
      'aac-usage in this folder.',
    '',
  ];
  const visible = snapshot.accounts.filter((account) => !account.hidden);
  if (visible.length === 0) lines.push('No account readings yet.', '');
  let provider: string | null = null;
  for (const account of visible) {
    if (account.provider !== provider) {
      provider = account.provider;
      lines.push(`## ${account.providerLabel}`, '');
    }
    const who = [
      account.email && account.email !== account.label ? account.email : null,
      account.profile && account.profile !== account.label ? `profile ${account.profile}` : null,
    ].filter((part): part is string => !!part);
    const facts = [account.plan, hostName(account), account.active ? 'active' : null].filter(
      (fact): fact is string => !!fact
    );
    const age =
      account.ageSeconds === null
        ? 'sample time unknown'
        : account.ageSeconds < 1
          ? 'sampled just now'
          : `sampled ${span(account.ageSeconds)} ago`;
    lines.push(
      `- **${account.label}**${who.length ? ` (${who.join(', ')})` : ''}, ${facts.join(', ')}: ` +
        `${STATUS_WORDS[account.status]}, ${age}`
    );
    if (account.status === 'ok' || account.status === 'cached')
      for (const window of account.windows) lines.push(`  - ${windowText(window, now)}`);
    lines.push('');
  }
  const signInProfiles = snapshot.signIn ?? [];
  if (signInProfiles.length > 0) {
    lines.push('');
    lines.push('## Sign-in health');
    lines.push('');
    lines.push(...renderRenewalMarkdown(snapshot.signInRenewal, now));
    lines.push(
      // The sign-in section reads against render time when the publisher
      // supplies it: its deadlines are wall-clock ends, not sample facts.
      ...renderSignInMarkdown(
        { machines: signInProfiles },
        Number.isNaN(signInNow) ? Date.now() : signInNow
      )
    );
    lines.push('');
  }
  return `${lines.join('\n').trimEnd()}\n`;
}
