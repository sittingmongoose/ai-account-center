/** Scan selection, verification and fallback use fake commands only. */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { createHash } from 'crypto';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  localAgentReady,
  runAgentVerb,
  type AgentArtifact,
} from '../../../src/web-server/services/agent-verbs';
import {
  localUsageScanReady,
  runAnalyticsRemoteScan,
  runLocalUsageScan,
  usageScanRequest,
  type AnalyticsRemoteRequest,
} from '../../../src/web-server/services/analytics-remote-transport';
import type { ManagedCommandResult } from '../../../src/utils/managed-process';

const artifact: AgentArtifact = {
  target: 'x86_64-unknown-linux-musl',
  version: '0.1.0',
  sha256: 'a'.repeat(64),
  file: 'unused',
};
const request: AnalyticsRemoteRequest = { kinds: ['codex'], minDateMs: 1000, fingerprints: {} };
const answer = {
  version: 2,
  truncated: false,
  kinds: { codex: { state: 'not_installed', fingerprints: {} } },
  rows: [],
  srows: [],
  reader: { name: 'aac-usage-scan', version: '0.1.0', protocol: 3, target: artifact.target },
};
const ok = (stdout: string): ManagedCommandResult => ({
  stdout,
  code: 0,
  failed: false,
  missing: false,
  timedOut: false,
});
const event = (value: unknown, extra = {}) =>
  JSON.stringify({ type: 'result', protocol: 1, id: 'scan-usage', value, ...extra });
let dir: string;
let ccs: string | undefined;
beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-scan-agent-'));
  ccs = process.env.CCS_HOME;
  process.env.CCS_HOME = path.join(dir, 'ccs');
  // SSH argv builders need an identity in the home; a placeholder, never a key.
  fs.mkdirSync(path.join(dir, 'ccs', '.ssh'), { recursive: true });
  fs.writeFileSync(path.join(dir, 'ccs', '.ssh', 'id_ed25519'), 'fixture placeholder, not a key\n');
});
afterEach(() => {
  if (ccs === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = ccs;
  fs.rmSync(dir, { recursive: true, force: true });
});

describe('scan-usage opt-in', () => {
  it('keeps the real local scan caller inside the service deadline slack', async () => {
    let budget: { deadlineMs: number; timeoutMs?: number } | undefined;
    const result = await runLocalUsageScan(request, {
      scanner: { ok: false, cause: 'binary_not_built' },
      agent: async (_host, _verb, _args, options) => {
        budget = options;
        return JSON.stringify(answer);
      },
    });
    expect(result.reader?.name).toBe('aac-usage-scan');
    expect(budget!.deadlineMs).toBe(45000);
    expect(budget!.timeoutMs).toBe(50000);
  });
  it('runs a verified remote agent with the identical scanner request and no helper', async () => {
    const result = await runAnalyticsRemoteScan('nas1-agent', 'nas1', request, {
      manifest: null,
      python: async () => {
        throw new Error('unexpected helper');
      },
      agent: (host, verb, args, options) =>
        runAgentVerb(host, verb, args, {
          ...options,
          mode: 'on',
          artifacts: [artifact],
          run: async (file, argv, opts) => {
            expect(file).toBe('ssh');
            expect(argv.join(' ')).toContain(artifact.sha256);
            const payload = JSON.parse(opts.input!);
            expect(payload.verb).toBe('scan-usage');
            expect(payload.args).toEqual(usageScanRequest(request, 20000));
            expect(opts.maxBytes).toBe(8 * 1024 * 1024 + 1024);
            return ok(event(answer));
          },
        }),
    });
    expect(result.reader?.name).toBe('aac-usage-scan');
    expect(result.kinds.codex?.state).toBe('not_installed');
  });

  it('uses the labelled Python path for every failed or malformed agent result', async () => {
    const outcomes = [
      async () => {
        throw new Error('spawn');
      },
      async () => ({ ...ok(''), code: 87 }),
      async () => ({ ...ok(''), code: 1 }),
      async () => ({ ...ok(''), failed: true }),
      async () => ({ ...ok(''), timedOut: true }),
      async () => ({ ...ok(''), missing: true }),
      async () => ok('not-json'),
      async () => ok(event(answer, { type: 'failure' })),
      async () => ok(event(answer, { protocol: 2 })),
      async () => ok(event(answer, { id: 'other' })),
      async () => ok(event({ fallback: 'python' })),
      async () => ok(event({ ...answer, kinds: {} })),
      async () => ok(event({ ...answer, rows: 'bad' })),
      async () => ok(event(answer) + '\n' + event(answer)),
      async () => ok('x'.repeat(8 * 1024 * 1024 + 1)),
    ];
    for (const run of outcomes) {
      let calls = 0;
      const result = await runAnalyticsRemoteScan('nas1-agent', 'nas1', request, {
        manifest: null,
        agent: (host, verb, args, options) =>
          runAgentVerb(host, verb, args, { ...options, mode: 'on', artifacts: [artifact], run }),
        python: async () => {
          calls++;
          return {
            ...answer,
            version: 2 as const,
            kinds: { codex: { state: 'not_installed' as const, fingerprints: {} } },
            reader: undefined,
          };
        },
      });
      expect(calls).toBe(1);
      expect(result.reader).toEqual({ name: 'python-fallback', cause: 'binary_not_built' });
    }
  });

  it('keeps off and missing artifacts from executing', async () => {
    let calls = 0;
    for (const mode of ['off', 'on'] as const) {
      expect(
        await runAgentVerb(
          { platform: 'ubuntu', sshHost: 'nas1-agent' },
          'scan-usage',
          {},
          {
            mode,
            artifacts: mode === 'off' ? [artifact] : [],
            deadlineMs: 1000,
            maxBytes: 8 * 1024 * 1024,
            run: async () => {
              calls++;
              return ok('');
            },
          }
        )
      ).toBeNull();
    }
    expect(calls).toBe(0);
    expect(localUsageScanReady({ ok: false, cause: 'binary_not_built' }).ok).toBe(false);
  });

  it('verifies local bytes and handshake before selecting the agent', async () => {
    const file = path.join(dir, 'agent');
    fs.writeFileSync(file, 'synthetic binary');
    const local = {
      ...artifact,
      file,
      sha256: createHash('sha256').update('synthetic binary').digest('hex'),
    };
    expect(localAgentReady('ubuntu', [local], 'on')).toBe(true);
    expect(localAgentReady('ubuntu', [artifact], 'on')).toBe(false);
    let versions = 0;
    let runs = 0;
    const agent: typeof runAgentVerb = (host, verb, args, options) =>
      runAgentVerb(host, verb, args, {
        ...options,
        mode: 'on',
        artifacts: [local],
        run: async (exe, argv, opts) => {
          expect(exe).toBe(file);
          if (argv[0] === '--version') {
            versions++;
            return ok(
              JSON.stringify({
                name: 'aac-agent',
                version: local.version,
                protocol: 1,
                target: local.target,
              })
            );
          }
          runs++;
          expect(JSON.parse(opts.input!).args).toEqual(usageScanRequest(request, 40000));
          return ok(event(answer));
        },
      });
    expect(
      (
        await runLocalUsageScan(request, {
          scanner: { ok: false, cause: 'binary_not_built' },
          agent,
        })
      ).reader?.name
    ).toBe('aac-usage-scan');
    expect(versions).toBe(1);
    expect(runs).toBe(1);
    fs.appendFileSync(file, 'damaged');
    expect(localAgentReady('ubuntu', [local], 'on')).toBe(false);
    await expect(
      runLocalUsageScan(request, { scanner: { ok: false, cause: 'binary_not_built' }, agent })
    ).rejects.toThrow();
    expect(versions).toBe(1);
    expect(runs).toBe(1);
  });
});
