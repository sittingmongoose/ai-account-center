/** H2 snapshot projection on the actual TS adapters and private Rust socket. */
import assert from 'node:assert/strict';
import path from 'node:path';
import type {
  UsageHelperResponse,
  UsageFailoverTarget,
} from '../../../src/web-server/services/usage-failover-contract';
import type { AccountDashboardDeps } from '../../../src/web-server/services/account-dashboard-service';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';

const fixture = JSON.parse(await Bun.stdin.text()) as {
  root: string;
  socketPath: string;
  secret: string;
  bootId: string;
  args: Record<string, unknown>;
  reference: DashboardAccount;
  inventory: UsageHelperResponse;
};
Object.assign(process.env, {
  CCS_DIR: fixture.root,
  CCS_HOME: fixture.root,
  AAC_AGENT_VERBS: 'on',
  AAC_AGENT_TRANSPORT: 'on',
  AAC_NO_LOCAL_RUST_BUILD: '1',
});
const { settlePrivateFileOwners } = await import('../../../src/shared/private-files');
const { registerRustAgentConnection } =
  await import('../../../src/web-server/rust-service-transport');
const { collectQuotaWithFallback, runQuotaAgent } =
  await import('../../../src/web-server/services/agent-verbs');
const { probeAdditionalSource } =
  await import('../../../src/web-server/services/additional-account-service');
const { AccountDashboardService, withPassedResetsAt } =
  await import('../../../src/web-server/services/account-dashboard-service');

const { UsageFailoverService } =
  await import('../../../src/web-server/services/usage-failover-service');
const { runFailoverInventoryAgent } =
  await import('../../../src/web-server/services/usage-failover-transport');

const close = registerRustAgentConnection(fixture.root, fixture, new Set());
const source = { provider: 'kimi-code', platform: 'ubuntu', sshHost: 'fixture' } as const;
const at = Date.parse('2026-01-01T00:00:00Z');
const common: AccountDashboardDeps = {
  scope: () => fixture.root,
  now: () => at,
  refreshIntervalSeconds: () => 60,
  getCodexSummary: async () => ({ active: null, activated: null, default: null, profiles: [] }),
  getCodexRows: async () => [],
  getCachedCodexRows: () => [],
  getRenewalStatus: async () => ({ profiles: [] }),
  listClaudeProfiles: async () => [],
  listClaudePending: async () => [],
  getOptionalWalletAccounts: async () => [],
  hasAntigravityProfiles: () => false,
  accountJobStates: () => new Map(),
  providerFacts: () => ({ lifecycleRoutes: false, secureTransport: false }),
  readVisibility: async () => ({
    state: 'ok',
    visibility: {
      hiddenProviders: [],
      hiddenAccountIds: [],
      trayHiddenProviders: [],
      trayHiddenAccountIds: [],
    },
  }),
  invalidateClaudeCache: () => undefined,
  claudeSignIns: async () => null,
  serverInfo: () => undefined,
  failover: null,
};
try {
  let nativeReads = 0;
  let fallbackReads = 0;
  const dashboard = (useRust: boolean) =>
    new AccountDashboardService({
      ...common,
      getAdditionalAccounts: async () => [
        await probeAdditionalSource(source, () =>
          useRust
            ? collectQuotaWithFallback(
                () => {
                  nativeReads++;
                  return runQuotaAgent(source, fixture.args);
                },
                () => {
                  fallbackReads++;
                  return Promise.resolve(JSON.stringify(fixture.reference));
                }
              )
            : Promise.resolve(JSON.stringify(fixture.reference))
        ),
      ],
    });
  settlePrivateFileOwners(() => false);
  const ts = dashboard(false);
  const expected = await ts.get('windows');
  assert.equal(expected.accounts.find((row) => row.provider === 'kimi-code')?.status, 'ok');
  const expectedHome = await ts.readForHome('windows');

  settlePrivateFileOwners((group) => group === 'collection');
  const rust = dashboard(true);
  const actual = await rust.get('windows');
  const actualHome = await rust.readForHome('windows');
  assert.equal(JSON.stringify(actual), JSON.stringify(expected));
  assert.equal(JSON.stringify(actualHome), JSON.stringify(expectedHome));
  assert.deepEqual(withPassedResetsAt(actualHome.dashboard, actualHome.now), actual);
  assert.equal(nativeReads, 1, 'Home consumed the same dashboard generation');
  assert.equal(fallbackReads, 0, 'the service returned the real fixture result');
  const template = actual.accounts.find((row) => row.provider === 'kimi-code');
  assert.ok(template);
  const host = { host: 'ubuntu', platform: 'ubuntu', sshHost: 'fixture' } as const;
  const target = (ccsDir: string): UsageFailoverTarget => ({
    ccsDir,
    accountId: template.id,
    provider: 'kimi-code',
    binding: 'fixture-kimi',
    primarySources: [{ ...host, selector: 'discover' }],
  });

  let inventories = 0;
  const collectFailover = (useRust: boolean) =>
    new UsageFailoverService({
      now: () => at,
      hosts: async () => [],
      transport: async (_host, request) => {
        if (request.action === 'inventory') {
          if (!useRust) return fixture.inventory;
          inventories++;
          const answer = await runFailoverInventoryAgent(
            host,
            { ...request, homeDir: String(fixture.args.home) },
            Date.now() + 10_000
          );
          assert.ok(answer);
          return answer;
        }
        return {
          version: 1,
          action: 'collect',
          candidate: request.candidate,
          failureCode: null,
          sample: fixture.reference,
        };
      },
    });

  settlePrivateFileOwners(() => false);
  const legacy = await collectFailover(false).collect(
    target(path.join(fixture.root, 'legacy')),
    template
  );
  settlePrivateFileOwners((group) => group === 'collection');
  const owned = await collectFailover(true).collect(target(fixture.root), template);
  assert.equal(owned.status, 'ok');
  assert.equal(JSON.stringify(owned), JSON.stringify(legacy));
  const restored = await collectFailover(true).collect(target(fixture.root), template);
  assert.deepEqual(restored, owned);
  assert.equal(inventories, 1, 'the cold coordinator reused the Rust-written observation');

  console.log(
    'PASS H2 dashboard and Home input bytes, inventory identity and cold cache restoration on the Kimi fixture'
  );
} finally {
  settlePrivateFileOwners(() => false);
  close();
}
