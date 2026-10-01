import { getCodexAuthProfilesSummary } from '../../codex-auth/codex-auth-dashboard-service';
import {
  getCachedCodexProfileQuotaRows,
  getCodexProfileQuotaRows,
} from '../usage/native-quota-collector';
import type { BarSummaryRow } from '../routes/bar-routes';

export interface CodexProfileQuotaWindow {
  key: string;
  label: string;
  usedPercent: number;
  resetsAt?: string;
}

export interface CodexProfileQuota {
  profileName: string;
  status: 'available' | 'reauth_required' | 'not_connected' | 'unavailable';
  windows: CodexProfileQuotaWindow[];
  fetchedAt?: string;
  message?: string;
}

interface QuotaProfile {
  name: string;
  authValid: boolean;
}

export interface CodexProfileQuotaDeps {
  listProfiles?: () => Promise<QuotaProfile[]>;
  getRows?: (names: string[]) => Promise<BarSummaryRow[]>;
  getCachedRows?: (names: string[]) => BarSummaryRow[];
  responseBudgetMs?: number;
}

function buildQuota(profile: QuotaProfile, row?: BarSummaryRow): CodexProfileQuota {
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
  }));
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

/** Safe account-keyed DTO; authentication and provider calls never reach the response. */
export async function getCodexProfileQuotas(
  deps: CodexProfileQuotaDeps = {}
): Promise<{ profiles: CodexProfileQuota[] }> {
  const profiles = await (
    deps.listProfiles ?? (async () => (await getCodexAuthProfilesSummary()).profiles)
  )();
  const names = profiles.map((profile) => profile.name);
  if (names.length === 0) return { profiles: [] };

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
  const byProfile = new Map(rows.map((row) => [row.profile, row]));
  return { profiles: profiles.map((profile) => buildQuota(profile, byProfile.get(profile.name))) };
}
