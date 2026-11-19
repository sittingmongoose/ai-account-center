/** Per-family verb modes (DESIGN 4.5) and shadow comparison: fake closures only, no binary. */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  agentVerbMode,
  agentVerbs,
  collectAgentWithFallback,
  collectQuotaWithFallback,
  collectSignInWithFallback,
  localAgentReady,
  runAgentShadow,
  type AgentShadowReport,
} from '../../../src/web-server/services/agent-verbs';
import {
  runAnalyticsRemoteScan,
  type AnalyticsRemoteRequest,
} from '../../../src/web-server/services/analytics-remote-transport';
import { dump } from 'js-yaml';
import { createEmptyUnifiedConfig } from '../../../src/config/schemas/unified-config';

const ENV_KEYS = ['AAC_AGENT_VERBS', 'AAC_AGENT_VERB_FAMILIES', 'CCS_HOME'] as const;
let saved: Record<string, string | undefined>;
let home: string;
beforeEach(() => {
  saved = Object.fromEntries(ENV_KEYS.map((key) => [key, process.env[key]]));
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-verb-families-'));
  process.env.CCS_HOME = home;
  delete process.env.AAC_AGENT_VERBS;
  delete process.env.AAC_AGENT_VERB_FAMILIES;
});
afterEach(() => {
  for (const key of ENV_KEYS) {
    const value = saved[key];
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
  }
  fs.rmSync(home, { recursive: true, force: true });
});

const ALL_VERBS = [
  'inventory',
  'signin-expiry',
  'quota-read',
  'scan-usage',
  't3-pins',
  'key-store',
  'claude-history',
  'failover-inventory',
] as const;

describe('per-family verb modes', () => {
  it('absent and invalid ceilings stay off', () => {
    expect(agentVerbs()).toBe('off');
    expect(agentVerbMode('signin-expiry')).toBe('off');
    process.env.AAC_AGENT_VERBS = 'loud';
    expect(agentVerbs()).toBe('off');
    expect(agentVerbMode('quota-read')).toBe('off');
  });
  it('a scalar on keeps every verb on, as before the family map', () => {
    process.env.AAC_AGENT_VERBS = 'on';
    for (const verb of ALL_VERBS) expect(agentVerbMode(verb)).toBe('on');
  });
  it('a family map turns on only its listed families, one at a time', () => {
    process.env.AAC_AGENT_VERBS = 'on';
    process.env.AAC_AGENT_VERB_FAMILIES = JSON.stringify({
      'signin-expiry': 'on',
      'quota-read': 'shadow',
    });
    expect(agentVerbMode('signin-expiry')).toBe('on');
    expect(agentVerbMode('quota-read')).toBe('shadow');
    expect(agentVerbMode('t3-pins')).toBe('off');
    expect(agentVerbMode('scan-usage')).toBe('off');
    expect(agentVerbMode('failover-inventory')).toBe('off');
  });
  it('the scalar ceiling caps a family setting', () => {
    process.env.AAC_AGENT_VERB_FAMILIES = JSON.stringify({ 'signin-expiry': 'on' });
    process.env.AAC_AGENT_VERBS = 'shadow';
    expect(agentVerbMode('signin-expiry')).toBe('shadow');
    process.env.AAC_AGENT_VERBS = 'off';
    expect(agentVerbMode('signin-expiry')).toBe('off');
  });
  it('a present but invalid map keeps every family off, never the ceiling', () => {
    process.env.AAC_AGENT_VERBS = 'on';
    for (const bad of ['not json', '{"nope":"on"}', '{"signin-expiry":"loud"}', '[]', '3']) {
      process.env.AAC_AGENT_VERB_FAMILIES = bad;
      for (const verb of ALL_VERBS) expect(agentVerbMode(verb)).toBe('off');
    }
  });
  it('write families never shadow; an explicit on still works', () => {
    process.env.AAC_AGENT_VERBS = 'shadow';
    process.env.AAC_AGENT_VERB_FAMILIES = JSON.stringify({
      'key-store': 'shadow',
      'claude-history': 'shadow',
      inventory: 'shadow',
    });
    expect(agentVerbMode('key-store')).toBe('off');
    expect(agentVerbMode('claude-history')).toBe('off');
    expect(agentVerbMode('inventory')).toBe('shadow');
    process.env.AAC_AGENT_VERBS = 'on';
    process.env.AAC_AGENT_VERB_FAMILIES = JSON.stringify({ 'key-store': 'on' });
    expect(agentVerbMode('key-store')).toBe('on');
  });
  it('an unreadable config leaves the scalar ceiling intact', () => {
    fs.mkdirSync(path.join(home, '.ccs'), { recursive: true });
    fs.writeFileSync(path.join(home, '.ccs', 'config.yaml'), '- not a mapping\n');
    process.env.AAC_AGENT_VERBS = 'on';
    expect(agentVerbMode('signin-expiry')).toBe('on');
    expect(agentVerbMode('scan-usage')).toBe('on');
  });
  it('a valid config family map gates without environment overrides', () => {
    const config = {
      ...createEmptyUnifiedConfig(),
      agentVerbs: 'on' as const,
      agentVerbFamilies: { 'signin-expiry': 'shadow' as const },
    };
    fs.mkdirSync(path.join(home, '.ccs'), { recursive: true });
    fs.writeFileSync(path.join(home, '.ccs', 'config.yaml'), dump(config));
    expect(agentVerbMode('signin-expiry')).toBe('shadow');
    expect(agentVerbMode('quota-read')).toBe('off');
  });
  it('admits the glibc Linux artifact and prefers musl when both exist', () => {
    const gnu = { target: 'x86_64-unknown-linux-gnu', version: '0.1.0', sha256: 'b'.repeat(64), file: 'unused' };
    const musl = { target: 'x86_64-unknown-linux-musl', version: '0.1.0', sha256: 'c'.repeat(64), file: 'unused' };
    process.env.AAC_AGENT_TRANSPORT = 'on';
    try {
      expect(localAgentReady('ubuntu', [gnu], 'on')).toBe(true);
      expect(localAgentReady('ubuntu', [gnu], 'off')).toBe(false);
      expect(localAgentReady('ubuntu', [gnu, musl], 'on')).toBe(true);
      expect(localAgentReady('mac', [gnu], 'on')).toBe(false);
    } finally {
      delete process.env.AAC_AGENT_TRANSPORT;
    }
  });
});

describe('shadow comparison at the collector boundaries', () => {
  it('serves the Python answer without waiting for the agent, and compares key-order-insensitively', async () => {
    const agentGate = Promise.withResolvers<string | null>();
    const compared = Promise.withResolvers<AgentShadowReport>();
    const pythonAnswer = '{"schemaVersion":1,"profiles":[]}';
    let agentCalls = 0;
    const result = await collectSignInWithFallback(
      () => {
        agentCalls++;
        return agentGate.promise;
      },
      async () => pythonAnswer,
      'shadow',
      (report) => compared.resolve(report)
    );
    // The served answer arrived while the agent was still pending.
    expect(result).toBe(pythonAnswer);
    expect(agentCalls).toBe(1);
    agentGate.resolve('{"profiles":[],"schemaVersion":1}');
    const report = await compared.promise;
    expect(report.verb).toBe('signin-expiry');
    expect(report.outcome).toBe('equal');
  });
  it('a different agent answer is a difference, and Python is still served', async () => {
    const compared = Promise.withResolvers<AgentShadowReport>();
    const result = await collectSignInWithFallback(
      async () => '{"schemaVersion":1,"profiles":[{"id":"x"}]}',
      async () => '{"schemaVersion":1,"profiles":[]}',
      'shadow',
      (report) => compared.resolve(report)
    );
    expect(result).toBe('{"schemaVersion":1,"profiles":[]}');
    expect((await compared.promise).outcome).toBe('different');
  });
  it('agent fallback and agent error are not differences', async () => {
    const fallback = Promise.withResolvers<AgentShadowReport>();
    await collectSignInWithFallback(
      async () => null,
      async () => 'reference',
      'shadow',
      (report) => fallback.resolve(report)
    );
    expect((await fallback.promise).outcome).toBe('fallback');
    const error = Promise.withResolvers<AgentShadowReport>();
    await collectSignInWithFallback(
      async () => {
        throw new Error('spawn failed');
      },
      async () => 'reference',
      'shadow',
      (report) => error.resolve(report)
    );
    expect((await error.promise).outcome).toBe('error');
  });
  it('a failing reference propagates unchanged and is logged as reference-failed', async () => {
    const compared = Promise.withResolvers<AgentShadowReport>();
    const failure = new Error('helper failed');
    await expect(
      collectSignInWithFallback(
        async () => 'agent answer',
        async () => {
          throw failure;
        },
        'shadow',
        (report) => compared.resolve(report)
      )
    ).rejects.toBe(failure);
    expect((await compared.promise).outcome).toBe('reference-failed');
  });
  it('quota shadow compares the same way', async () => {
    const compared = Promise.withResolvers<AgentShadowReport>();
    const result = await collectQuotaWithFallback(
      async () => '{"id":"plan-qwen-ubuntu","provider":"qwen"}',
      async () => '{"provider":"qwen","id":"plan-qwen-ubuntu"}',
      'shadow',
      (report) => compared.resolve(report)
    );
    expect(result).toBe('{"provider":"qwen","id":"plan-qwen-ubuntu"}');
    const report = await compared.promise;
    expect(report.verb).toBe('quota-read');
    expect(report.outcome).toBe('equal');
  });
  it('object answers compare by key order; array order stays a difference', async () => {
    const equal = Promise.withResolvers<AgentShadowReport>();
    const helperValue = { action: 'inventory', candidates: [{ id: 'a' }] };
    const served = await collectAgentWithFallback(
      'failover-inventory',
      async () => ({ candidates: [{ id: 'a' }], action: 'inventory' }),
      async () => helperValue,
      'shadow',
      (report) => equal.resolve(report)
    );
    expect(served).toBe(helperValue);
    expect((await equal.promise).outcome).toBe('equal');
    const different = Promise.withResolvers<AgentShadowReport>();
    await collectAgentWithFallback(
      't3-pins',
      async () => '["b","a"]',
      async () => '["a","b"]',
      'shadow',
      (report) => different.resolve(report)
    );
    expect((await different.promise).outcome).toBe('different');
  });
  it('off and on keep the legacy agent-first boundary', async () => {
    let pythonCalls = 0;
    const onAnswer = await collectSignInWithFallback(
      async () => 'agent',
      async () => {
        pythonCalls++;
        return 'python';
      },
      'on'
    );
    expect(onAnswer).toBe('agent');
    expect(pythonCalls).toBe(0);
    // off: the gated agent closure returns null (runSignInAgent's own gate); Python answers,
    // and no shadow comparison runs.
    let reports = 0;
    const offAnswer = await collectSignInWithFallback(
      async () => null,
      async () => {
        pythonCalls++;
        return 'python';
      },
      'off',
      () => {
        reports++;
      }
    );
    expect(offAnswer).toBe('python');
    expect(pythonCalls).toBe(1);
    expect(reports).toBe(0);
  });
});

describe('scan-usage shadow', () => {
  const request: AnalyticsRemoteRequest = { kinds: ['codex'], minDateMs: 1000, fingerprints: {} };
  const scanAnswer = {
    version: 2,
    truncated: false,
    kinds: { codex: { state: 'not_installed', fingerprints: {} } },
    rows: [],
    srows: [],
    reader: { name: 'aac-usage-scan', version: '0.1.0', protocol: 3, target: 'x86_64-unknown-linux-gnu' },
  };
  it('serves the reference path and runs the agent alongside, never the agent answer', async () => {
    process.env.AAC_AGENT_VERBS = 'shadow';
    process.env.AAC_AGENT_VERB_FAMILIES = JSON.stringify({ 'scan-usage': 'shadow' });
    let agentCalls = 0;
    const result = await runAnalyticsRemoteScan('nas1-agent', 'nas1', request, {
      manifest: null,
      python: async () => ({
        version: 2,
        truncated: false,
        kinds: { codex: { state: 'not_installed', fingerprints: {} } },
        rows: [],
        srows: [],
      }),
      agent: async () => {
        agentCalls++;
        return JSON.stringify(scanAnswer);
      },
    });
    expect(agentCalls).toBe(1);
    expect(result.reader?.name).toBe('python-fallback');
  });
  it('a python-fallback reference is reference-fallback, not a difference', async () => {
    const compared = Promise.withResolvers<AgentShadowReport>();
    const reference = {
      version: 2 as const,
      truncated: false,
      kinds: {},
      rows: [],
      srows: [],
      reader: { name: 'python-fallback', cause: 'binary_not_built' },
    };
    const served = await runAgentShadow(
      'scan-usage',
      async () => scanAnswer,
      async () => reference,
      (report) => compared.resolve(report),
      (answer) => answer.reader?.name === 'aac-usage-scan'
    );
    expect(served).toBe(reference);
    expect((await compared.promise).outcome).toBe('reference-fallback');
  });
});
