import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { CodexAuthProfilesCard } from '@/components/compatible-cli/codex-auth-profiles-card';
import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/switch';
import {
  useCodexAutomaticSwitch,
  useUpdateCodexAutomaticSwitch,
} from '@/hooks/use-codex-auth-profiles';

export function CodexAccountsPage() {
  const { t } = useTranslation();
  const automaticSwitch = useCodexAutomaticSwitch();
  const updateAutomaticSwitch = useUpdateCodexAutomaticSwitch();
  const switchStatus = automaticSwitch.data;

  return (
    <div className="mx-auto w-full max-w-6xl space-y-6 p-4 sm:p-6">
      <header className="space-y-2">
        <h1 className="text-2xl font-semibold tracking-tight">{t('codexPage.accountsTitle')}</h1>
        <p className="text-sm text-muted-foreground">{t('codexPage.accountsInstruction')}</p>
        <p className="text-sm text-muted-foreground">{t('accountQuota.explanation')}</p>
      </header>
      <section className="space-y-3 rounded-md border p-4">
        <div className="flex items-center justify-between gap-4">
          <div className="space-y-1">
            <label htmlFor="codex-automatic-switch" className="font-medium">
              {t('codexPage.autoSwitch.title')}
            </label>
            <p className="text-sm text-muted-foreground" aria-live="polite">
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
            id="codex-automatic-switch"
            aria-describedby="codex-automatic-switch-description"
            checked={switchStatus?.enabled ?? false}
            disabled={!switchStatus || !!automaticSwitch.error || updateAutomaticSwitch.isPending}
            onCheckedChange={(enabled) => updateAutomaticSwitch.mutate(enabled)}
          />
        </div>
        <p id="codex-automatic-switch-description" className="text-sm text-muted-foreground">
          {t('codexPage.autoSwitch.description')}
        </p>
        <p className="text-sm text-muted-foreground">{t('codexPage.autoSwitch.claudeManual')}</p>
        {switchStatus && (
          <p role="status" className="text-sm">
            {t(`codexPage.autoSwitch.outcomes.${switchStatus.outcome}`, {
              defaultValue: switchStatus.message,
            })}
            {switchStatus.outcome === 'error' && switchStatus.message && (
              <span className="mt-1 block text-muted-foreground">{switchStatus.message}</span>
            )}
          </p>
        )}
        {automaticSwitch.error && (
          <div role="alert" className="flex items-center gap-3 text-sm text-destructive">
            <p>{t('codexPage.autoSwitch.loadError')}</p>
            <Button variant="outline" size="sm" onClick={() => automaticSwitch.refetch()}>
              {t('codexPage.autoSwitch.retry')}
            </Button>
          </div>
        )}
        {updateAutomaticSwitch.error && (
          <p role="alert" className="text-sm text-destructive">
            {t('codexPage.autoSwitch.updateError')} {updateAutomaticSwitch.error.message}
          </p>
        )}
      </section>
      <CodexAuthProfilesCard />
      <div className="space-y-1 rounded-md border p-4">
        <Link
          to="/analytics"
          className="text-sm font-medium text-primary underline underline-offset-4"
        >
          {t('accountQuota.recordedCliUsage')}
        </Link>
        <p className="text-sm text-muted-foreground">
          {t('accountQuota.recordedCliUsageDescription')}
        </p>
      </div>
    </div>
  );
}
