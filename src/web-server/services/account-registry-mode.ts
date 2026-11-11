/**
 * The one place that answers which account registry is active. "Additional" accounts (Cursor,
 * Kimi, Qwen, Z.ai, OpenCode Go, Muse, Antigravity-by-registry) can be listed by three stores:
 * a valid registry v2 (`account-usage-accounts.json`), an unreadable v2 file, or the version 1
 * manifest (`account-usage-sources.json`, a deliberate read-only migration input). The honesty
 * rule lives here: an unreadable v2 file means every provider unavailable, never a version 1
 * fallback (CONTRACT-registry-lifecycle 3.1). Callers ask this module which store is active;
 * none of them compares registry states or mode strings itself.
 */
import type { AccountRegistryRead, AccountRegistryV2 } from './account-registry-v2';

/** Which store listed the accounts: v1 manifest, valid v2 registry, or an unreadable v2 file. */
export type AdditionalRegistryMode = 'v1' | 'v2' | 'v2-invalid';

/** One registry read classified: the active store, and its accounts when it is a valid v2. */
export type AccountRegistryClassification =
  { mode: 'v2'; registry: AccountRegistryV2 } | { mode: 'v2-invalid' } | { mode: 'v1' };

/** Answers which account store one registry read leaves active. */
export function classifyAccountRegistry(read: AccountRegistryRead): AccountRegistryClassification {
  if (read.state === 'ok') return { mode: 'v2', registry: read.registry };
  if (read.state === 'invalid') return { mode: 'v2-invalid' };
  return { mode: 'v1' };
}

/** An unreadable v2 file: every provider unavailable, never a version 1 fallback. */
export function isUnreadableAccountRegistry(mode: AdditionalRegistryMode): boolean {
  return mode === 'v2-invalid';
}

/** The registry itself named the accounts (a valid or unreadable v2), not the v1 defaults. */
export function usesAccountRegistry(mode: AdditionalRegistryMode): boolean {
  return mode !== 'v1';
}

/** Only a valid v2 registry drops a provider's placeholder row when it has no accounts. */
export function keepsProviderPlaceholderRow(mode: AdditionalRegistryMode): boolean {
  return mode !== 'v2';
}
