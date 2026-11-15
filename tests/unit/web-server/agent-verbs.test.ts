import { describe, expect, it } from 'bun:test';
import { createHash } from 'crypto';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  agentLauncher,
  runQuotaAgent,
  collectQuotaWithFallback,
  runSignInAgent,
  collectSignInWithFallback,
  runInventoryAgent,
} from '../../../src/web-server/services/agent-verbs';
import type { UsageHost } from '../../../src/web-server/services/usage-failover-contract';
import type { ManagedCommandResult } from '../../../src/utils/managed-process';
const host: UsageHost = { machine: 'fixture', platform: 'linux', sshHost: 'fixture' };
const artifact = {
  target: 'x86_64-unknown-linux-musl',
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
describe('agent verb opt-in', () => {
  it('defaults to Python without a trusted artifact, and off never launches', async () => {
    let calls = 0;
    const run = async () => {
      calls++;
      return ok('');
    };
    expect(await runSignInAgent(host, [], run, 'on')).toBeNull();
    expect(await runSignInAgent(host, [artifact], run, 'off')).toBeNull();
    expect(calls).toBe(0);
  });
  it('verifies the remote hash and sends one bounded NDJSON request', async () => {
    const result = await runSignInAgent(
      host,
      [artifact],
      async (file, args, options) => {
        expect(file).toBe('ssh');
        expect(args.join(' ')).toContain(artifact.sha256);
        const request = JSON.parse(options.input!);
        expect(request.verb).toBe('signin-expiry');
        expect(request.args).toEqual({});
        return ok(
          JSON.stringify({
            type: 'result',
            protocol: 1,
            id: request.id,
            value: { schemaVersion: 1, profiles: [] },
          })
        );
      },
      'on'
    );
    expect(JSON.parse(result!)).toEqual({ schemaVersion: 1, profiles: [] });
  });
  it('invokes the Python collector after every agent failure or fallback reply', async () => {
    const outcomes = [
      async () => {
        throw new Error('spawn failed');
      },
      async () => ({ ...ok(''), code: 87, failed: true }),
      async () => ({ ...ok(''), code: 1, failed: true }),
      async () => ({ ...ok(''), code: 1 }),
      async () => ({ ...ok(''), timedOut: true }),
      async () => ({ ...ok(''), missing: true }),
      async () => ok('broken json'),
      async () => ok('{"type":"failure","protocol":1,"id":"signin-expiry"}'),
      async () => ok('{"type":"result","protocol":1,"id":"wrong"}'),
      async () =>
        ok('{"type":"result","protocol":1,"id":"signin-expiry","value":{"fallback":"python"}}'),
    ];
    for (const run of outcomes) {
      let pythonCalls = 0;
      const result = await collectSignInWithFallback(
        () => runSignInAgent(host, [artifact], run, 'on'),
        async () => {
          pythonCalls++;
          return 'python fixture report';
        }
      );
      expect(result).toBe('python fixture report');
      expect(pythonCalls).toBe(1);
    }
    let pythonCalls = 0;
    expect(
      await collectSignInWithFallback(
        async () => {
          throw new Error('verify failed');
        },
        async () => {
          pythonCalls++;
          return 'python';
        }
      )
    ).toBe('python');
    expect(pythonCalls).toBe(1);
  });
  it('rejects a corrupt local binary before executing it', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-agent-adapter-'));
    try {
      const file = path.join(dir, 'agent');
      fs.writeFileSync(file, 'fixture binary');
      let calls = 0;
      expect(
        await runSignInAgent(
          { ...host, sshHost: null },
          [{ ...artifact, file }],
          async () => {
            calls++;
            return ok('');
          },
          'on'
        )
      ).toBeNull();
      expect(calls).toBe(0);
      const sha256 = createHash('sha256').update('fixture binary').digest('hex');
      expect(
        await runSignInAgent(
          { ...host, sshHost: null },
          [{ ...artifact, file, sha256 }],
          async () => ok('{"name":"wrong"}'),
          'on'
        )
      ).toBeNull();
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
  it('keeps remote paths fixed for both launcher dialects', () => {
    expect(agentLauncher(artifact, 'windows')).toStartWith(
      'powershell.exe -NoProfile -NonInteractive -EncodedCommand '
    );
    expect(agentLauncher(artifact, 'mac')).toContain(
      '$HOME/.cache/ai-account-center/bin/aac-agent'
    );
  });
});

describe('plan quota agent', () => {
  it('keeps the same provider, platform and credential selector on the wire', async () => {
    const args = {
      provider: 'zai',
      platform: 'ubuntu',
      account: 'zai:acct:12345678',
      credential: { kind: 'aac-key', keyId: '12345678' },
    };
    const value = {
      platform: 'ubuntu',
      id: args.account,
      provider: 'zai',
      status: 'ok',
      windows: [],
      capabilities: {},
    };
    const result = await runQuotaAgent(
      host,
      args,
      [artifact],
      async (_file, _args, options) => {
        const request = JSON.parse(options.input!);
        expect(request.verb).toBe('quota-read');
        expect(request.args).toEqual(args);
        return ok(JSON.stringify({ type: 'result', protocol: 1, id: request.id, value }));
      },
      'on'
    );
    expect(JSON.parse(result!)).toEqual(value);
  });
  it('falls back once for unavailable, failed and invalid agents', async () => {
    for (const run of [
      async () => {
        throw new Error('spawn');
      },
      async () => ({ ...ok(''), timedOut: true }),
      async () => ok('{}'),
      async () =>
        ok(
          JSON.stringify({
            type: 'result',
            protocol: 1,
            id: 'quota-read',
            value: { fallback: 'python' },
          })
        ),
      async () =>
        ok(
          JSON.stringify({ type: 'result', protocol: 1, id: 'quota-read', value: { status: 'ok' } })
        ),
    ]) {
      let calls = 0;
      const result = await collectQuotaWithFallback(
        () => runQuotaAgent(host, { provider: 'qwen' }, [artifact], run, 'on'),
        async () => {
          calls++;
          return 'python';
        }
      );
      expect(result).toBe('python');
      expect(calls).toBe(1);
    }
  });

  it('caps a status inventory attempt at 8s so the probe fallback keeps a budget', async () => {
    let timeoutMs = 0;
    let deadlineMs = 0;
    const run = async (
      _executable: string,
      _args: string[],
      options: { timeoutMs: number; input?: string }
    ) => {
      timeoutMs = options.timeoutMs;
      deadlineMs = JSON.parse(options.input ?? '{}').deadlineMs ?? 0;
      return ok(
        `${JSON.stringify({
          protocol: 1,
          id: 'inventory',
          type: 'result',
          value: { schemaVersion: 1, machine: { cores: 1 } },
        })}\n`
      );
    };
    const value = await runInventoryAgent({ platform: 'linux', sshHost: 'fixture' }, () => true, {
      run,
      artifacts: [artifact],
      mode: 'on',
      transport: 'off',
    });
    expect(timeoutMs).toBe(8000);
    // The agent's deadline ends before the kill, so it can answer for itself (Lows 3 review L3).
    expect(deadlineMs).toBeGreaterThan(0);
    expect(deadlineMs).toBeLessThan(timeoutMs);
    expect(value).toEqual({ cores: 1 });
  });
});
