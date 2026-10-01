import { Clock3 } from 'lucide-react';
import { Badge } from '@/components/ui/badge';
import { readableUsageTime } from '@/lib/accounts-dashboard';
import type { DashboardAccount, DashboardUsageWindow } from '@/hooks/use-accounts-dashboard';

function validPercent(value: number | null): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 100;
}

function usagePercent(window: DashboardUsageWindow): number | null {
  if (validPercent(window.usedPercent)) return window.usedPercent;
  if (validPercent(window.remainingPercent)) return 100 - window.remainingPercent;
  return null;
}

function compactNumber(value: number) {
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 2, notation: 'compact' }).format(
    value
  );
}

function UsageWindow({ window }: { window: DashboardUsageWindow }) {
  const percent = usagePercent(window);
  const used =
    typeof window.used === 'number' && Number.isFinite(window.used) && window.used >= 0
      ? window.used
      : null;
  const limit =
    typeof window.limit === 'number' && Number.isFinite(window.limit) && window.limit > 0
      ? window.limit
      : null;
  const reset = readableUsageTime(window.resetAt);
  return (
    <div className="space-y-1.5">
      <div className="flex flex-wrap items-baseline justify-between gap-2 text-xs">
        <span className="text-muted-foreground">{window.label}</span>
        <span className="font-medium tabular-nums">
          {percent !== null ? `${Math.round(percent * 10) / 10}% used` : 'Usage not reported'}
        </span>
      </div>
      {percent !== null && (
        <div
          role="progressbar"
          aria-label={window.label}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={percent}
          className="h-1.5 overflow-hidden rounded-full bg-muted"
        >
          <div
            className={
              percent >= 90 ? 'h-full rounded-full bg-destructive' : 'h-full rounded-full bg-accent'
            }
            style={{ width: `${percent}%` }}
          />
        </div>
      )}
      {(used !== null || limit !== null) && (
        <p className="text-xs text-muted-foreground tabular-nums">
          {used !== null ? compactNumber(used) : '—'}
          {limit !== null ? ` / ${compactNumber(limit)}` : ''}
          {window.unit ? ` ${window.unit}` : ''}
        </p>
      )}
      <p className="flex items-start gap-1 text-xs text-muted-foreground">
        <Clock3 className="mt-0.5 h-3 w-3 shrink-0" />
        {reset ? (
          <time dateTime={window.resetAt ?? undefined} title={window.resetAt ?? undefined}>
            Resets {reset}
          </time>
        ) : (
          'Reset time unavailable'
        )}
      </p>
    </div>
  );
}

export function AccountStatusBadge({ status }: { status: DashboardAccount['status'] }) {
  const labels = {
    ok: 'Live',
    cached: 'Cached',
    needs_sign_in: 'Sign-in needed',
    unavailable: 'Unavailable',
    error: 'Refresh failed',
  };
  return (
    <Badge
      variant={status === 'needs_sign_in' || status === 'error' ? 'destructive' : 'outline'}
      className="text-[10px] font-medium"
    >
      {labels[status] ?? 'Unavailable'}
    </Badge>
  );
}

export function DashboardUsage({
  account,
  loading,
  error,
}: {
  account?: DashboardAccount;
  loading?: boolean;
  error?: boolean;
}) {
  const windows = Array.isArray(account?.windows) ? account.windows : [];
  const sample = readableUsageTime(account?.sampledAt ?? account?.fetchedAt);
  return (
    <div className="space-y-3" aria-label="Subscription usage">
      {loading && !account ? (
        <p className="text-xs text-muted-foreground">Loading usage…</p>
      ) : windows.length ? (
        <div className="grid gap-4">
          {windows.map((window) => (
            <UsageWindow key={window.key} window={window} />
          ))}
        </div>
      ) : (
        <p className="text-xs leading-5 text-muted-foreground">
          {account?.message ||
            (error
              ? 'Unable to load subscription usage.'
              : account?.status === 'needs_sign_in'
                ? 'No usable signed-in session was found.'
                : 'Subscription usage unavailable.')}
        </p>
      )}
      {windows.length > 0 && account?.message && (
        <p className="text-xs leading-5 text-muted-foreground">{account.message}</p>
      )}
      {sample && <p className="text-[11px] text-muted-foreground">Updated {sample}</p>}
      {error && account && (
        <p className="text-xs text-destructive">
          Refresh failed. Showing the last received sample.
        </p>
      )}
    </div>
  );
}
