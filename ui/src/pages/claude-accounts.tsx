import { useState } from 'react';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Apple, Copy, ExternalLink, Loader2, Monitor, RefreshCw } from 'lucide-react';
import type { TFunction } from 'i18next';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { Card, CardContent } from '@/components/ui/card';
import {
  useClaudeDesktopProfiles,
  useClaudeDesktopUsage,
  useOpenClaudeDesktopProfile,
} from '@/hooks/use-claude-desktop-profiles';
import type { ClaudeDesktopUsageProfile } from '@/hooks/use-claude-desktop-profiles';
import { SubscriptionQuotaDisplay } from '@/components/accounts/subscription-quota-display';
import type {
  AccountQuotaSummary,
  AccountQuotaWindow,
} from '@/components/accounts/subscription-quota-display';

type DesktopPlatform = 'mac' | 'windows';

function cachedQuota(usage: ClaudeDesktopUsageProfile, t: TFunction): AccountQuotaSummary {
  const labels = {
    fiveHour: 'fiveHour',
    weekly: 'weekly',
    weeklyOpus: 'weeklyOpus',
    weeklySonnet: 'weeklySonnet',
    extra: 'extra',
  } as const;
  const windows: AccountQuotaWindow[] = [];
  for (const key of Object.keys(labels) as (keyof typeof labels)[]) {
    const usedPercent = usage.utilization[key];
    if (typeof usedPercent === 'number') {
      windows.push({ key, label: t(`accountQuota.${labels[key]}`), usedPercent });
    }
  }
  return {
    status:
      usage.status === 'cached'
        ? 'available'
        : usage.status === 'needs-sign-in'
          ? 'not_connected'
          : 'unavailable',
    windows,
    fetchedAt: usage.fetchedAt,
    sampledAt: usage.sampledAt,
    message: usage.status === 'needs-sign-in' ? t('claudeAccountsPage.noUsageHistory') : undefined,
  };
}

function LauncherLocation({ label, path }: { label: string; path: string }) {
  const { t } = useTranslation();

  const copyPath = async () => {
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(path);
      } else {
        // LAN HTTP pages may not expose the Clipboard API; the text remains selectable.
        const field = document.createElement('textarea');
        field.value = path;
        field.style.position = 'fixed';
        field.style.opacity = '0';
        document.body.appendChild(field);
        field.select();
        try {
          if (!document.execCommand('copy')) throw new Error('Clipboard unavailable');
        } finally {
          field.remove();
        }
      }
      toast.success(t('claudeAccountsPage.copied'));
    } catch {
      toast.error(t('claudeAccountsPage.copyFailed'));
    }
  };

  return (
    <div className="flex min-w-0 items-start gap-2">
      <div className="min-w-0 flex-1">
        <p className="text-xs text-muted-foreground">{label}</p>
        <code className="select-text break-all text-xs">{path}</code>
      </div>
      <Button
        variant="ghost"
        size="icon"
        className="h-8 w-8 shrink-0"
        aria-label={t('claudeAccountsPage.copyLocation', { label })}
        onClick={copyPath}
      >
        <Copy className="h-4 w-4" />
      </Button>
    </div>
  );
}

export function ClaudeAccountsPage() {
  const { t } = useTranslation();
  const { data, isLoading, error, refetch, isFetching } = useClaudeDesktopProfiles();
  const [platform, setPlatform] = useState<DesktopPlatform>(() =>
    /Windows/i.test(navigator.userAgent) ? 'windows' : 'mac'
  );
  const usage = useClaudeDesktopUsage(platform);
  const openProfile = useOpenClaudeDesktopProfile();

  return (
    <div className="mx-auto w-full max-w-6xl space-y-6 p-4 sm:p-6">
      <header className="space-y-2">
        <h1 className="text-2xl font-semibold tracking-tight">{t('claudeAccountsPage.title')}</h1>
        <p className="text-sm text-muted-foreground">{t('claudeAccountsPage.instruction')}</p>
        <p className="text-sm text-muted-foreground">{t('accountQuota.explanation')}</p>
      </header>

      <Tabs value={platform} onValueChange={(value) => setPlatform(value as DesktopPlatform)}>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <TabsList>
            <TabsTrigger value="mac">
              <Apple className="mr-2 h-4 w-4" />
              Mac
            </TabsTrigger>
            <TabsTrigger value="windows">
              <Monitor className="mr-2 h-4 w-4" />
              Windows
            </TabsTrigger>
          </TabsList>
          <Button
            variant="outline"
            size="sm"
            disabled={isFetching || usage.isFetching}
            onClick={() => {
              void refetch();
              void usage.refetch();
            }}
          >
            <RefreshCw className="mr-2 h-4 w-4" />
            {t('claudeAccountsPage.refresh')}
          </Button>
        </div>
        <p className="mt-3 text-xs text-muted-foreground">
          {t('claudeAccountsPage.cachedUsageHint')}
        </p>
        {platform === 'windows' && (
          <p className="mt-2 text-xs text-muted-foreground">
            {t('claudeAccountsPage.browserOpenHint')}
          </p>
        )}

        <TabsContent value={platform} className="mt-6">
          {isLoading ? (
            <div className="flex items-center gap-2 py-6 text-muted-foreground" role="status">
              <Loader2 className="h-4 w-4 animate-spin" />
              {t('claudeAccountsPage.loading')}
            </div>
          ) : error ? (
            <p className="rounded-lg border p-4 text-sm text-destructive" role="alert">
              {t('claudeAccountsPage.loadError')}
            </p>
          ) : !data?.profiles.length ? (
            <p className="rounded-lg border p-4 text-sm text-muted-foreground">
              {t('claudeAccountsPage.empty')}
            </p>
          ) : (
            <div
              className="grid gap-4 xl:grid-cols-2"
              aria-label={t('claudeAccountsPage.accountList')}
            >
              {data.profiles.map((profile) => {
                const launcher = profile[platform];
                const profileUsage = usage.data?.profiles.find((entry) =>
                  profile.id && entry.id ? profile.id === entry.id : profile.email === entry.email
                );
                return (
                  <Card key={profile.email} className="gap-0 py-4">
                    <CardContent className="grid gap-3 px-4 sm:grid-cols-[minmax(0,1fr)_minmax(0,2fr)] sm:gap-6 sm:px-6">
                      <div className="min-w-0 space-y-2">
                        <h2 className="break-all text-sm font-semibold">{profile.email}</h2>
                        {launcher?.isDefault && (
                          <Badge variant="secondary">
                            {t('claudeAccountsPage.originalProfile')}
                          </Badge>
                        )}
                        <div>
                          {platform === 'windows' && launcher?.launchUri ? (
                            <Button
                              asChild
                              size="sm"
                              aria-label={t('claudeAccountsPage.openProfile', {
                                email: profile.email,
                              })}
                            >
                              <a href={launcher.launchUri}>
                                <ExternalLink className="mr-2 h-4 w-4" />
                                {t('claudeAccountsPage.openAccount')}
                              </a>
                            </Button>
                          ) : (
                            <Button
                              size="sm"
                              aria-label={t('claudeAccountsPage.openProfile', {
                                email: profile.email,
                              })}
                              disabled={
                                platform !== 'mac' ||
                                !profile.id ||
                                !launcher?.canOpen ||
                                openProfile.isPending
                              }
                              onClick={() =>
                                profile.id &&
                                openProfile.mutate(profile.id, {
                                  onSuccess: () =>
                                    toast.success(
                                      t('claudeAccountsPage.openSuccess', { email: profile.email })
                                    ),
                                  onError: (launchError) => toast.error(launchError.message),
                                })
                              }
                            >
                              {openProfile.isPending && openProfile.variables === profile.id && (
                                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                              )}
                              {openProfile.isPending && openProfile.variables === profile.id
                                ? t('claudeAccountsPage.opening')
                                : t('claudeAccountsPage.openAccount')}
                            </Button>
                          )}
                        </div>
                      </div>
                      <div className="min-w-0 space-y-2">
                        {launcher ? (
                          <>
                            <details className="space-y-2">
                              <summary className="cursor-pointer text-sm">
                                {t('claudeAccountsPage.launcherDetails')} · {launcher.launcherName}
                              </summary>
                              {launcher.launcherPath && (
                                <LauncherLocation
                                  label={t('claudeAccountsPage.launcherLocation')}
                                  path={launcher.launcherPath}
                                />
                              )}
                              {launcher.startMenuPath && (
                                <LauncherLocation
                                  label={t('claudeAccountsPage.startMenuLocation')}
                                  path={launcher.startMenuPath}
                                />
                              )}
                              {!launcher.launcherPath && !launcher.startMenuPath && (
                                <p className="text-xs text-muted-foreground">
                                  {t('claudeAccountsPage.findInstalledApp', {
                                    name: launcher.launcherName,
                                  })}
                                </p>
                              )}
                            </details>
                          </>
                        ) : (
                          <p className="text-sm text-muted-foreground">
                            {t('claudeAccountsPage.noLauncher', {
                              platform: platform === 'mac' ? 'Mac' : 'Windows',
                            })}
                          </p>
                        )}
                      </div>
                    </CardContent>
                    <div className="mt-4 border-t px-4 pt-4 sm:px-6">
                      <SubscriptionQuotaDisplay
                        quota={profileUsage ? cachedQuota(profileUsage, t) : undefined}
                        isLoading={usage.isLoading}
                        error={usage.error}
                        cached
                      />
                    </div>
                  </Card>
                );
              })}
            </div>
          )}
        </TabsContent>
      </Tabs>

      <p className="text-sm text-muted-foreground">
        {t('claudeAccountsPage.cliDescription')}{' '}
        <Link to="/accounts" className="font-medium text-foreground underline underline-offset-4">
          {t('claudeAccountsPage.cliLink')}
        </Link>
      </p>
      <p className="text-sm text-muted-foreground">
        <Link to="/analytics" className="font-medium text-foreground underline underline-offset-4">
          {t('accountQuota.recordedCliUsage')}
        </Link>
        {' — '}
        {t('accountQuota.recordedCliUsageDescription')}
      </p>
    </div>
  );
}
