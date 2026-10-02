import type { DashboardProvider } from './account-dashboard-types';

/**
 * The server's provider table: every supported provider, in display order
 * (Claude, Codex, Antigravity, then the rest). Page routes such as
 * /accounts/<provider> resolve only for ids listed here, so a provider that
 * leaves the table stops resolving there too.
 */
export const DASHBOARD_PROVIDER_IDS: readonly DashboardProvider[] = Object.freeze([
  'claude',
  'codex',
  'antigravity',
  'cursor',
  'muse',
  'kimi-code',
  'qwen',
  'zai',
  'opencode-go',
]);

export function isDashboardProviderId(value: unknown): value is DashboardProvider {
  return typeof value === 'string' && (DASHBOARD_PROVIDER_IDS as readonly string[]).includes(value);
}
