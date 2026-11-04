import { afterAll, describe, expect, it, mock } from 'bun:test';

/** A host-resolution failure must surface as a failed collection, not
 * an empty success: the snapshot publisher turns a rejection into its
 * named "sign-in collection failed" row, while an empty result would
 * silently drop the whole section. The transport is mocked at the
 * module boundary before the service is imported.
 *
 * Bun module mocks are process-wide, so the mock spreads the real
 * module's exports (a partial shape would break later importers of
 * the other exports in the same process) and is restored after. */

const actualTransport =
  await import('../../../src/web-server/services/usage-failover-transport');

mock.module('../../../src/web-server/services/usage-failover-transport', () => ({
  ...actualTransport,
  configuredUsageHosts: async () => {
    throw new Error('host resolution exploded');
  },
  localUsagePython: () => 'python3',
  signinHelperBundle: async () => '',
  usageHelperCommand: () => 'python3 -',
}));

const { getSignInHealth, invalidateSignInHealthCache } =
  await import('../../../src/web-server/services/signin-health-service');

afterAll(() => {
  mock.restore();
});

describe('host resolution failure', () => {
  it('rejects instead of publishing an empty machine map', async () => {
    invalidateSignInHealthCache();
    await expect(getSignInHealth()).rejects.toThrow('host resolution exploded');
  });
});
