import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { withApiBase } from '@/lib/api-client';
import type { CodexAutomaticSwitchStatus } from '@/hooks/use-codex-auth-profiles';

export type DashboardProvider =
  | 'codex'
  | 'claude'
  | 'antigravity'
  | 'muse'
  | 'cursor'
  | 'kimi-code'
  | 'qwen'
  | 'zai'
  | 'opencode-go';
export type DashboardPlatform = 'mac' | 'windows';
export interface DashboardUsageWindow {
  key: string;
  label: string;
  usedPercent: number | null;
  remainingPercent: number | null;
  resetAt: string | null;
  windowMinutes: number | null;
  used: number | null;
  limit: number | null;
  unit: string | null;
}
export interface DashboardAccount {
  id: string;
  provider: DashboardProvider;
  providerLabel: string;
  label: string;
  email: string | null;
  plan: string | null;
  platform: 'ubuntu' | 'mac' | 'windows';
  source: string;
  status: 'ok' | 'cached' | 'unavailable' | 'error' | 'needs_sign_in';
  message: string | null;
  fetchedAt: string | null;
  sampledAt: string | null;
  isActive: boolean;
  windows: DashboardUsageWindow[];
  capabilities: {
    codexProfile: string | null;
    claudeProfileId: string | null;
    claudePlatforms: DashboardPlatform[];
  };
}
export interface AccountsDashboardResponse {
  schemaVersion: 1;
  updatedAt: string;
  accounts: DashboardAccount[];
  codexAutoSwitch: CodexAutomaticSwitchStatus;
}

async function fetchAccountsDashboard(
  platform: DashboardPlatform,
  refresh = false
): Promise<AccountsDashboardResponse> {
  const response = await fetch(
    withApiBase(`/accounts/dashboard?platform=${platform}&refresh=${refresh}`)
  );
  if (!response.ok) throw new Error('Unable to refresh account usage.');
  const data = (await response.json()) as AccountsDashboardResponse;
  if (data.schemaVersion !== 1 || !Array.isArray(data.accounts)) {
    throw new Error('Account usage returned an unsupported response.');
  }
  return data;
}

export function useAccountsDashboard(platform: DashboardPlatform) {
  return useQuery({
    queryKey: ['accounts-dashboard', platform],
    queryFn: () => fetchAccountsDashboard(platform),
    refetchInterval: 60_000,
    staleTime: 30_000,
    retry: 1,
  });
}

export function useRefreshAccountsDashboard(platform: DashboardPlatform) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => fetchAccountsDashboard(platform, true),
    onMutate: () => queryClient.cancelQueries({ queryKey: ['accounts-dashboard', platform] }),
    onSuccess: async (data) => {
      await queryClient.cancelQueries({ queryKey: ['accounts-dashboard', platform] });
      queryClient.setQueryData(['accounts-dashboard', platform], data);
    },
  });
}
