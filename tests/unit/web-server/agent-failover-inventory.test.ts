import { describe, expect, it } from 'bun:test';
import { collectAgentWithFallback } from '../../../src/web-server/services/agent-verbs';
import { runFailoverInventoryAgent } from '../../../src/web-server/services/usage-failover-transport';
import type { ManagedCommandResult } from '../../../src/utils/managed-process';
import type {
  UsageHelperRequest,
  UsageHost,
} from '../../../src/web-server/services/usage-failover-contract';

const host: UsageHost = { host: 'mac', platform: 'mac', sshHost: 'fixture-mac' };
const artifact = {
  target: 'aarch64-apple-darwin',
  version: '0.1.0',
  sha256: 'a'.repeat(64),
  file: 'unused',
};
const request: UsageHelperRequest = {
  version: 1,
  action: 'inventory',
  provider: 'claude',
  ccsDir: '/fixture/scope',
};
const candidate = {
  locator: '1'.repeat(64),
  selectors: ['claude-profile:fixture'],
  identity: { kind: 'claude-account-org-v1', key: '2'.repeat(64) },
  revision: '3'.repeat(64),
  email: 'one@example.test',
  plan: 'max',
};
const inventory = { version: 1, action: 'inventory', candidates: [candidate] };
const ok = (stdout: string): ManagedCommandResult => ({
  stdout,
  code: 0,
  failed: false,
  missing: false,
  timedOut: false,
});
const event = (value: unknown) =>
  JSON.stringify({ type: 'result', protocol: 1, id: 'failover-inventory', value });
const invoke = (run: (...args: any[]) => Promise<ManagedCommandResult>, req = request) =>
  runFailoverInventoryAgent(host, req, Date.now() + 10000, {
    artifacts: [artifact],
    mode: 'on',
    run,
  });

describe('failover inventory native boundary', () => {
  it('keeps selectors/scopes on verified stdin and validates the full inventory', async () => {
    expect(
      await invoke(async (file, args, options) => {
        expect(file).toBe('ssh');
        expect(args.join(' ')).toContain(artifact.sha256);
        expect(args.join(' ')).not.toContain('/fixture/scope');
        const wire = JSON.parse(options.input);
        expect(wire.verb).toBe('failover-inventory');
        expect(wire.args).toEqual(request);
        expect(wire.deadlineMs).toBeLessThanOrEqual(10000);
        return ok(event(inventory));
      })
    ).toEqual(inventory);
  });
  it('runs Python exactly once for execution, protocol, schema and provider failures', async () => {
    for (const run of [
      async () => {
        throw new Error('synthetic spawn');
      },
      async () => ({ ...ok(''), code: 87 }),
      async () => ({ ...ok(''), code: 1 }),
      async () => ({ ...ok(''), failed: true }),
      async () => ({ ...ok(''), missing: true }),
      async () => ({ ...ok(''), timedOut: true }),
      async () => ok('malformed'),
      async () =>
        ok(
          JSON.stringify({
            type: 'result',
            protocol: 2,
            id: 'failover-inventory',
            value: inventory,
          })
        ),
      async () =>
        ok(JSON.stringify({ type: 'result', protocol: 1, id: 'wrong', value: inventory })),
      async () => ok(event({ fallback: 'python' })),
      async () => ok(event({ ...inventory, failed: true })),
      async () => ok(event({ ...inventory, secret: 'synthetic-private' })),
      async () =>
        ok(event({ ...inventory, candidates: [{ ...candidate, token: 'synthetic-private' }] })),
      async () =>
        ok(
          event({
            ...inventory,
            candidates: [{ ...candidate, identity: { kind: 'muse-team', key: '2'.repeat(64) } }],
          })
        ),
      async () => ok(event({ ...inventory, candidates: [{ ...candidate, plan: 'bad\tplan' }] })),
      async () =>
        ok(
          event({ ...inventory, candidates: [{ ...candidate, selectors: ['invalid selector'] }] })
        ),
      async () =>
        ok(event({ ...inventory, candidates: [], signInNeededSelectors: ['bad selector'] })),
      async () =>
        ok(
          event({
            version: 1,
            action: 'collect',
            candidate: null,
            sample: null,
            failureCode: 'unavailable',
          })
        ),
    ]) {
      let calls = 0;
      expect(
        await collectAgentWithFallback(
          'failover-inventory',
          () => invoke(run),
          async () => {
            calls++;
            return inventory;
          }
        )
      ).toEqual(inventory);
      expect(calls).toBe(1);
    }
  });
  it('keeps incomplete selectors and rejects signed-out selectors for other providers', async () => {
    expect(
      await invoke(async () =>
        ok(event({ ...inventory, incompleteSelectors: ['claude-profile:*'] }))
      )
    ).toEqual({ ...inventory, incompleteSelectors: ['claude-profile:*'] });
    let calls = 0;
    await collectAgentWithFallback(
      'failover-inventory',
      () =>
        invoke(
          async () =>
            ok(
              event({
                ...inventory,
                candidates: [],
                signInNeededSelectors: ['claude-profile:fixture'],
              })
            ),
          { ...request, provider: 'muse' }
        ),
      async () => {
        calls++;
        return inventory;
      }
    );
    expect(calls).toBe(1);
  });
  it('delegates off, empty catalogs and collection before launch', async () => {
    let calls = 0;
    const run = async () => {
      calls++;
      return ok('');
    };
    const deadline = Date.now() + 10000;
    expect(
      await runFailoverInventoryAgent(host, request, deadline, {
        artifacts: [artifact],
        mode: 'off',
        run,
      })
    ).toBeNull();
    expect(
      await runFailoverInventoryAgent(host, request, deadline, { artifacts: [], mode: 'on', run })
    ).toBeNull();
    expect(
      await runFailoverInventoryAgent(
        host,
        { ...request, action: 'collect', candidate },
        deadline,
        { artifacts: [artifact], mode: 'on', run }
      )
    ).toBeNull();
    expect(calls).toBe(0);
  });
  it('retains host and caller-deadline checks before launching', async () => {
    let calls = 0;
    const run = async () => {
      calls++;
      return ok(event(inventory));
    };
    const options = { artifacts: [artifact], mode: 'on' as const, run };
    await expect(
      runFailoverInventoryAgent(
        { ...host, sshHost: 'bad;host' },
        request,
        Date.now() + 10000,
        options
      )
    ).rejects.toThrow('Invalid usage host');
    await expect(runFailoverInventoryAgent(host, request, Date.now() - 1, options)).rejects.toThrow(
      'Usage source unavailable'
    );
    expect(calls).toBe(0);
  });
});
