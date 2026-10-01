/**
 * React hook for fetching codex-auth profile summary from
 * GET /api/codex/profiles. Returns the live shared-home account,
 * CCS launch selection/default, and profiles with decoded identity fields.
 *
 * Mirrors the useCodex pattern (use-codex.ts:70) with a 15s refetch
 * interval — dashboard polls are low-frequency; the server-side 5s
 * cache absorbs bursts.
 */

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { withApiBase } from '@/lib/api-client';
import type { AccountQuotaSummary } from '@/components/accounts/subscription-quota-display';

export interface CodexAuthProfileQuota extends AccountQuotaSummary {
  profileName: string;
}

export interface CodexAuthProfileQuotasResponse {
  profiles: CodexAuthProfileQuota[];
}

export type CodexAutomaticSwitchOutcome =
  | 'disabled'
  | 'scheduled'
  | 'healthy'
  | 'no_quota'
  | 'no_candidate'
  | 'waiting_idle'
  | 'switching'
  | 'switched'
  | 'error';

export interface CodexAutomaticSwitchStatus {
  enabled: boolean;
  thresholdPercent: number;
  pollIntervalSeconds: number;
  outcome: CodexAutomaticSwitchOutcome;
  message: string;
  lastCheckedAt?: string;
  lastSwitchedAt?: string;
  activationInProgress: boolean;
}

export interface CodexAuthProfileEntry {
  name: string;
  codexHome: string;
  email: string | null;
  plan: string | null;
  /** accountId returned by API but not displayed in UI per D6. */
  accountId: string | null;
  lastUsed: string | null;
  authValid: boolean;
}

export interface CodexAuthActiveProfile {
  name: string | null;
  source: 'default' | 'env' | 'explicit-codex-home';
  codexHome: string;
}

export interface CodexAuthProfilesResponse {
  active: CodexAuthActiveProfile | null;
  /** Actual account installed in the shared ~/.codex/auth.json. */
  activated: CodexActivatedAccount | null;
  default: string | null;
  profiles: CodexAuthProfileEntry[];
}

export interface CodexActivatedAccount {
  name: string | null;
  email: string;
  plan: string | null;
  codexHome: string;
}

interface CodexActivationResponse extends CodexActivatedAccount {
  success: true;
  name: string;
  previousEmail: string | null;
}

async function fetchCodexAuthProfiles(): Promise<CodexAuthProfilesResponse> {
  const res = await fetch(withApiBase('/codex/profiles'));
  if (!res.ok) {
    throw new Error('Failed to fetch Codex auth profiles');
  }
  return res.json() as Promise<CodexAuthProfilesResponse>;
}

export function useCodexAuthProfiles() {
  return useQuery({
    queryKey: ['codex-auth-profiles'],
    queryFn: fetchCodexAuthProfiles,
    refetchInterval: 15000,
  });
}

async function fetchCodexAuthProfileQuotas(): Promise<CodexAuthProfileQuotasResponse> {
  const res = await fetch(withApiBase('/codex/profiles/quotas'));
  if (!res.ok) {
    throw new Error('Failed to fetch Codex subscription usage');
  }
  return res.json() as Promise<CodexAuthProfileQuotasResponse>;
}

export function useCodexAuthProfileQuotas() {
  return useQuery({
    queryKey: ['codex-auth-profile-quotas'],
    queryFn: fetchCodexAuthProfileQuotas,
    refetchInterval: 60_000,
    staleTime: 60_000,
    retry: 1,
  });
}

async function fetchCodexAutomaticSwitch(): Promise<CodexAutomaticSwitchStatus> {
  const res = await fetch(withApiBase('/codex/profiles/auto-switch'));
  if (!res.ok) {
    throw new Error('Failed to fetch Codex automatic switching');
  }
  return res.json() as Promise<CodexAutomaticSwitchStatus>;
}

export function useCodexAutomaticSwitch() {
  return useQuery({
    queryKey: ['codex-automatic-switch'],
    queryFn: fetchCodexAutomaticSwitch,
    refetchInterval: 15_000,
    retry: 1,
  });
}

async function updateCodexAutomaticSwitch(enabled: boolean): Promise<CodexAutomaticSwitchStatus> {
  const res = await fetch(withApiBase('/codex/profiles/auto-switch'), {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ enabled }),
  });
  if (!res.ok) {
    const payload = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new Error(payload?.error || 'Failed to update Codex automatic switching.');
  }
  return res.json() as Promise<CodexAutomaticSwitchStatus>;
}

export function useUpdateCodexAutomaticSwitch() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: updateCodexAutomaticSwitch,
    onMutate: () => queryClient.cancelQueries({ queryKey: ['codex-automatic-switch'] }),
    onSuccess: async (result) => {
      // A poll started before the save must not replace the confirmed setting.
      await queryClient.cancelQueries({ queryKey: ['codex-automatic-switch'] });
      queryClient.setQueryData(['codex-automatic-switch'], result);
    },
  });
}

async function activateCodexAuthProfile(name: string): Promise<CodexActivationResponse> {
  const res = await fetch(withApiBase(`/codex/profiles/${encodeURIComponent(name)}/activate`), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: '{}',
  });
  if (!res.ok) {
    const payload = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new Error(payload?.error || 'Failed to activate the Codex account.');
  }
  return res.json() as Promise<CodexActivationResponse>;
}

export function useActivateCodexAuthProfile() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: activateCodexAuthProfile,
    onSuccess: async (result) => {
      queryClient.setQueryData<CodexAuthProfilesResponse>(['codex-auth-profiles'], (current) =>
        current
          ? {
              ...current,
              activated: {
                name: result.name,
                email: result.email,
                plan: result.plan,
                codexHome: result.codexHome,
              },
            }
          : current
      );
      await queryClient.invalidateQueries({ queryKey: ['codex-auth-profiles'] });
    },
    // A failed restart may still have changed auth.json; refresh actual live state.
    onError: () => queryClient.invalidateQueries({ queryKey: ['codex-auth-profiles'] }),
  });
}
