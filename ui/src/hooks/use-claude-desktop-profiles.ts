import { useMutation, useQuery } from '@tanstack/react-query';
import { withApiBase } from '@/lib/api-client';

/** Launcher controls open an isolated desktop profile; they do not verify desktop sign-in. */
export interface ClaudeDesktopLauncher {
  launcherName: string;
  launcherPath?: string;
  startMenuPath?: string;
  profilePath?: string;
  isDefault?: boolean;
  canOpen?: boolean;
  launchUri?: string;
}

export interface ClaudeDesktopProfile {
  id?: string;
  email: string;
  mac?: ClaudeDesktopLauncher;
  windows?: ClaudeDesktopLauncher;
}

export interface ClaudeDesktopUsageProfile {
  id?: string;
  email: string;
  status: 'cached' | 'needs-sign-in' | 'unavailable';
  cached: true;
  fetchedAt: string;
  sampledAt: string | null;
  utilization: {
    fiveHour?: number;
    weekly?: number;
    weeklyOpus?: number;
    weeklySonnet?: number;
    extra?: number;
  };
}

export interface ClaudeDesktopUsageResponse {
  platform: 'mac' | 'windows';
  fetchedAt: string;
  profiles: ClaudeDesktopUsageProfile[];
}

export interface ClaudeDesktopProfilesResponse {
  profiles: ClaudeDesktopProfile[];
}

async function fetchClaudeDesktopProfiles(): Promise<ClaudeDesktopProfilesResponse> {
  const response = await fetch(withApiBase('/claude/desktop-profiles'));
  if (!response.ok) {
    throw new Error('Failed to load Claude desktop profiles');
  }
  return response.json() as Promise<ClaudeDesktopProfilesResponse>;
}

export function useClaudeDesktopProfiles() {
  return useQuery({
    queryKey: ['claude-desktop-profiles'],
    queryFn: fetchClaudeDesktopProfiles,
    refetchInterval: 60000,
  });
}

async function openClaudeDesktopProfile(
  id: string
): Promise<{ opened: true; id: string; platform: 'mac' }> {
  const response = await fetch(
    withApiBase(`/claude/desktop-profiles/${encodeURIComponent(id)}/open`),
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ platform: 'mac' }),
    }
  );
  if (!response.ok) {
    const body = (await response.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error || 'Unable to open the Claude launcher.');
  }
  return response.json() as Promise<{ opened: true; id: string; platform: 'mac' }>;
}

export function useOpenClaudeDesktopProfile() {
  return useMutation({ mutationFn: openClaudeDesktopProfile });
}

async function fetchClaudeDesktopUsage(
  platform: 'mac' | 'windows'
): Promise<ClaudeDesktopUsageResponse> {
  const response = await fetch(withApiBase(`/claude/desktop-profiles/usage?platform=${platform}`));
  if (!response.ok) throw new Error('Failed to load cached Claude subscription limits');
  return response.json() as Promise<ClaudeDesktopUsageResponse>;
}

export function useClaudeDesktopUsage(platform: 'mac' | 'windows') {
  return useQuery({
    queryKey: ['claude-desktop-usage', platform],
    queryFn: () => fetchClaudeDesktopUsage(platform),
    refetchInterval: 60000,
    retry: 1,
  });
}
