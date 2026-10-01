import type { CodexQuotaResult } from './quota-types';

function object(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function amount(value: unknown): number | null {
  if (typeof value === 'string' && /^-?\d+(?:\.\d+)?$/.test(value.trim())) {
    value = Number(value);
  }
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

/** The official backend schema reports reset_at as Unix seconds. */
export function codexResetTimestamp(value: unknown): string | null {
  const seconds = amount(value);
  if (seconds === null || seconds <= 0) return null;
  const time = new Date(seconds * 1000);
  return Number.isFinite(time.getTime()) ? time.toISOString() : null;
}

/**
 * Preserve credits and monthly spend controls without inventing balances/dates.
 * Units follow openai/codex codex-rs/tui/src/status/rate_limits.rs: credits.
 * Credits have no expiration field in the currently documented backend schema.
 */
export function normalizeCodexExtraUsage(
  payload: unknown
): Pick<CodexQuotaResult, 'credits' | 'monthlySpend' | 'resetCredits'> {
  const data = object(payload);
  if (!data) return {};
  const result: Pick<CodexQuotaResult, 'credits' | 'monthlySpend' | 'resetCredits'> = {};
  const credits = object(data.credits);
  if (credits) {
    result.credits = {
      hasCredits: typeof credits.has_credits === 'boolean' ? credits.has_credits : null,
      unlimited: typeof credits.unlimited === 'boolean' ? credits.unlimited : null,
      balance: amount(credits.balance),
    };
  }
  const control = object(data.spend_control);
  const monthly = object(control?.individual_limit);
  if (monthly) {
    const remaining = amount(monthly.remaining_percent);
    result.monthlySpend = {
      used: amount(monthly.used),
      limit: amount(monthly.limit),
      remainingPercent: remaining !== null && remaining >= 0 && remaining <= 100 ? remaining : null,
      resetAt: codexResetTimestamp(monthly.reset_at),
    };
  }
  const resets = object(data.rate_limit_reset_credits);
  if (resets) {
    result.resetCredits = {
      available: count(resets.available_count),
      applicable: count(resets.applicable_available_count),
    };
  }
  return result;
}

function count(value: unknown): number | null {
  const number = amount(value);
  return number !== null && Number.isSafeInteger(number) && number >= 0 ? number : null;
}

/** Whitelist usable pack metadata; omit IDs, text, consumed and expired packs. */
export function normalizeCodexResetCreditDetails(
  payload: unknown
): NonNullable<CodexQuotaResult['resetCredits']>['credits'] {
  const data = object(payload);
  if (!Array.isArray(data?.credits)) return undefined;
  return data.credits.slice(0, 256).flatMap((entry) => {
    const credit = object(entry);
    if (credit?.status !== 'available') return [];
    const rawType = credit.reset_type;
    const resetType =
      typeof rawType === 'string' && /^[a-z0-9_]{1,64}$/.test(rawType) ? rawType : 'reset';
    const rawExpiry = credit.expires_at;
    const date =
      typeof rawExpiry === 'string' && /^\d{4}-\d{2}-\d{2}T/.test(rawExpiry)
        ? new Date(rawExpiry)
        : null;
    return [
      {
        resetType,
        expiresAt: date && Number.isFinite(date.getTime()) ? date.toISOString() : null,
      },
    ];
  });
}
