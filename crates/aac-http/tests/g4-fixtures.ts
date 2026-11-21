/**
 * G4 parity fixture generator: drives the TypeScript UsageFailoverService on fixture scenarios
 * (no provider calls) and records the inputs plus the exact outputs — the returned dashboard row,
 * the helper collect-request hosts, and the cooldown/observation files. The Rust integration test
 * (g4_parity.rs) replays each scenario through the native coordinator and must match byte for byte.
 *
 * Run from the repo root:  bun crates/aac-http/tests/g4-fixtures.ts
 */
import fs from 'fs/promises';
import os from 'os';
import path from 'path';
import { UsageFailoverService } from '../../../src/web-server/services/usage-failover-service';
import { musePeerBindingFile } from '../../../src/web-server/services/muse-usage-peer-bindings';
import type { DashboardAccount } from '../../../src/web-server/services/account-dashboard-types';
import type {
  UsageCandidate, UsageCollection, UsageFailoverTarget, UsageHelperRequest,
  UsageHelperResponse, UsageHost, UsageReading,
} from '../../../src/web-server/services/usage-failover-contract';

const BASE_TIME = Date.parse('2026-10-14T12:00:00.000Z');
const MAC: UsageHost = { host: 'mac', platform: 'mac', sshHost: 'fixture-mac' };
const WINDOWS: UsageHost = { host: 'windows', platform: 'windows', sshHost: 'fixture-windows' };
const UBUNTU: UsageHost = { host: 'ubuntu', platform: 'ubuntu', sshHost: 'fixture-ubuntu' };

function candidate(overrides: Partial<UsageCandidate> = {}): UsageCandidate {
  return {
    locator: '1'.repeat(64), selectors: ['discover'],
    identity: { kind: 'muse-team', key: 'a'.repeat(64) },
    revision: '2'.repeat(64), email: 'same@example.test', plan: 'Pro', ...overrides,
  };
}
function reading(at = BASE_TIME, overrides: Partial<UsageReading> = {}): UsageReading {
  return {
    status: 'ok', email: 'same@example.test', plan: 'Pro',
    fetchedAt: new Date(at).toISOString(), sampledAt: new Date(at).toISOString(),
    windows: [{ key: 'weekly', label: 'Weekly', usedPercent: 0, remainingPercent: 100, used: 0, limit: 100, resetAt: '2026-10-18T12:00:00.000Z', windowMinutes: 10080, unit: 'credits' }],
    ...overrides,
  };
}
function template(): DashboardAccount {
  return {
    id: 'muse:usage', provider: 'muse', providerLabel: 'Muse Code', label: 'Personal Muse',
    email: null, plan: null, platform: 'mac', source: 'Account on Mac', status: 'unavailable',
    message: null, fetchedAt: null, sampledAt: null, windows: [], isActive: false,
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
  };
}
function museTarget(ccsDir: string, overrides: Partial<UsageFailoverTarget> = {}): UsageFailoverTarget {
  return { ccsDir, accountId: 'muse:usage', provider: 'muse', binding: 'fixture-source-and-credential', primarySources: [{ ...MAC, selector: 'discover' }], ...overrides };
}

interface Step {
  op: 'collect' | 'recover' | 'peek' | 'holdCooldown' | 'cooldownUntil';
  now: number;
  restart?: boolean;
  inventories: Record<string, UsageCandidate[] | 'error'>;
  collections: Record<string, UsageCollection | 'error'>;
  hosts?: UsageHost[];
  signInNeeded?: Record<string, string[]>;
  incomplete?: Record<string, string[]>;
  musePeers?: Array<{ host: string; identity: UsageCandidate['identity'] }>;
  holdUntil?: number;
  template?: DashboardAccount;
}

async function runScenario(name: string, target: UsageFailoverTarget, baseTemplate: DashboardAccount, steps: Step[]) {
  const ccsDir = path.join(os.tmpdir(), 'aac-g4-parity', name);
  await fs.rm(ccsDir, { recursive: true, force: true });
  await fs.mkdir(ccsDir, { recursive: true });
  target.ccsDir = ccsDir;
  let now = BASE_TIME;
  let hosts: UsageHost[] = [MAC, UBUNTU, WINDOWS];
  const inventories = new Map<string, UsageCandidate[] | Error>();
  const collections = new Map<string, UsageCollection | Error>();
  const signInNeeded = new Map<string, string[]>();
  const incomplete = new Map<string, string[]>();
  let calls: Array<{ host: string; request: UsageHelperRequest }> = [];
  const transport = async (host: UsageHost, request: UsageHelperRequest): Promise<UsageHelperResponse> => {
    calls.push({ host: host.host, request });
    if (request.action === 'inventory') {
      const value = inventories.get(host.host) ?? [];
      if (value instanceof Error) throw value;
      return { version: 1, action: 'inventory', candidates: structuredClone(value),
        ...(signInNeeded.has(host.host) ? { signInNeededSelectors: signInNeeded.get(host.host) } : {}),
        ...(incomplete.has(host.host) ? { incompleteSelectors: incomplete.get(host.host) } : {}) };
    }
    const value = collections.get(host.host) ?? { sample: null, failureCode: 'unavailable' };
    if (value instanceof Error) throw value;
    return { version: 1, action: 'collect', candidate: structuredClone(request.candidate), ...structuredClone(value) };
  };
  const deps = { transport, hosts: async () => hosts, now: () => now };
  let service = new UsageFailoverService(deps);
  const outSteps: Array<Record<string, unknown>> = [];
  for (const step of steps) {
    if (step.restart) service = new UsageFailoverService(deps);
    now = step.now;
    if (step.hosts) hosts = step.hosts;
    inventories.clear(); collections.clear(); signInNeeded.clear(); incomplete.clear();
    for (const [h, v] of Object.entries(step.inventories)) inventories.set(h, v === 'error' ? new Error('offline') : v);
    for (const [h, v] of Object.entries(step.collections)) collections.set(h, v === 'error' ? new Error('offline') : v);
    for (const [h, v] of Object.entries(step.signInNeeded ?? {})) signInNeeded.set(h, v);
    for (const [h, v] of Object.entries(step.incomplete ?? {})) incomplete.set(h, v);
    if (step.musePeers) {
      const names = await fs.readdir(path.join(ccsDir, 'account-usage-failover'));
      const key = names.find((n: string) => /^[a-f0-9]{64}\.json$/.test(n))!.slice(0, -5);
      const grant = { version: 1, key, identity: candidate().identity, peers: step.musePeers };
      await fs.writeFile(musePeerBindingFile(ccsDir, key), JSON.stringify(grant), { mode: 0o600 });
    }
    calls = [];
    const tmpl = step.template ?? baseTemplate;
    let result: DashboardAccount | number;
    if (step.op === 'collect') result = await service.collect(target, tmpl);
    else if (step.op === 'recover') result = await service.recover(target, tmpl);
    else if (step.op === 'peek') result = service.peek(target, tmpl);
    else if (step.op === 'holdCooldown') result = service.holdCooldown(target, step.holdUntil ?? now);
    else if (step.op === 'cooldownUntil') result = service.cooldownUntil(target);
    // Capture cooldown + observation files.
    const dir = path.join(ccsDir, 'account-usage-failover');
    const cooldowns: Record<string, unknown> = {};
    let cache: unknown = null;
    let cacheKey = '';
    try {
      for (const n of await fs.readdir(dir)) {
        if (n.startsWith('cooldown-') && n.endsWith('.json')) cooldowns[n] = JSON.parse(await fs.readFile(path.join(dir, n), 'utf8'));
        else if (/^[a-f0-9]{64}\.json$/.test(n)) { cacheKey = n.slice(0, -5); cache = JSON.parse(await fs.readFile(path.join(dir, n), 'utf8')); }
      }
    } catch { /* dir may not exist yet */ }
    outSteps.push({
      op: step.op, now: step.now, restart: step.restart ?? false,
      inventories: step.inventories, collections: step.collections,
      hosts: step.hosts ?? null, signInNeeded: step.signInNeeded ?? null, incomplete: step.incomplete ?? null,
      musePeers: step.musePeers ?? null, holdUntil: step.holdUntil ?? null,
      template: step.template ?? null,
      expected: {
        result, collectedHosts: calls.filter((c) => c.request.action === 'collect').map((c) => c.host),
        cooldowns, cache, cacheKey,
      },
    });
  }
  await fs.rm(ccsDir, { recursive: true, force: true });
  return { name, target: { ...target, ccsDir: 'CCS_DIR' }, template: baseTemplate, steps: outSteps };
}

const scenarios: Array<Promise<unknown>> = [];
const okCollection = (at: number): UsageCollection => ({ sample: reading(at), failureCode: null });

// 1. Primary read on Mac, then a second collect within the fresh window does not re-read.
scenarios.push(runScenario('primary-fresh', museTarget(''), template(), [
  { op: 'collect', now: BASE_TIME, inventories: { mac: [candidate()], windows: [candidate({ locator: '3'.repeat(64), revision: '4'.repeat(64) })], ubuntu: [] }, collections: { mac: okCollection(BASE_TIME), windows: okCollection(BASE_TIME) } },
  { op: 'collect', now: BASE_TIME + 1000, inventories: { mac: [candidate()], windows: [], ubuntu: [] }, collections: { mac: okCollection(BASE_TIME + 1000) } },
  { op: 'peek', now: BASE_TIME + 2000, inventories: {}, collections: {} },
]));

// 2. Mac offline -> fallback to a linked native Muse peer on Windows, restart keeps it.
scenarios.push(runScenario('muse-peer-fallback', museTarget(''), template(), [
  { op: 'collect', now: BASE_TIME, inventories: { mac: [candidate()], windows: [], ubuntu: [] }, collections: { mac: okCollection(BASE_TIME) } },
  { op: 'collect', now: BASE_TIME + 31000, musePeers: [{ host: 'windows', identity: { kind: 'muse-native-credential', key: 'c'.repeat(64) } }],
    inventories: { mac: 'error', ubuntu: [], windows: [candidate({ identity: { kind: 'muse-native-credential', key: 'c'.repeat(64) } })] },
    collections: { windows: { sample: reading(BASE_TIME + 31000, { windows: [{ key: 'weekly', label: 'Weekly limit', usedPercent: 33, remainingPercent: null, resetAt: null, windowMinutes: null, used: null, limit: null, unit: null }] }), failureCode: null, refreshAfterSeconds: 300 } } },
  { op: 'peek', now: BASE_TIME + 32000, restart: true, inventories: {}, collections: {} },
]));

// 3. Peer 429 holds the linked credential for its own account rows.
scenarios.push(runScenario('peer-429-hold', museTarget(''), template(), [
  { op: 'collect', now: BASE_TIME, inventories: { mac: [candidate()], windows: [], ubuntu: [] }, collections: { mac: okCollection(BASE_TIME) } },
  { op: 'collect', now: BASE_TIME + 31000, musePeers: [{ host: 'windows', identity: { kind: 'muse-native-credential', key: 'c'.repeat(64) } }],
    inventories: { mac: 'error', windows: [candidate({ locator: '5'.repeat(64), identity: { kind: 'muse-native-credential', key: 'c'.repeat(64) } })] },
    collections: { windows: { sample: null, failureCode: 'rate_limited', retryAfterSeconds: 900 } } },
  { op: 'collect', now: BASE_TIME + 32000, inventories: { mac: 'error', windows: [candidate({ locator: '5'.repeat(64), identity: { kind: 'muse-native-credential', key: 'c'.repeat(64) } })] }, collections: { windows: okCollection(BASE_TIME + 32000) } },
]));

// 4. Claude Desktop 429 cadence: stepped delay + offset, one step down per verified success.
const claudeTarget = (ccsDir: string): UsageFailoverTarget => ({ ccsDir, accountId: 'claude:default', provider: 'claude', binding: 'claude-binding', primarySources: [{ ...MAC, selector: 'default', claudeProfile: { id: 'default', email: 'same@example.test' } }], claudeDesktopCadence: true });
const claudeTemplate = (): DashboardAccount => ({ ...template(), id: 'claude:default', provider: 'claude', providerLabel: 'Claude', platform: 'mac' });
scenarios.push(runScenario('claude-429-cadence', claudeTarget(''), claudeTemplate(), [
  { op: 'collect', now: BASE_TIME, inventories: { mac: [candidate({ selectors: ['default'], identity: { kind: 'claude-oauth', key: 'd'.repeat(64) } })], windows: [], ubuntu: [] }, collections: { mac: { sample: null, failureCode: 'rate_limited', retryAfterSeconds: 120 } } },
  { op: 'cooldownUntil', now: BASE_TIME + 1000, inventories: {}, collections: {} },
  { op: 'collect', now: BASE_TIME + 421000, inventories: { mac: [candidate({ selectors: ['default'], identity: { kind: 'claude-oauth', key: 'd'.repeat(64) } })], windows: [], ubuntu: [] }, collections: { mac: { sample: null, failureCode: 'rate_limited', retryAfterSeconds: 60 } } },
  { op: 'collect', now: BASE_TIME + 961000, restart: true, inventories: { mac: [candidate({ selectors: ['default'], identity: { kind: 'claude-oauth', key: 'd'.repeat(64) } })], windows: [], ubuntu: [] }, collections: { mac: okCollection(BASE_TIME + 961000) } },
  { op: 'cooldownUntil', now: BASE_TIME + 961001, inventories: {}, collections: {} },
  { op: 'collect', now: BASE_TIME + 1501000, inventories: { mac: [candidate({ selectors: ['default'], identity: { kind: 'claude-oauth', key: 'd'.repeat(64) } })], windows: [], ubuntu: [] }, collections: { mac: { sample: null, failureCode: 'rate_limited', retryAfterSeconds: 60 } } },
  { op: 'cooldownUntil', now: BASE_TIME + 1501001, inventories: {}, collections: {} },
]));

// 5. identity_mismatch on the primary clears the saved reading and identity.
scenarios.push(runScenario('identity-mismatch', museTarget(''), template(), [
  { op: 'collect', now: BASE_TIME, inventories: { mac: [candidate()], windows: [], ubuntu: [] }, collections: { mac: okCollection(BASE_TIME) } },
  { op: 'collect', now: BASE_TIME + 31000, inventories: { mac: [candidate()], windows: [], ubuntu: [] }, collections: { mac: { sample: null, failureCode: 'identity_mismatch' } } },
  { op: 'peek', now: BASE_TIME + 32000, inventories: {}, collections: {} },
]));

// 6. refreshAfterSeconds schedules the next attempt; a collect before it does not re-read.
scenarios.push(runScenario('refresh-after', museTarget(''), template(), [
  { op: 'collect', now: BASE_TIME, inventories: { mac: [candidate()], windows: [], ubuntu: [] }, collections: { mac: { sample: reading(BASE_TIME), failureCode: null, refreshAfterSeconds: 300 } } },
  { op: 'collect', now: BASE_TIME + 10000, inventories: { mac: [candidate()], windows: [], ubuntu: [] }, collections: { mac: okCollection(BASE_TIME + 10000) } },
  { op: 'collect', now: BASE_TIME + 310000, inventories: { mac: [candidate()], windows: [], ubuntu: [] }, collections: { mac: okCollection(BASE_TIME + 310000) } },
]));

// 7. holdCooldown / cooldownUntil external hold.
scenarios.push(runScenario('external-hold', museTarget(''), template(), [
  { op: 'collect', now: BASE_TIME, inventories: { mac: [candidate()], windows: [], ubuntu: [] }, collections: { mac: okCollection(BASE_TIME) } },
  { op: 'holdCooldown', now: BASE_TIME + 1000, holdUntil: BASE_TIME + 600000, inventories: {}, collections: {} },
  { op: 'cooldownUntil', now: BASE_TIME + 2000, inventories: {}, collections: {} },
  { op: 'collect', now: BASE_TIME + 3000, inventories: { mac: [candidate()], windows: [], ubuntu: [] }, collections: { mac: okCollection(BASE_TIME + 3000) } },
]));

// 8. needs_sign_in on a Claude desktop primary adds the sign-in selector.
scenarios.push(runScenario('claude-signin', claudeTarget(''), claudeTemplate(), [
  { op: 'collect', now: BASE_TIME, inventories: { mac: [candidate({ selectors: ['default'], identity: { kind: 'claude-oauth', key: 'd'.repeat(64) } })], windows: [], ubuntu: [] }, collections: { mac: { sample: null, failureCode: 'needs_sign_in' } }, signInNeeded: { mac: ['default'] } },
  { op: 'peek', now: BASE_TIME + 1000, inventories: {}, collections: {} },
]));

// 9. Cursor session discovered on another computer when the primary has no candidate.
const cursorTarget = (ccsDir: string): UsageFailoverTarget => ({ ccsDir, accountId: 'cursor:usage', provider: 'cursor', binding: 'cursor-binding', primarySources: [{ ...MAC, selector: 'discover' }] });
const cursorTemplate = (): DashboardAccount => ({ ...template(), id: 'cursor:usage', provider: 'cursor', providerLabel: 'Cursor' });
scenarios.push(runScenario('cursor-session', cursorTarget(''), cursorTemplate(), [
  { op: 'collect', now: BASE_TIME, inventories: { mac: [], windows: [candidate({ identity: { kind: 'cursor-session', key: 'e'.repeat(64) } })], ubuntu: [] }, collections: { windows: okCollection(BASE_TIME) } },
  { op: 'peek', now: BASE_TIME + 1000, restart: true, inventories: {}, collections: {} },
]));

// 10. Unreadable configured store keeps the saved reading (not a sign-out).
scenarios.push(runScenario('unreadable-store', museTarget(''), template(), [
  { op: 'collect', now: BASE_TIME, inventories: { mac: [candidate()], windows: [], ubuntu: [] }, collections: { mac: okCollection(BASE_TIME) } },
  { op: 'collect', now: BASE_TIME + 31000, inventories: { mac: 'error', windows: [], ubuntu: [] }, collections: {} },
  { op: 'peek', now: BASE_TIME + 32000, inventories: {}, collections: {} },
]));

// 11. A 429 streak still falls back to the account's healthy computer when the first
// candidate fails without a 429; the clean read there steps the streak down.
const claudeWindowsTarget = (ccsDir: string): UsageFailoverTarget => ({ ccsDir, accountId: 'claude:windows-fallback', provider: 'claude', binding: 'claude-windows-binding', primarySources: [{ ...WINDOWS, selector: 'default', claudeProfile: { id: 'default', email: 'same@example.test' } }], claudeDesktopCadence: true });
const cadenceToken = (locator: string) => candidate({ locator, selectors: ['default'], identity: { kind: 'claude-oauth', key: 'd'.repeat(64) } });
const fallbackInventories = () => ({ windows: [cadenceToken('1'.repeat(64))], mac: [cadenceToken('2'.repeat(64))], ubuntu: [] });
scenarios.push(runScenario('claude-429-streak-fallback', claudeWindowsTarget(''), claudeTemplate(), [
  { op: 'collect', now: BASE_TIME, inventories: fallbackInventories(), collections: { windows: { sample: null, failureCode: 'rate_limited', retryAfterSeconds: 120 } } },
  { op: 'collect', now: BASE_TIME + 421000, inventories: fallbackInventories(), collections: { windows: { sample: null, failureCode: 'unavailable' }, mac: okCollection(BASE_TIME + 421000) } },
  { op: 'collect', now: BASE_TIME + 841000, inventories: fallbackInventories(), collections: { windows: { sample: null, failureCode: 'unavailable' }, mac: okCollection(BASE_TIME + 841000) } },
]));

const corpus = await Promise.all(scenarios);
const outDir = path.join(process.cwd(), 'crates', 'aac-http', 'tests', 'fixtures');
await fs.mkdir(outDir, { recursive: true });
await fs.writeFile(path.join(outDir, 'g4-corpus.json'), JSON.stringify(corpus));
console.log(`wrote g4-corpus.json (${corpus.length} scenarios) to ${outDir}`);
