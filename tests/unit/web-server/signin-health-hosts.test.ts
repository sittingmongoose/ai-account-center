import { describe, expect, it, mock } from 'bun:test';

/** A host-resolution failure must surface as a failed collection, not
 * an empty success: the snapshot publisher turns a rejection into its
 * named "sign-in collection failed" row, while an empty result would
 * silently drop the whole section. The transport is mocked at the
 * module boundary before the service is imported. */

mock.module('../../../src/web-server/services/usage-failover-transport', () => ({
  configuredUsageHosts: async () => {
    throw new Error('host resolution exploded');
  },
  localUsagePython: () => 'python3',
  signinHelperBundle: async () => '',
  usageHelperCommand: () => 'python3 -',
}));

const { getSignInHealth, invalidateSignInHealthCache } =
  await import('../../../src/web-server/services/signin-health-service');

describe('host resolution failure', () => {
  it('rejects instead of publishing an empty machine map', async () => {
    invalidateSignInHealthCache();
    await expect(getSignInHealth()).rejects.toThrow('host resolution exploded');
  });
});
