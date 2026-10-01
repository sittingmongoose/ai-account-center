import { ArrowRightLeft, Loader2, RefreshCw } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/switch';
import { SubscriptionQuotaDisplay } from '@/components/accounts/subscription-quota-display';
import { PRIVACY_BLUR_CLASS, usePrivacy } from '@/contexts/privacy-context';
import { cn } from '@/lib/utils';
import {
  useActivateCodexAuthProfile,
  useCodexAuthProfileQuotas,
  useCodexAuthProfiles,
  useCodexAutomaticSwitch,
  useUpdateCodexAutomaticSwitch,
} from '@/hooks/use-codex-auth-profiles';

export function CodexHomePanel() {
  const { t } = useTranslation();
  const { privacyMode } = usePrivacy();
  const profiles = useCodexAuthProfiles();
  const quotas = useCodexAuthProfileQuotas();
  const activation = useActivateCodexAuthProfile();
  const automaticSwitch = useCodexAutomaticSwitch();
  const updateAutomaticSwitch = useUpdateCodexAutomaticSwitch();
  const switchStatus = automaticSwitch.data;

  return (
    <section id="codex" aria-labelledby="codex-home-heading" className="scroll-mt-24 space-y-4">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div className="space-y-1">
          <div className="flex items-center gap-2">
            <h2 id="codex-home-heading" className="text-xl font-semibold tracking-tight">
              Codex
            </h2>
            {profiles.data && (
              <Badge variant="outline">{profiles.data.profiles.length} accounts</Badge>
            )}
          </div>
          <p className="text-sm text-muted-foreground">
            Use these accounts for Codex on Ubuntu. Switching waits until Codex is idle.
          </p>
        </div>
        <Button
          variant="ghost"
          size="sm"
          aria-label="Refresh Codex accounts and usage"
          disabled={profiles.isFetching || quotas.isFetching}
          onClick={() => {
            void profiles.refetch();
            void quotas.refetch();
          }}
        >
          <RefreshCw
            className={`mr-1.5 h-3.5 w-3.5 ${profiles.isFetching || quotas.isFetching ? 'animate-spin' : ''}`}
          />
          Refresh
        </Button>
      </header>

      <div id="codex-settings" className="scroll-mt-24 rounded-xl border bg-card px-4 py-3 sm:px-5">
        <div className="flex items-center justify-between gap-4">
          <div className="space-y-1">
            <label htmlFor="codex-home-automatic-switch" className="text-sm font-medium">
              {t('codexPage.autoSwitch.title')}
            </label>
            <p className="text-xs text-muted-foreground" aria-live="polite">
              {automaticSwitch.isLoading
                ? t('codexPage.autoSwitch.loading')
                : switchStatus
                  ? t(
                      switchStatus.enabled
                        ? 'codexPage.autoSwitch.enabled'
                        : 'codexPage.autoSwitch.disabled'
                    )
                  : t('codexPage.autoSwitch.unavailable')}
            </p>
          </div>
          <Switch
            id="codex-home-automatic-switch"
            aria-describedby="codex-home-automatic-switch-settings"
            checked={switchStatus?.enabled ?? false}
            disabled={!switchStatus || !!automaticSwitch.error || updateAutomaticSwitch.isPending}
            onCheckedChange={(enabled) => updateAutomaticSwitch.mutate(enabled)}
          />
        </div>
        <p
          id="codex-home-automatic-switch-settings"
          className="mt-2 text-xs leading-relaxed text-muted-foreground"
        >
          {switchStatus
            ? `Switch at ${switchStatus.thresholdPercent}% remaining · check every ${switchStatus.pollIntervalSeconds} seconds. Switching waits until Codex is idle.`
            : t('codexPage.autoSwitch.description')}
        </p>
        {switchStatus && (
          <p role="status" className="mt-2 text-xs">
            {t(`codexPage.autoSwitch.outcomes.${switchStatus.outcome}`, {
              defaultValue: switchStatus.message,
            })}
            {switchStatus.outcome === 'error' && switchStatus.message && (
              <span className="mt-1 block text-muted-foreground">{switchStatus.message}</span>
            )}
          </p>
        )}
        {automaticSwitch.error && (
          <div role="alert" className="mt-2 flex items-center gap-3 text-xs text-destructive">
            <p>{t('codexPage.autoSwitch.loadError')}</p>
            <Button variant="outline" size="sm" onClick={() => automaticSwitch.refetch()}>
              {t('codexPage.autoSwitch.retry')}
            </Button>
          </div>
        )}
        {updateAutomaticSwitch.error && (
          <p role="alert" className="mt-2 text-xs text-destructive">
            {t('codexPage.autoSwitch.updateError')} {updateAutomaticSwitch.error.message}
          </p>
        )}
      </div>

      {activation.error && (
        <p
          role="alert"
          className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive"
        >
          {activation.error.message}
        </p>
      )}
      {activation.isSuccess && (
        <p
          role="status"
          className={cn('text-sm text-muted-foreground', privacyMode && PRIVACY_BLUR_CLASS)}
        >
          {t('codex.auth.activationSuccess', { email: activation.data.email })}
        </p>
      )}

      {profiles.isLoading ? (
        <p role="status" className="flex items-center gap-2 py-3 text-sm text-muted-foreground">
          <Loader2 className="h-4 w-4 animate-spin" />
          {t('codex.auth.loading')}
        </p>
      ) : profiles.error || !profiles.data ? (
        <p
          role="alert"
          className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive"
        >
          {t('codex.auth.loadError')}
        </p>
      ) : profiles.data.profiles.length === 0 ? (
        <p className="rounded-xl border bg-card p-4 text-sm text-muted-foreground">
          No Codex accounts are connected.
        </p>
      ) : (
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          {profiles.data.profiles.map((entry) => {
            const isCurrent = profiles.data.activated?.name === entry.name;
            const isActivating = activation.isPending && activation.variables === entry.name;
            const reportedQuota = quotas.data?.profiles.find(
              (item) => item.profileName === entry.name
            );
            const quota = reportedQuota && {
              ...reportedQuota,
              windows: Array.isArray(reportedQuota.windows)
                ? reportedQuota.windows.filter(
                    (window) =>
                      window &&
                      typeof window.key === 'string' &&
                      typeof window.label === 'string' &&
                      Number.isFinite(window.usedPercent) &&
                      window.usedPercent >= 0 &&
                      window.usedPercent <= 100
                  )
                : [],
            };

            return (
              <article
                key={entry.name}
                aria-label={`${entry.name} Codex account`}
                className={`flex min-w-0 flex-col gap-4 rounded-xl border p-4 sm:p-5 ${
                  isCurrent ? 'border-primary/40 bg-primary/5' : 'bg-card'
                }`}
              >
                <div className="space-y-2">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                      {entry.name}
                    </span>
                    {entry.plan && (
                      <Badge variant="outline" className="capitalize">
                        {entry.plan}
                      </Badge>
                    )}
                    {isCurrent && (
                      <Badge variant="secondary">{t('codex.auth.activatedBadge')}</Badge>
                    )}
                  </div>
                  <h3
                    className={cn(
                      'break-all text-sm font-semibold leading-relaxed',
                      privacyMode && PRIVACY_BLUR_CLASS
                    )}
                  >
                    {entry.email ?? 'Account email unavailable'}
                  </h3>
                  {!entry.authValid && (
                    <p className="text-xs text-destructive">{t('codex.auth.statusInvalid')}</p>
                  )}
                </div>
                <div
                  className="flex-1"
                  aria-label={t('codex.auth.quotaProfileLabel', { name: entry.name })}
                >
                  <SubscriptionQuotaDisplay
                    quota={quota}
                    isLoading={quotas.isLoading}
                    error={quotas.error}
                    cached={Boolean(quota && quotas.error)}
                  />
                </div>
                <Button
                  variant={isCurrent ? 'secondary' : 'outline'}
                  size="sm"
                  className="w-full"
                  disabled={
                    !entry.authValid ||
                    !entry.email ||
                    isCurrent ||
                    activation.isPending ||
                    switchStatus?.activationInProgress
                  }
                  title={!entry.email ? t('codex.auth.activationRequiresEmail') : undefined}
                  aria-label={t('codex.auth.activateProfileAction', { name: entry.name })}
                  onClick={() => activation.mutate(entry.name)}
                >
                  {isActivating ? (
                    <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <ArrowRightLeft className="mr-1.5 h-3.5 w-3.5" />
                  )}
                  {isCurrent
                    ? 'Current account'
                    : isActivating
                      ? t('codex.auth.activatingAction')
                      : t('codex.auth.activateAction')}
                </Button>
              </article>
            );
          })}
        </div>
      )}
    </section>
  );
}
