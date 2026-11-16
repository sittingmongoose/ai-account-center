import { describe, expect, it } from 'bun:test';
import { createHash } from 'crypto';
import {
  collectAgentWithFallback,
  runPinsAgent,
  runKeyStoreAgent,
  runHistoryAgent,
} from '../../../src/web-server/services/agent-verbs';
import type { ManagedCommandResult } from '../../../src/utils/managed-process';

const host = { platform: 'mac' as const, sshHost: 'fixture-mac' };
const artifact = {
  target: 'aarch64-apple-darwin',
  version: '0.1.0',
  sha256: 'a'.repeat(64),
  file: 'unused',
};
const ok = (stdout: string): ManagedCommandResult => ({
  stdout,
  code: 0,
  failed: false,
  missing: false,
  timedOut: false,
});
const secret = 'synthetic-fixture-key-1234';
const keyArgs = { action: 'put', provider: 'zai', keyId: '12345678', secret };
const summary = {
  ok: true,
  fingerprint: `sha256:${createHash('sha256').update(secret).digest('hex').slice(0, 16)}`,
  last4: '1234',
};
const verbs = ['t3-pins', 'key-store', 'claude-history'] as const;
function invoke(
  verb: (typeof verbs)[number],
  run: (...args: any[]) => Promise<ManagedCommandResult>
) {
  const opts = { artifacts: [artifact], mode: 'on' as const, run };
  return verb === 't3-pins'
    ? runPinsAgent(host, opts)
    : verb === 'key-store'
      ? runKeyStoreAgent(host, keyArgs, opts)
      : runHistoryAgent(host, { mode: 'closed-check' }, opts);
}
for (const verb of verbs)
  describe(verb, () => {
    it('uses the verified stdin protocol without private argv or environment', async () => {
      const result = await invoke(verb, async (file, args, options) => {
        expect(file).toMatch(/(^|[\/])ssh(\.exe)?$/);
        expect(args.join(' ')).toContain(artifact.sha256);
        expect(args.join(' ')).not.toContain(secret);
        expect(JSON.stringify(options.env)).not.toContain(secret);
        const request = JSON.parse(options.input);
        expect(request.verb).toBe(verb);
        if (verb === 'key-store') expect(request.args).toEqual(keyArgs);
        return ok(
          JSON.stringify({
            type: 'result',
            protocol: 1,
            id: verb,
            value: verb === 't3-pins' ? [] : verb === 'key-store' ? summary : { closed: true },
          })
        );
      });
      expect(result).not.toBeNull();
    });
    it('runs the reference exactly once for every failed, malformed or delegated agent', async () => {
      for (const run of [
        async () => {
          throw new Error('fixture spawn');
        },
        async () => ({ ...ok(''), code: 87 }),
        async () => ({ ...ok(''), code: 1 }),
        async () => ({ ...ok(''), timedOut: true }),
        async () => ({ ...ok(''), missing: true }),
        async () => ok('invalid'),
        async () => ok('{}'),
        async () => ok(JSON.stringify({ type: 'result', protocol: 1, id: 'wrong', value: {} })),
        async () =>
          ok(
            JSON.stringify({ type: 'result', protocol: 1, id: verb, value: { fallback: 'python' } })
          ),
        async () =>
          ok(JSON.stringify({ type: 'result', protocol: 1, id: verb, value: { secret } })),
      ]) {
        let calls = 0;
        const result = await collectAgentWithFallback(
          verb,
          () => invoke(verb, run),
          async () => {
            calls++;
            return 'reference';
          }
        );
        expect(result).toBe('reference');
        expect(calls).toBe(1);
      }
    });
  });
it('rejects reflected key material, mismatched bindings and forged summaries', async () => {
  for (const value of [
    { ...summary, secret },
    { ...summary, last4: 'nope' },
    { ...summary, fingerprint: 'sha256:' + '0'.repeat(16) },
  ]) {
    expect(
      await runKeyStoreAgent(host, keyArgs, {
        artifacts: [artifact],
        mode: 'on',
        run: async () =>
          ok(JSON.stringify({ type: 'result', protocol: 1, id: 'key-store', value })),
      })
    ).toBeNull();
  }
});
it('delegates history mutations before launching an agent', async () => {
  for (const mode of ['append', 'snapshot-cleanup', 'verify-transcripts']) {
    let calls = 0;
    expect(
      await runHistoryAgent(
        host,
        { mode },
        {
          artifacts: [artifact],
          mode: 'on',
          run: async () => {
            calls++;
            return ok('');
          },
        }
      )
    ).toBeNull();
    expect(calls).toBe(0);
  }
});
it('off and an empty catalog never execute any of the three verbs', async () => {
  let calls = 0;
  const run = async () => {
    calls++;
    return ok('');
  };
  for (const mode of ['off', 'on'] as const) {
    const opts = { mode, artifacts: mode === 'on' ? [] : [artifact], run };
    expect(await runPinsAgent(host, opts)).toBeNull();
    expect(await runKeyStoreAgent(host, keyArgs, opts)).toBeNull();
    expect(await runHistoryAgent(host, { mode: 'collect' }, opts)).toBeNull();
  }
  expect(calls).toBe(0);
});
