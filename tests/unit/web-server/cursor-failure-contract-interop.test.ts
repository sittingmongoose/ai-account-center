import { afterEach, describe, expect, it } from 'bun:test';
import { execFileSync } from 'child_process';
import fs from 'fs/promises';
import os from 'os';
import path from 'path';
import { UsageFailoverService } from '../../../src/web-server/services/usage-failover-service';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';
import type {
  UsageCandidate,
  UsageFailoverTarget,
  UsageHelperResponse,
  UsageHost,
} from '../../../src/web-server/services/usage-failover-contract';

type Collection = Extract<UsageHelperResponse, { action: 'collect' }>;
interface Scenario {
  status: number;
  candidate: UsageCandidate;
  initial: Collection;
  failed: Collection;
  externalRequests: number;
}

const fixtures: { cases: Scenario[] } = JSON.parse(
  execFileSync(
    process.platform === 'win32' ? 'python' : '/usr/bin/python3',
    ['-B', path.resolve(__dirname, '../account-usage/cursor_failure_contract_fixture.py')],
    {
      encoding: 'utf8',
      timeout: 15_000,
      maxBuffer: 256 * 1024,
      env: {
        PATH: process.env.PATH,
        SystemRoot: process.env.SystemRoot,
        PYTHONDONTWRITEBYTECODE: '1',
      },
    }
  )
);
const scopes: string[] = [];
afterEach(async () => {
  await Promise.all(
    scopes.splice(0).map((scope) => fs.rm(scope, { recursive: true, force: true }))
  );
});

describe('Cursor Python dispatcher to TypeScript coordinator contract', () => {
  for (const scenario of fixtures.cases) {
    it(`invalidates legacy quota after Premium membership followed by HTTP ${scenario.status}`, async () => {
      const ccsDir = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-cursor-dispatcher-'));
      scopes.push(ccsDir);
      const host: UsageHost = { host: 'ubuntu', platform: 'ubuntu', sshHost: 'fixture-ubuntu' };
      const target: UsageFailoverTarget = {
        accountId: 'cursor:fixture',
        provider: 'cursor',
        ccsDir,
        binding: 'synthetic-cursor-source',
        primarySources: [{ ...host, selector: 'discover' }],
      };
      const template: DashboardAccount = {
        id: target.accountId,
        provider: 'cursor',
        providerLabel: 'Cursor',
        label: 'Cursor',
        platform: 'ubuntu',
        source: 'Fixture',
        status: 'unavailable',
        email: null,
        plan: null,
        message: null,
        fetchedAt: null,
        sampledAt: null,
        isActive: false,
        windows: [],
        capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
      };
      let now = Date.parse('2026-10-14T12:00:00Z');
      let phase: 'initial' | 'failed' = 'initial';
      let collections = 0;
      const deps = {
        now: () => now,
        hosts: async () => [host],
        transport: async (
          _host: UsageHost,
          request: { action: string }
        ): Promise<UsageHelperResponse> => {
          if (request.action === 'inventory') {
            return { version: 1, action: 'inventory', candidates: [scenario.candidate] };
          }
          collections++;
          return structuredClone(scenario[phase]);
        },
      };
      const service = new UsageFailoverService(deps);
      const initial = await service.collect(target, template);
      expect(initial.plan).toBe('ultra');
      expect(initial.windows.some((window) => window.key === 'apiPercentUsed')).toBe(true);
      phase = 'failed';
      now += 60_000;
      const changed = await service.collect(target, template);
      expect(changed.plan).toBe('premium');
      expect(changed.windows).toHaveLength(0);
      expect(changed).not.toHaveProperty('observedPlan');
      expect(changed).not.toHaveProperty('observedPlanAt');
      expect(scenario.externalRequests).toBe(0);
      if (scenario.status === 429) {
        expect(scenario.failed.retryAfterSeconds).toBe(1800);
        const count = collections;
        now += 1799_000;
        const held = await new UsageFailoverService(deps).collect(target, template);
        expect(held.plan).toBe('premium');
        expect(held.windows).toHaveLength(0);
        expect(collections).toBe(count);
      }
    });
  }
});
