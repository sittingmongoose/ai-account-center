import { useTranslation } from 'react-i18next';
import { Badge } from '@/components/ui/badge';

export interface AccountQuotaWindow {
  key: string;
  label: string;
  usedPercent: number;
  resetsAt?: string;
}

export interface AccountQuotaSummary {
  status: 'available' | 'reauth_required' | 'not_connected' | 'unavailable';
  windows: AccountQuotaWindow[];
  fetchedAt?: string;
  sampledAt?: string | null;
  message?: string;
}

function readableDate(value: string, locale: string) {
  const date = new Date(value);
  return Number.isFinite(date.getTime()) ? date.toLocaleString(locale) : value;
}

export function SubscriptionQuotaDisplay({
  quota,
  isLoading,
  error,
  cached = false,
}: {
  quota?: AccountQuotaSummary;
  isLoading?: boolean;
  error?: unknown;
  cached?: boolean;
}) {
  const { t, i18n } = useTranslation();
  const windowLabel = (window: AccountQuotaWindow) => {
    if (window.key === 'five_hour') return t('accountQuota.fiveHour');
    if (window.key === 'seven_day') return t('accountQuota.weekly');
    return window.label;
  };
  const validWindows =
    quota?.windows.filter(
      (window) => Number.isFinite(window.usedPercent) && window.usedPercent >= 0
    ) ?? [];
  const statusKey =
    quota?.status === 'reauth_required'
      ? 'reauthRequired'
      : quota?.status === 'not_connected'
        ? 'notConnected'
        : 'unavailable';

  return (
    <div className="min-w-0 space-y-2" aria-label={t('accountQuota.title')}>
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-xs font-medium">{t('accountQuota.title')}</span>
        {cached && (
          <Badge variant="outline" className="text-xs">
            {t('accountQuota.cached')}
          </Badge>
        )}
      </div>
      {isLoading && !quota ? (
        <p className="text-xs text-muted-foreground">{t('accountQuota.loading')}</p>
      ) : error && !quota ? (
        <p className="text-xs text-muted-foreground">{t('accountQuota.loadError')}</p>
      ) : quota?.status === 'available' && validWindows.length ? (
        <div className="grid gap-3 sm:grid-cols-2">
          {validWindows.map((window) => (
            <div key={window.key} className="min-w-0 space-y-1">
              <div className="flex flex-wrap items-baseline justify-between gap-x-2 text-xs">
                <span>{windowLabel(window)}</span>
                <span className="font-medium">
                  {t('accountQuota.usedPercent', {
                    percent: Math.round(window.usedPercent * 10) / 10,
                  })}
                </span>
              </div>
              <div
                role="progressbar"
                aria-label={windowLabel(window)}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={Math.min(window.usedPercent, 100)}
                aria-valuetext={t('accountQuota.usedPercent', { percent: window.usedPercent })}
                className="h-1.5 overflow-hidden rounded-full bg-muted"
              >
                <div
                  className={
                    window.usedPercent >= 90 ? 'h-full bg-destructive' : 'h-full bg-primary'
                  }
                  style={{ width: `${Math.min(window.usedPercent, 100)}%` }}
                />
              </div>
              {window.resetsAt ? (
                <p className="text-xs text-muted-foreground">
                  {t('accountQuota.resets', { time: readableDate(window.resetsAt, i18n.language) })}
                </p>
              ) : (
                <p className="text-xs text-muted-foreground">
                  {t('accountQuota.resetUnavailable')}
                </p>
              )}
            </div>
          ))}
        </div>
      ) : (
        <p className="text-xs text-muted-foreground">
          {quota?.message || t(`accountQuota.${statusKey}`)}
        </p>
      )}
      {quota?.sampledAt && (
        <p className="text-xs text-muted-foreground">
          {t('accountQuota.sampled', { time: readableDate(quota.sampledAt, i18n.language) })}
        </p>
      )}
      {quota?.fetchedAt && (
        <p className="text-xs text-muted-foreground">
          {t('accountQuota.fetched', { time: readableDate(quota.fetchedAt, i18n.language) })}
        </p>
      )}
      {Boolean(error) && quota && (
        <p className="text-xs text-muted-foreground">{t('accountQuota.refreshError')}</p>
      )}
    </div>
  );
}
