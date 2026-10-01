import { Apple, ExternalLink, Loader2, Monitor } from 'lucide-react';
import { toast } from 'sonner';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import {
  useClaudeDesktopProfiles,
  useOpenClaudeDesktopProfile,
} from '@/hooks/use-claude-desktop-profiles';
import type { DashboardAccount, DashboardPlatform } from '@/hooks/use-accounts-dashboard';
import { usePrivacy, PRIVACY_BLUR_CLASS } from '@/contexts/privacy-context';
import { cn } from '@/lib/utils';
import { AccountStatusBadge, DashboardUsage } from './dashboard-usage';

export function ClaudeHomePanel({
  platform,
  onPlatformChange,
  accounts,
  isUsageLoading,
  usageError,
}: {
  platform: DashboardPlatform;
  onPlatformChange: (platform: DashboardPlatform) => void;
  accounts: DashboardAccount[];
  isUsageLoading: boolean;
  usageError: boolean;
}) {
  const profiles = useClaudeDesktopProfiles();
  const openProfile = useOpenClaudeDesktopProfile();
  const { privacyMode } = usePrivacy();
  return (
    <section id="claude" className="scroll-mt-24 space-y-4" aria-labelledby="claude-heading">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="space-y-1">
          <div className="flex items-center gap-2">
            <h2 id="claude-heading" className="text-xl font-semibold tracking-tight">
              Claude
            </h2>
            <Badge variant="secondary">Manual profiles</Badge>
          </div>
          <p className="text-sm text-muted-foreground">
            Open a saved profile on your computer. Profiles can stay open independently.
          </p>
        </div>
        <div
          className="flex rounded-lg border bg-card p-1"
          role="group"
          aria-label="Claude computer"
        >
          <Button
            size="sm"
            variant={platform === 'mac' ? 'secondary' : 'ghost'}
            aria-pressed={platform === 'mac'}
            onClick={() => onPlatformChange('mac')}
          >
            <Apple className="mr-1.5 h-4 w-4" />
            Mac
          </Button>
          <Button
            size="sm"
            variant={platform === 'windows' ? 'secondary' : 'ghost'}
            aria-pressed={platform === 'windows'}
            onClick={() => onPlatformChange('windows')}
          >
            <Monitor className="mr-1.5 h-4 w-4" />
            Windows
          </Button>
        </div>
      </div>
      {profiles.isLoading ? (
        <p role="status" className="text-sm text-muted-foreground">
          Loading Claude profiles…
        </p>
      ) : profiles.error ? (
        <div
          role="alert"
          className="flex flex-wrap items-center gap-3 rounded-xl border p-4 text-sm"
        >
          <span className="text-destructive">Unable to load Claude account launchers.</span>
          <Button size="sm" variant="outline" onClick={() => profiles.refetch()}>
            Try again
          </Button>
        </div>
      ) : !profiles.data?.profiles.length ? (
        <p className="rounded-xl border p-4 text-sm text-muted-foreground">
          No Claude desktop profiles are configured.
        </p>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
          {profiles.data.profiles.map((profile) => {
            const launcher = profile[platform];
            const account = accounts.find(
              (entry) =>
                entry.capabilities.claudeProfileId === profile.id && entry.platform === platform
            );
            const opening = openProfile.isPending && openProfile.variables === profile.id;
            return (
              <Card
                key={profile.id ?? profile.email}
                className="gap-0 overflow-hidden border-border/70 py-0 shadow-sm"
              >
                <CardContent className="space-y-4 p-5">
                  <div className="flex items-start justify-between gap-2">
                    <div className="min-w-0 space-y-1">
                      <p className="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
                        {account?.plan || 'Claude account'}
                      </p>
                      <h3
                        className={cn(
                          'break-all text-sm font-semibold leading-5',
                          privacyMode && PRIVACY_BLUR_CLASS
                        )}
                      >
                        {profile.email}
                      </h3>
                    </div>
                    {account && <AccountStatusBadge status={account.status} />}
                  </div>
                  <DashboardUsage account={account} loading={isUsageLoading} error={usageError} />
                </CardContent>
                <div className="mt-auto border-t border-border/60 bg-muted/15 px-5 py-3">
                  {platform === 'windows' && launcher?.launchUri ? (
                    <Button
                      asChild
                      size="sm"
                      variant="outline"
                      className="w-full"
                      aria-label={`Open account ${profile.email} on Windows`}
                    >
                      <a href={launcher.launchUri}>
                        <ExternalLink className="mr-1.5 h-3.5 w-3.5" />
                        Open account
                      </a>
                    </Button>
                  ) : (
                    <Button
                      size="sm"
                      variant="outline"
                      className="w-full"
                      aria-label={`Open account ${profile.email} on ${platform === 'mac' ? 'Mac' : 'Windows'}`}
                      disabled={
                        platform !== 'mac' ||
                        !profile.id ||
                        !launcher?.canOpen ||
                        openProfile.isPending
                      }
                      onClick={() =>
                        profile.id &&
                        openProfile.mutate(profile.id, {
                          onSuccess: () => toast.success(`Opened Claude ${profile.email}.`),
                          onError: (error) => toast.error(error.message),
                        })
                      }
                    >
                      {opening ? (
                        <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />
                      ) : (
                        <ExternalLink className="mr-1.5 h-3.5 w-3.5" />
                      )}
                      {opening ? 'Opening…' : 'Open account'}
                    </Button>
                  )}
                  {!launcher && (
                    <p className="mt-2 text-xs text-muted-foreground">
                      No launcher on this computer.
                    </p>
                  )}
                </div>
              </Card>
            );
          })}
        </div>
      )}
    </section>
  );
}
