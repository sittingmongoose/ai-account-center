/**
 * The registry-mode module answers which account store is active; one test per mode keeps the
 * three answers (and the no-v1-fallback honesty rule) pinned in this one place.
 */
import { describe, expect, it } from 'bun:test';
import {
  classifyAccountRegistry,
  isUnreadableAccountRegistry,
  keepsProviderPlaceholderRow,
  usesAccountRegistry,
} from '../../../src/web-server/services/account-registry-mode';

describe('account registry mode', () => {
  it('v1: an absent registry leaves the version 1 manifest active', () => {
    const active = classifyAccountRegistry({ state: 'absent' });
    expect(active).toEqual({ mode: 'v1' });
    expect(usesAccountRegistry('v1')).toBe(false);
    expect(keepsProviderPlaceholderRow('v1')).toBe(true);
    expect(isUnreadableAccountRegistry('v1')).toBe(false);
  });

  it('v2: a valid registry is active and carries its accounts', () => {
    const registry = { version: 2 as const, accounts: [] };
    const active = classifyAccountRegistry({ state: 'ok', registry });
    expect(active).toEqual({ mode: 'v2', registry });
    expect(active.mode === 'v2' ? active.registry : null).toBe(registry);
    expect(usesAccountRegistry('v2')).toBe(true);
    expect(keepsProviderPlaceholderRow('v2')).toBe(false);
    expect(isUnreadableAccountRegistry('v2')).toBe(false);
  });

  it('v2-invalid: an unreadable registry is its own mode and never falls back to v1', () => {
    const active = classifyAccountRegistry({ state: 'invalid' });
    expect(active).toEqual({ mode: 'v2-invalid' });
    expect(isUnreadableAccountRegistry('v2-invalid')).toBe(true);
    // Still the registry's own account list shape, not the v1 defaults.
    expect(usesAccountRegistry('v2-invalid')).toBe(true);
    expect(keepsProviderPlaceholderRow('v2-invalid')).toBe(true);
  });
});
