import type { CodexQuotaResult } from '../../cliproxy/quota/quota-types';
import type { BalanceWindowDetail, QuotaWindowDetail } from '../routes/bar-routes';

/** Additional windows have non-core keys so they cannot drive native rotation. */
export function getCodexAdditionalUsageWindows(quota: CodexQuotaResult): {
  quotaWindows: QuotaWindowDetail[];
  balanceWindows: BalanceWindowDetail[];
} {
  const quotaWindows: QuotaWindowDetail[] = [];
  const balanceWindows: BalanceWindowDetail[] = [];
  const coreLabels = new Set(
    [quota.coreUsage?.fiveHour?.label, quota.coreUsage?.weekly?.label].filter(Boolean)
  );
  for (const [index, window] of (quota.windows ?? []).entries()) {
    if ((window.category === 'usage' || !window.category) && coreLabels.has(window.label)) {
      continue;
    }
    if (
      !Number.isFinite(window.usedPercent) ||
      window.usedPercent < 0 ||
      window.usedPercent > 100
    ) {
      continue;
    }
    const minutes =
      typeof window.limitWindowSeconds === 'number' && window.limitWindowSeconds > 0
        ? window.limitWindowSeconds / 60
        : window.cadence === '5h'
          ? 300
          : window.cadence === 'weekly'
            ? 10080
            : null;
    const cadence =
      minutes === 60
        ? 'hourly'
        : minutes === 300
          ? '5 hours'
          : minutes === 1440
            ? 'daily'
            : minutes === 10080
              ? 'weekly'
              : minutes === null
                ? null
                : `${minutes} minutes`;
    quotaWindows.push({
      key: `extra_${window.category ?? 'usage'}_${index}`,
      label: [window.featureLabel ?? window.label, cadence].filter(Boolean).join(' · '),
      usedPercent: window.usedPercent,
      remainingPercent: 100 - window.usedPercent,
      resetAt: window.resetAt,
      windowMinutes: minutes,
      kind: 'rate_limit',
    });
  }
  if (quota.credits) {
    balanceWindows.push({
      key: 'credits_balance',
      label: 'Extra usage credits',
      kind: 'balance',
      usedPercent: null,
      remainingPercent: null,
      resetAt: null,
      expiresAt: null,
      windowMinutes: null,
      used: null,
      limit: null,
      remaining: quota.credits.balance,
      unit: 'credits',
      ...(quota.credits.hasCredits === null ? {} : { enabled: quota.credits.hasCredits }),
      ...(quota.credits.unlimited === null ? {} : { unlimited: quota.credits.unlimited }),
    });
  }
  if (quota.monthlySpend) {
    const monthly = quota.monthlySpend;
    balanceWindows.push({
      key: 'monthly_credit_limit',
      label: 'Monthly credit limit',
      kind: 'spend',
      usedPercent: monthly.remainingPercent === null ? null : 100 - monthly.remainingPercent,
      remainingPercent: monthly.remainingPercent,
      resetAt: monthly.resetAt,
      windowMinutes: null,
      used: monthly.used,
      limit: monthly.limit,
      remaining:
        monthly.used === null || monthly.limit === null ? null : monthly.limit - monthly.used,
      unit: 'credits',
    });
  }
  if (quota.resetCredits) {
    const resets = quota.resetCredits;
    const groups = new Map<string | null, number>();
    for (const credit of resets.credits ?? []) {
      groups.set(credit.expiresAt, (groups.get(credit.expiresAt) ?? 0) + 1);
    }
    // A complete details response supplies real pack expirations. Otherwise
    // keep the known total without suggesting an expiry for unlisted packs.
    if (groups.size && (resets.credits?.length ?? 0) === resets.available) {
      let index = 0;
      for (const [expiresAt, remaining] of groups) {
        balanceWindows.push({
          key: `banked_resets_${index++}`,
          label: 'Banked resets',
          kind: 'balance',
          usedPercent: null,
          remainingPercent: null,
          resetAt: null,
          windowMinutes: null,
          remaining,
          unit: 'resets',
          expiresAt,
        });
      }
    } else {
      balanceWindows.push({
        key: 'banked_resets',
        label: 'Banked resets',
        kind: 'balance',
        usedPercent: null,
        remainingPercent: null,
        resetAt: null,
        windowMinutes: null,
        remaining: resets.available,
        unit: 'resets',
        expiresAt: null,
      });
    }
  }
  return { quotaWindows, balanceWindows };
}
