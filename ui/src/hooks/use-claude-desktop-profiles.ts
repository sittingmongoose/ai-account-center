import { useQuery } from '@tanstack/react-query';
import { withApiBase } from '@/lib/api-client';

/** Launcher metadata only; it does not verify desktop sign-in or control remote applications. */
export interface ClaudeDesktopLauncher {
  launcherName: string;
  launcherPath?: string;
  startMenuPath?: string;
  profilePath?: string;
  isDefault?: boolean;
}

export interface ClaudeDesktopProfile {
  email: string;
  mac?: ClaudeDesktopLauncher;
  windows?: ClaudeDesktopLauncher;
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
