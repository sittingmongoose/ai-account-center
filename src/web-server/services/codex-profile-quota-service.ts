import { getCodexAuthProfilesSummary } from '../../codex-auth/codex-auth-dashboard-service';
import {
  getCachedCodexProfileQuotaRows,
  getCodexProfileQuotaRows,
} from '../usage/native-quota-collector';
import type { BarSummaryRow } from './account-dashboard-types';
import type { CodexProfileRenewalProfileStatus } from '../../codex-auth/codex-profile-renewal';
import { readingPredatesReset } from './account-window-reset';
import { readCodexRenewalEntries } from './codex-renewal-entries';

export interface CodexProfileQuotaWindow {
  key: string;
  label: string;
  usedPercent: number;
  resetsAt?: string;
  /** Present only when resetsAt has passed since the reading: usedPercent is then history. */
  resetPassed?: true;
}

export interface CodexProfileQuota {
  profileName: string;
  status: 'available' | 'reauth_required' | 'not_connected' | 'unavailable';
  windows: CodexProfileQuotaWindow[];
  fetchedAt?: string;
  message?: string;
}

export interface QuotaProfile {
  name: string;
  authValid: boolean;
}

export interface CodexProfileQuotaDeps {
  listProfiles?: () => Promise<QuotaProfile[]>;
  getRows?: (names: string[]) => Promise<BarSummaryRow[]>;
  getCachedRows?: (names: string[]) => BarSummaryRow[];
  /** Renewal state per saved Codex login (local files only); unreadable means no renewal notes. */
  getRenewalStatus?: () => Promise<{ profiles: CodexProfileRenewalProfileStatus[] }>;
  responseBudgetMs?: number;
  now?: () => number;
}

function buildQuota(
  profile: QuotaProfile,
  now: number,
  row?: BarSummaryRow,
  renewal?: CodexProfileRenewalProfileStatus
): CodexProfileQuota {
  if (!profile.authValid) {
    return {
      profileName: profile.name,
      status: 'not_connected',
      windows: [],
      message: 'No valid saved Codex login is available for this profile.',
    };
  }
  const windows = (row?.quotaWindows ?? []).map((window) => ({
    key: window.key,
    label:
      window.key === 'five_hour'
        ? '5-hour limit'
        : window.key === 'seven_day'
          ? 'Weekly limit'
          : window.label,
    usedPercent: window.usedPercent,
    ...(window.resetAt ? { resetsAt: window.resetAt } : {}),
    // A stale local reading was sampled at its session file's mtime, not at fetchedAt.
    ...(readingPredatesReset(window.resetAt, row?.staleAsOf ?? row?.fetchedAt, now)
      ? { resetPassed: true as const }
      : {}),
  }));
  // OpenAI rejected the saved login: say so even while its last reading still shows.
  if (renewal?.state === 'failed') {
    return {
      profileName: profile.name,
      status: 'reauth_required',
      windows: row?.quotaStatus === 'ok' ? windows : [],
      ...(row ? { fetchedAt: row.fetchedAt } : {}),
      message: renewal.message,
    };
  }
  if (row?.quotaStatus === 'ok' && windows.length > 0) {
    return { profileName: profile.name, status: 'available', windows, fetchedAt: row.fetchedAt };
  }
  if (row?.quotaStatus === 'error' && row.needsReauth) {
    return {
      profileName: profile.name,
      status: 'reauth_required',
      windows: [],
      fetchedAt: row.fetchedAt,
      message: 'The saved Codex login needs to be renewed.',
    };
  }
  return {
    profileName: profile.name,
    status: 'unavailable',
    windows: [],
    ...(row ? { fetchedAt: row.fetchedAt } : {}),
    message: row ? 'Codex usage is temporarily unavailable.' : 'Codex usage is being retrieved.',
  };
}

/** The owner reads behind one quota answer; composing them reads nothing more. */
export interface CodexProfileQuotaFacts {
  profiles: QuotaProfile[];
  rows: BarSummaryRow[];
  renewals: ReadonlyMap<string, CodexProfileRenewalProfileStatus>;
  now: number;
}

/** The saved profiles, collector rows (fresh within the budget, else cached) and renewal notes. */
export async function readCodexProfileQuotaFacts(
  deps: CodexProfileQuotaDeps = {}
): Promise<CodexProfileQuotaFacts> {
  const profiles = await (
    deps.listProfiles ?? (async () => (await getCodexAuthProfilesSummary()).profiles)
  )();
  const names = profiles.map((profile) => profile.name);
  if (names.length === 0)
    return { profiles: [], rows: [], renewals: new Map(), now: (deps.now ?? Date.now)() };

  const renewals = readCodexRenewalEntries(deps.getRenewalStatus);
  // Keep the dashboard responsive. The bounded collector continues filling all
  // saved profiles after this deadline, with its per-profile cache/coalescing.
  const pending = (deps.getRows ?? getCodexProfileQuotaRows)(names).catch(() => null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<null>((resolve) => {
    timer = setTimeout(() => resolve(null), deps.responseBudgetMs ?? 2500);
  });
  const freshRows = await Promise.race([pending, deadline]);
  clearTimeout(timer);
  const rows = freshRows ?? (deps.getCachedRows ?? getCachedCodexProfileQuotaRows)(names);
  const now = (deps.now ?? Date.now)();
  return { profiles, rows, renewals: await renewals, now };
}

/** The quota answer from its owner reads (pure; the M3 Rust service ports it). */
export function composeCodexProfileQuotas(facts: CodexProfileQuotaFacts): {
  profiles: CodexProfileQuota[];
} {
  const byProfile = new Map(facts.rows.map((row) => [row.profile, row]));
  return {
    profiles: facts.profiles.map((profile) =>
      buildQuota(profile, facts.now, byProfile.get(profile.name), facts.renewals.get(profile.name))
    ),
  };
}

/** Safe account-keyed DTO; authentication and provider calls never reach the response. */
export async function getCodexProfileQuotas(
  deps: CodexProfileQuotaDeps = {},
  compose: (
    facts: CodexProfileQuotaFacts
  ) =>
    | { profiles: CodexProfileQuota[] }
    | Promise<{ profiles: CodexProfileQuota[] }> = composeCodexProfileQuotas
): Promise<{ profiles: CodexProfileQuota[] }> {
  return compose(await readCodexProfileQuotaFacts(deps));
}
