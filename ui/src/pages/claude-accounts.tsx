import { useState } from 'react';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Apple, Copy, Loader2, Monitor, RefreshCw } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { Card, CardContent } from '@/components/ui/card';
import { useClaudeDesktopProfiles } from '@/hooks/use-claude-desktop-profiles';

type DesktopPlatform = 'mac' | 'windows';

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

  return (
    <div className="mx-auto w-full max-w-6xl space-y-6 p-4 sm:p-6">
      <header className="space-y-2">
        <h1 className="text-2xl font-semibold tracking-tight">{t('claudeAccountsPage.title')}</h1>
        <p className="text-sm text-muted-foreground">{t('claudeAccountsPage.instruction')}</p>
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
          <Button variant="outline" size="sm" disabled={isFetching} onClick={() => refetch()}>
            <RefreshCw className="mr-2 h-4 w-4" />
            {t('claudeAccountsPage.refresh')}
          </Button>
        </div>

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
            <div className="space-y-3" aria-label={t('claudeAccountsPage.accountList')}>
              {data.profiles.map((profile) => {
                const launcher = profile[platform];
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
                      </div>
                      <div className="min-w-0 space-y-2">
                        {launcher ? (
                          <>
                            <p className="text-sm">
                              {t('claudeAccountsPage.openLauncher', {
                                name: launcher.launcherName,
                              })}
                            </p>
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
    </div>
  );
}
