/**
 * Generated composition tables for tests/spec/profile-read-parity.ts: each entry is one input
 * the TS composer answers in process and the Rust service answers over its private socket.
 * A Rust 200 must equal the TS bytes; an input TS throws on must be refused; `mayRefuse`
 * marks inputs whose JavaScript meaning Rust deliberately leaves to TS (a legacy date parse,
 * a coerced array). Plan names come from the plan.ts source, so a new plan is covered.
 */
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export interface TableEntry {
  table: string;
  label: string;
  route: string;
  /** The TS answer; throwing means TS answers an error (Rust must refuse). */
  ts: () => unknown | Promise<unknown>;
  /** The Rust input (null: the bridge never sends it); undefined for a bodyless file read. */
  input?: () => unknown;
  /** Write the owned file before a bodyless read. */
  setup?: () => void;
  mayRefuse?: boolean;
}

/** The composer modules, imported by the worker after its fixture mocks. */
export interface Composers {
  publicInventory: (value: never) => unknown;
  publicQuotas: (accounts: never, at: number) => unknown;
  composeClaudeDesktopUsage: (facts: never) => unknown;
  composeCodexAuthProfilesSummary: (facts: never) => unknown;
  composeCodexProfileQuotas: (facts: never) => unknown;
  listClaudeDesktopProfileMetadata: () => Promise<unknown>;
  input: typeof import('../../src/web-server/rust-profile-input');
}

const repo = fileURLToPath(new URL('../..', import.meta.url));
const NOW = Date.parse('2026-11-15T12:00:00Z');

/** A small deterministic generator, so the tables are the same in every mode. */
function random(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function pick<T>(next: () => number, values: readonly T[]): T {
  return values[Math.floor(next() * values.length)];
}

/** Every plan label and alias in plan.ts. */
export function planNames(): string[] {
  const source = fs.readFileSync(path.join(repo, 'src/antigravity/plan.ts'), 'utf8');
  const table = source.slice(source.indexOf('const PLANS = {'), source.indexOf('} satisfies'));
  const names = [...table.matchAll(/label: '([^']+)'/g)].map((m) => m[1]);
  for (const list of table.matchAll(/aliases: \[([^\]]*)\]/g))
    names.push(...[...list[1].matchAll(/'([^']+)'/g)].map((m) => m[1]));
  if (names.length < 30) throw new Error('plan.ts table not found');
  return names;
}

function respellings(name: string): string[] {
  return [
    name,
    name.toUpperCase(),
    name.toLowerCase(),
    ` 　${name.replace(/ /g, '_')}﻿`,
    name.replace(/ /g, ' - ').replace(/^/, '('),
    name.replace(/k/g, 'K'),
    `${name}é`,
    name.replace(/ /g, '  '),
  ];
}

const TIMESTAMPS = [
  '2026-11-15T08:00:00Z',
  '2026-11-15T08:00:00.1Z',
  '2026-11-15T08:00:00.123456789Z',
  '2026-11-15T13:30:00+05:30',
  '2026-11-15T00:00:00-08:00',
  '2026-11-15T08:00:00-00:00',
  '2026-11-15T08:00:00+24:00',
  '2026-11-15T24:00:00Z',
  '2026-11-15T23:59:60Z',
  '2026-02-30T00:00:00Z',
  '2026-13-01T00:00:00Z',
  '2026-11-15T08:00:00z',
  '2026-11-15T08:00Z',
  '2026-11-15 08:00:00Z',
  '1999-12-31T23:59:59.999Z',
  '2000-01-01T00:00:00Z',
  '2200-12-31T23:59:59.999Z',
  '2201-01-01T00:00:00Z',
  '2026-11-15T08:00:00.12345678901234567890Z',
  '2020-01-01T00:00:00Z',
  '2099-01-01T00:00:00Z',
  'Nov 15 2026',
  '',
] as const;

const TEXTS = [
  'Gemini Models',
  '  padded label  ',
  '　﻿',
  'Bearer token',
  'bearer',
  'BEARER\ttoken',
  'bearer\u0085x',
  'sk-live',
  'SK_test',
  'ſk-x',
  'eyJabcdefgh',
  'eyJabcdefg',
  'EYJ12345678',
  'eyJabcdéfgh',
  'dca:x',
  'HTTPS://x',
  'http:/x',
  '{braces}',
  'Access_Token',
  'refresh_tokenx',
  'client_secrets',
  'tab\tinside',
  'nul\u0000',
  'del\u007f',
  'Límite semanal 週 ✨',
  '🚀'.repeat(40),
  '🚀'.repeat(41),
  'a'.repeat(80),
  'a'.repeat(81),
  ' '.repeat(10) + 'a'.repeat(79),
] as const;

function account(id: string, extra: Record<string, unknown>): Record<string, unknown> {
  return {
    id: `antigravity:profile:${id}`,
    provider: 'antigravity',
    email: `${id}@example.com`,
    platform: 'ubuntu',
    status: 'ok',
    fetchedAt: '2026-11-15T09:00:00Z',
    sampledAt: '2026-11-15T08:59:00Z',
    windows: [],
    capabilities: { antigravityProfileId: id, antigravityHostIds: ['ubuntu'] },
    ...extra,
  };
}

function quotasEntry(
  c: Composers,
  table: string,
  label: string,
  accounts: unknown[],
  mayRefuse = false
): TableEntry {
  return {
    table,
    label,
    route: '/api/antigravity/profiles/quotas',
    ts: () => c.publicQuotas(accounts as never, NOW),
    input: () => c.input.rustAntigravityQuotasInput(accounts, NOW),
    mayRefuse,
  };
}

function windowTable(c: Composers): TableEntry[] {
  const next = random(4);
  const numbers = [undefined, null, -1, 0, -0, 0.5, 50, 100, 100.5, 1e-9, '50', true] as const;
  const entries: TableEntry[] = [];
  for (let a = 0; a < 60; a++) {
    const windows: Record<string, unknown>[] = [];
    let mayRefuse = false;
    for (let w = 0; w < 8; w++) {
      const window: Record<string, unknown> = {
        key: pick(next, [
          'gemini-models',
          'google-ai-credits',
          'model/x@y:1',
          'bad key',
          'sk-x',
          'a'.repeat(161),
          5,
        ]),
      };
      for (const name of ['usedPercent', 'remainingPercent', 'used', 'limit', 'windowMinutes'])
        if (next() < 0.7) window[name] = pick(next, [...numbers, 525600, 525601, 30.5]);
      if (next() < 0.4) window.remaining = pick(next, [null, 0, 3.5, -2, 'x']);
      for (const name of ['resetAt', 'expiresAt'])
        if (next() < 0.6) window[name] = pick(next, [...TIMESTAMPS, null, 5]);
      if (next() < 0.5) window.label = pick(next, [...TEXTS, null, 7]);
      if (next() < 0.3) window.unit = pick(next, ['credits', 'a'.repeat(25), ' x ', 'https://u']);
      if (next() < 0.5)
        window.kind = pick(next, [
          'rate_limit',
          'balance',
          'spend',
          'extra_usage',
          'other',
          null,
          5,
          {},
        ]);
      for (const name of ['enabled', 'unlimited'])
        if (next() < 0.3) window[name] = pick(next, [true, false, 'true']);
      if (next() < 0.4) {
        window.poolId = pick(next, ['pool-1', 'p/x@y:z', 'bad id', 'sk-x', 'a'.repeat(129), 3]);
        if (next() < 0.6)
          window.poolIdSource = pick(next, ['provider-id', 'provider-bucket-membership', 'other']);
        if (next() < 0.6) window.poolLabel = pick(next, [...TEXTS]);
      }
      if (next() < 0.3)
        window.modelIds = pick(next, [
          [],
          ['m-1', 'm-1', 'm-2'],
          'x',
          [1, 'ok', 'bad id'],
          Array.from({ length: 70 }, (_, i) => `m${i}`),
        ]);
      if (next() < 0.03) {
        window.kind = ['spend'];
        mayRefuse = true;
      }
      if (Array.isArray(window.kind)) mayRefuse = true;
      windows.push(window);
    }
    const sampledAt = pick(next, [...TIMESTAMPS, null]);
    entries.push(
      quotasEntry(
        c,
        'windows',
        `accounts ${a}`,
        [account(`w${a}`, { windows, sampledAt, fetchedAt: pick(next, [...TIMESTAMPS, null]) })],
        mayRefuse
      )
    );
  }
  return entries;
}

/** `slice(0, 256)`: windows past the 256th are never published. */
function manyWindowsTable(c: Composers): TableEntry[] {
  const windows = Array.from({ length: 260 }, (_, i) => ({
    key: `w${i}`,
    usedPercent: i % 101,
    resetAt: i % 2 ? '2020-01-01T00:00:00Z' : null,
  }));
  return [quotasEntry(c, 'windows', '260 windows', [account('many', { windows })])];
}

function planTable(c: Composers): TableEntry[] {
  const entries: TableEntry[] = [];
  const times: Array<[string | null, string | null]> = [
    ['2026-11-15T08:59:00Z', null],
    ['2026-11-01T23:59:59.999Z', null],
    [null, '2026-10-01T00:00:00Z'],
    [null, null],
  ];
  for (const name of planNames())
    for (const [i, plan] of respellings(name).entries()) {
      const [sampledAt, fetchedAt] = times[i % times.length];
      entries.push(
        quotasEntry(c, 'plans', `${name} #${i}`, [account('p', { plan, sampledAt, fetchedAt })])
      );
    }
  for (const plan of ['Mystery', 'pro trial', 'ultra 5x', 'standard-tier', null, 5, 'sk-pro'])
    entries.push(quotasEntry(c, 'plans', `other ${String(plan)}`, [account('p', { plan })]));
  return entries;
}

function textTable(c: Composers): TableEntry[] {
  const entries: TableEntry[] = [];
  for (const text of TEXTS) {
    entries.push(
      quotasEntry(c, 'texts', JSON.stringify(text).slice(0, 40), [
        account('t', {
          windows: [
            { key: 'k', usedPercent: 1, label: text, unit: text, poolId: 'pool', poolLabel: text },
          ],
        }),
      ])
    );
    for (const email of [`${text}@example.com`, `a@${text}.com`, ` ${text}x@y.z `]) {
      const id = 'e';
      entries.push(
        quotasEntry(c, 'texts', `email ${JSON.stringify(email).slice(0, 30)}`, [
          { ...account(id, {}), email },
        ])
      );
    }
  }
  return entries;
}

function inventoryTable(c: Composers): TableEntry[] {
  const next = random(9);
  const entries: TableEntry[] = [];
  for (let i = 0; i < 80; i++) {
    // Mostly valid rows, so most inventories publish; one bad row makes TS answer 500.
    const bad = next() < 0.2;
    const profiles = Array.from({ length: 1 + Math.floor(next() * 4) }, (_, p) => ({
      id: bad && p === 0 ? pick(next, ['bad id', 'a'.repeat(65), 7, 'p1']) : `p${p}`,
      email:
        bad && p === 0
          ? pick(next, ['x@y', null, 'https://a@b.c'])
          : pick(next, [`p${p}@example.com`, ` ünï${p}@exämple.de `, `P${p}@Example.ORG`]),
      plan: pick(next, [...TEXTS, null, undefined]),
      hostId: bad && p === 0 ? pick(next, ['ubuntu', 'mac']) : 'ubuntu',
      available: pick(next, [true, false, 1, 'yes', undefined]),
      selected: pick(next, [true, false, null]),
      runtimeVerified: pick(next, [true, 0]),
      verifiedAt: pick(next, [...TIMESTAMPS, null, 5]),
    }));
    const inventory: Record<string, unknown> = {
      schemaVersion: 1,
      hostId: next() < 0.97 ? 'ubuntu' : 'mac',
      profiles,
    };
    if (next() < 0.4)
      inventory.nativeUpdatePaused = pick(next, [
        { installedVersion: '1.2.3' },
        { installedVersion: '1.2.3-rc.1' },
        { installedVersion: '1.2' },
        { installedVersion: 'v1.2.3' },
        { installedVersion: '1.2.3-' },
        {},
        true,
        'x',
        0,
        [],
      ]);
    if (next() < 0.4)
      inventory.runtimeServiceProblem = pick(next, [
        { reason: 'service-failed', module: null, python: null, builtFor: null, exitStatus: 3 },
        {
          reason: 'missing-python-module',
          module: '_x.y',
          python: '3.12',
          builtFor: '3.1234',
          exitStatus: 256,
        },
        {
          reason: 'parser-import-failed',
          module: '9bad',
          python: '12.123',
          builtFor: '1.',
          exitStatus: -0,
        },
        { reason: 'nope' },
        'service-failed',
        null,
      ]);
    entries.push({
      table: 'inventory',
      label: `inventory ${i}`,
      route: '/api/antigravity/profiles',
      ts: () => c.publicInventory(inventory as never),
      input: () => c.input.rustAntigravityInventoryInput(inventory),
    });
  }
  return entries;
}

function codexQuotaTable(c: Composers): TableEntry[] {
  const next = random(12);
  const entries: TableEntry[] = [];
  const names = ['work', 'home', '123', 'x'];
  for (let i = 0; i < 120; i++) {
    let mayRefuse = false;
    const profiles = names
      .slice(0, 1 + Math.floor(next() * names.length))
      .map((name) => ({ name, authValid: pick(next, [true, true, false, 1, 0, 'yes', null]) }));
    const rows = Array.from({ length: Math.floor(next() * 5) }, () => {
      const row: Record<string, unknown> = {
        profile: pick(next, [...names, 'other', 5]),
        quotaStatus: pick(next, ['ok', 'ok', 'error', 'stale', undefined]),
        needsReauth: pick(next, [true, false, 1, 0, '', 'x', null, undefined]),
        fetchedAt: pick(next, [...TIMESTAMPS, null, undefined]),
      };
      if (next() < 0.4) row.staleAsOf = pick(next, [...TIMESTAMPS, null]);
      if (next() < 0.9)
        row.quotaWindows =
          pick(next, [null, []]) ??
          Array.from({ length: Math.floor(next() * 4) }, () => ({
            key: pick(next, ['five_hour', 'seven_day', 'other', undefined, 3]),
            label: pick(next, ['L', 'Límite ✨', undefined, null]),
            usedPercent: pick(next, [0, 12.5, 100, -0, undefined, null]),
            resetAt: pick(next, [...TIMESTAMPS, null, undefined]),
          }));
      if (next() < 0.5 && !Array.isArray(row.quotaWindows) && row.quotaWindows !== null)
        row.quotaWindows = Array.from({ length: 2 }, () => ({
          key: 'five_hour',
          usedPercent: 5,
          resetAt: pick(next, [...TIMESTAMPS]),
        }));
      const windows = Array.isArray(row.quotaWindows)
        ? (row.quotaWindows as Array<Record<string, unknown>>)
        : [];
      // A date only TS's legacy parser reads (not ISO-shaped, not empty) is TS's to decide.
      const legacy = (value: unknown) =>
        typeof value === 'string' && value !== '' && !/^\d{4}-\d{2}-\d{2}T/.test(value);
      if (windows.some((w) => legacy(w.resetAt)) || legacy(row.staleAsOf) || legacy(row.fetchedAt))
        mayRefuse = true;
      return row;
    });
    const renewals = new Map(
      names
        .filter(() => next() < 0.3)
        .map((name) => [
          name,
          {
            name,
            state: pick(next, ['failed', 'ok', 'renewing']),
            ...(next() < 0.7 ? { message: 'Sign in again.' } : {}),
          },
        ])
    );
    const facts = { profiles, rows, renewals, now: NOW };
    entries.push({
      table: 'codex-quotas',
      label: `facts ${i}`,
      route: '/api/codex/profiles/quotas',
      ts: () => c.composeCodexProfileQuotas(facts as never),
      input: () => c.input.rustCodexQuotasInput(facts as never),
      mayRefuse,
    });
  }
  return entries;
}

function codexSummaryTable(c: Composers): TableEntry[] {
  const entries: TableEntry[] = [];
  const dir = '/home/fixture/.ccs/codex-instances';
  const profile = (name: string) => ({
    name,
    codexHome: `${dir}/${name}`,
    email: name === 'home' ? null : `${name}@example.com`,
    plan: 'pro',
    accountId: null,
    lastUsed: null,
    authValid: name !== 'home',
  });
  for (const codexHome of [
    null,
    '',
    '  ',
    `${dir}/home`,
    ` ${dir}/work\n`,
    '/elsewhere',
    `${dir}/gone`,
  ])
    for (const envProfile of [null, '', 'work', ' home ', 'gone'])
      for (const defaultName of [null, 'work'])
        for (const names of [[], ['work', 'home'], ['123', 'work']]) {
          if (defaultName && !names.includes(defaultName)) continue;
          const facts = {
            instancesDir: dir,
            env: { codexHome, profile: envProfile },
            registry: { default: defaultName, names },
            profiles: names.map(profile),
            activated: names.includes('work')
              ? {
                  name: 'work',
                  email: 'work@example.com',
                  plan: 'pro',
                  codexHome: '/home/fixture/.codex',
                }
              : null,
          };
          entries.push({
            table: 'codex-summary',
            label: JSON.stringify(facts.env) + JSON.stringify(facts.registry),
            route: '/api/codex/profiles',
            ts: () => c.composeCodexAuthProfilesSummary(facts as never),
            input: () => c.input.rustCodexProfilesInput(facts as never),
            // A stale CCS_CODEX_PROFILE is logged by TS.
            mayRefuse:
              !(codexHome ?? '').trim() &&
              !!(envProfile ?? '').trim() &&
              !names.includes((envProfile ?? '').trim()),
          });
        }
  return entries;
}

function usageTable(c: Composers): TableEntry[] {
  const next = random(21);
  const histories: Array<string | null> = [
    null,
    '',
    '[]',
    '{}',
    '{"version":2}',
    '{"version":2,"samples":{}}',
    '{"version":2.0,"samples":[]}',
    '{"version":"2","samples":[{"t":1,"u":{"fh":1}}]}',
    '{"version":2,"samples":[null,1,"x",{"t":"1","u":{"fh":1}}]}',
    '{"version":2,"samples":[{"t":1e3,"u":{"fh":1e2,"sd":0.1,"so":-0,"sn":1e-7,"xu":"5"}}]}',
    '{"version":2,"samples":[{"t":253402300799999,"u":{"fh":1}}]}',
    '{"version":2,"samples":[{"t":0,"u":{"fh":1}},{"t":0,"u":{"sd":2}}]}',
    '{"version":2,"samples":[{"t":10,"u":[]}]}',
    '{"version":2,"samples":[{"t":10,"u":{"fh":-0.0}}]}',
    '{"version":2,"samples":[{"t":10,"u":{"zz":1}}]}',
    '{"version":2,"version":2,"samples":[{"t":10,"u":{"fh":1,"fh":2}}]}',
    '  {"version":2,"samples":[{"t":10,"u":{"fh":1}}]}\n',
    '{"version":2,"samples":[{"t":1.7e12,"u":{"fh":3.14159265358979}}]}',
  ];
  const entries: TableEntry[] = [];
  for (let i = 0; i < 60; i++) {
    const count = 1 + Math.floor(next() * 4);
    const profiles = Array.from({ length: count }, (_, p) => {
      const state = pick(next, ['read', 'read', 'read', 'unconfigured', 'failed'] as const);
      return {
        profile: { ...(next() < 0.7 ? { id: `p${p}` } : {}), email: `p${p}@exämple.com` },
        read: state === 'read' ? { state, history: pick(next, histories) } : ({ state } as const),
      };
    });
    const facts = {
      platform: pick(next, ['mac', 'windows']),
      fetchedAt: '2026-11-15T12:00:00.000Z',
      profiles,
    };
    entries.push({
      table: 'usage',
      label: `usage ${i}`,
      route: '/api/claude/desktop-profiles/usage',
      ts: () => c.composeClaudeDesktopUsage(facts as never),
      input: () => c.input.rustDesktopUsageInput(facts as never),
      // '' and other text JSON.parse refuses is unprovable for serde: TS decides.
      mayRefuse: profiles.some((p) => p.read.state === 'read' && p.read.history === ''),
    });
  }
  return entries;
}

function manifestTable(c: Composers, ccs: string): TableEntry[] {
  const file = path.join(ccs, 'claude-desktop-profiles.json');
  const launcher = (extra: Record<string, unknown>) => ({ launcherName: 'L', ...extra });
  const rows: unknown[] = [
    { email: 'a@b.c', mac: launcher({}) },
    { id: 'x', email: 'a@b.c', mac: launcher({ sshHost: 'h', launcherPath: '/A.app' }) },
    { id: 'x', email: 'a@b.c', mac: launcher({ sshHost: 'h', launcherPath: 'A.app' }) },
    { id: 'x', email: 'a@b.c', mac: launcher({ launcherPath: '/A.app' }) },
    { id: 'x', email: 'a@b.c', windows: launcher({ sshHost: 'h', startMenuPath: 'S' }) },
    { id: 'x', email: 'a@b.c', mac: launcher({ startMenuPath: 'ignored on mac' }) },
    { id: 'x', email: 'a@b.c', mac: launcher({ isDefault: false }) },
    { id: 'x', email: 'a@b.c', mac: launcher({ isDefault: 'yes' }) },
    { id: 'x', email: 'a@b.c', mac: launcher({ sshHost: 'h_x' }) },
    { id: 'x', email: 'a@b.c', mac: launcher({ sshHost: 'a'.repeat(129) }) },
    { id: 'x', email: 'a@b.c', mac: launcher({ launcherName: 'n'.repeat(255) }) },
    { id: 'x', email: 'a@b.c', mac: launcher({ launcherName: '🚀'.repeat(128) }) },
    { id: 'x', email: 'a@b.c', mac: launcher({ launcherName: '🚀'.repeat(127) + 'a' }) },
    { id: 'x', email: 'a@b.c', mac: launcher({ profilePath: null }) },
    { id: 'a'.repeat(64), email: 'a@b.c', mac: launcher({}) },
    { id: 'a'.repeat(65), email: 'a@b.c', mac: launcher({}) },
    { id: '-x', email: 'a@b.c', mac: launcher({}) },
    { id: 'x', email: 'a b@c.d', mac: launcher({}) },
    { id: 'x', email: 'a@b.c', mac: [] },
    { id: 'x', email: 'a@b.c' },
    { id: 'x', email: 'a@b.c', mac: launcher({}), windows: launcher({ launchUri: 'ignored' }) },
    'not an object',
  ];
  return rows.map((row, i) => ({
    table: 'manifest',
    label: `row ${i}`,
    route: '/api/claude/desktop-profiles',
    setup: () => {
      fs.writeFileSync(file, JSON.stringify({ version: 1, profiles: [row] }), { mode: 0o600 });
    },
    ts: () => c.listClaudeDesktopProfileMetadata().then((profiles) => ({ profiles })),
  }));
}

export function compositionTables(c: Composers, ccs: string): TableEntry[] {
  return [
    ...windowTable(c),
    ...manyWindowsTable(c),
    ...planTable(c),
    ...textTable(c),
    ...inventoryTable(c),
    ...codexQuotaTable(c),
    ...codexSummaryTable(c),
    ...usageTable(c),
    ...manifestTable(c, ccs),
  ];
}
