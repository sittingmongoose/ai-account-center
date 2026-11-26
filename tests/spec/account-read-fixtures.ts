/**
 * Fixtures for account-read-parity.ts: file forms for visibility and preferences, an injected
 * lifecycle environment for the registry, the Analytics service over real remote-cache files
 * whose sources read as read, partial, cached with a full-read age, no complete read, not
 * installed and permission denied, and generated composition tables. Fixed clocks only.
 */
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  appendAccountAnalyticsSnapshot,
  type AccountAnalyticsHistoryData,
} from '../../src/web-server/services/account-analytics-history';
import { buildDashboardProviders } from '../../src/web-server/services/dashboard-provider-registry';
import { defaultAnalyticsProviderTable } from '../../src/web-server/services/account-analytics-providers';
import { resolveAccountAnalyticsRange } from '../../src/web-server/services/account-analytics-range';
import { analyticsAvailableRange } from '../../src/web-server/services/account-analytics-compose';
import type {
  AccountAnalyticsComposeInput,
  AccountAnalyticsRangeInput,
} from '../../src/web-server/services/account-analytics-compose';
import type { RegistryListingFacts } from '../../src/web-server/services/account-lifecycle-listing';
import type {
  DashboardAccount,
  DashboardAccountWindow,
} from '../../src/web-server/services/account-dashboard-types';
import type { AccountAnalyticsQuery } from '../../src/web-server/services/account-analytics-types';

const repo = fileURLToPath(new URL('../..', import.meta.url));
export const NOW = Date.parse('2026-11-14T21:10:00.000Z');
const HOUR = 3_600_000;
const DAY = 24 * HOUR;

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/** Remove everything but the Rust boot folder; restore the private folder mode. */
export function resetCcs(ccs: string): void {
  fs.chmodSync(ccs, 0o700);
  for (const entry of fs.readdirSync(ccs))
    if (entry !== 'run') fs.rmSync(path.join(ccs, entry), { recursive: true, force: true });
}

function write(ccs: string, name: string, contents: string | Buffer, mode = 0o600): void {
  const file = path.join(ccs, name);
  fs.writeFileSync(file, contents, { mode });
  fs.chmodSync(file, mode);
}

interface FileForm {
  name: string;
  rust: boolean;
  setup: (ccs: string) => void;
}

const ID = 'codex:work';
const ids = (count: number) =>
  Array.from({ length: count }, (_, i) => `zai:acct:${i.toString(16).padStart(8, '0')}`);

export function visibilityForms(): FileForm[] {
  const file =
    (value: unknown, mode = 0o600) =>
    (ccs: string) =>
      write(
        ccs,
        'account-visibility.json',
        typeof value === 'string' ? value : JSON.stringify(value, null, 2) + '\n',
        mode
      );
  const lists = {
    version: 1,
    hiddenProviders: ['muse', 'retired-provider', 'muse', 'zai'],
    hiddenAccountIds: [
      ID,
      'claude:me',
      ID,
      'plan-opencode-go-console-mac-0123456789ab',
      'antigravity:profile:p1',
    ],
    trayHiddenProviders: ['cursor'],
    trayHiddenAccountIds: ['cursor:acct:0a1b2c3d'],
  };
  return [
    { name: 'absent', rust: true, setup: () => {} },
    {
      name: 'empty lists',
      rust: true,
      setup: file({
        version: 1,
        hiddenProviders: [],
        hiddenAccountIds: [],
        trayHiddenProviders: [],
        trayHiddenAccountIds: [],
      }),
    },
    { name: 'lists with duplicates and a retired provider', rust: true, setup: file(lists) },
    {
      name: 'file before the tray lists',
      rust: true,
      setup: file({ version: 1, hiddenProviders: ['qwen'], hiddenAccountIds: [ID] }),
    },
    {
      name: 'version 1.0 spelling',
      rust: true,
      setup: file('{"version":1.0,"hiddenProviders":[],"hiddenAccountIds":[]}'),
    },
    {
      name: '33 provider entries filtered to 32',
      rust: true,
      setup: file({
        version: 1,
        hiddenProviders: [...Array(32).fill('zai'), 'gone'],
        hiddenAccountIds: [],
      }),
    },
    {
      name: '128 account ids',
      rust: true,
      setup: file({ version: 1, hiddenProviders: [], hiddenAccountIds: ids(128) }),
    },
    {
      name: 'group-writable folder (one-time warning)',
      rust: true,
      setup: (ccs) => {
        file(lists)(ccs);
        fs.chmodSync(ccs, 0o770);
      },
    },
    {
      name: 'version as text',
      rust: false,
      setup: file({ version: '1', hiddenProviders: [], hiddenAccountIds: [] }),
    },
    {
      name: 'unknown key',
      rust: false,
      setup: file({ version: 1, hiddenProviders: [], hiddenAccountIds: [], extra: true }),
    },
    { name: 'missing account list', rust: false, setup: file({ version: 1, hiddenProviders: [] }) },
    {
      name: 'duplicate key (JSON.parse keeps the last)',
      rust: true,
      setup: file(
        '{"version":1,"hiddenProviders":["zai"],"hiddenProviders":[],"hiddenAccountIds":[]}'
      ),
    },
    {
      name: 'invalid account id',
      rust: false,
      setup: file({ version: 1, hiddenProviders: [], hiddenAccountIds: ['Codex:Work'] }),
    },
    {
      name: '129 account ids',
      rust: false,
      setup: file({ version: 1, hiddenProviders: [], hiddenAccountIds: ids(129) }),
    },
    {
      name: '33 known provider entries',
      rust: false,
      setup: file({ version: 1, hiddenProviders: Array(33).fill('zai'), hiddenAccountIds: [] }),
    },
    {
      name: 'tray ids null',
      rust: false,
      setup: file({
        version: 1,
        hiddenProviders: [],
        hiddenAccountIds: [],
        trayHiddenAccountIds: null,
      }),
    },
    { name: 'mode 0644', rust: false, setup: file(lists, 0o644) },
    {
      name: 'symlink',
      rust: false,
      setup: (ccs) => {
        write(ccs, 'target.json', JSON.stringify(lists));
        fs.symlinkSync(path.join(ccs, 'target.json'), path.join(ccs, 'account-visibility.json'));
      },
    },
    {
      name: 'folder writable by others',
      rust: false,
      setup: (ccs) => {
        file(lists)(ccs);
        fs.chmodSync(ccs, 0o707);
      },
    },
    {
      name: 'oversized',
      rust: false,
      setup: file({
        version: 1,
        hiddenProviders: [],
        hiddenAccountIds: [],
        pad: 'x'.repeat(33_000),
      }),
    },
    {
      name: 'byte order mark',
      rust: false,
      setup: file('\ufeff{"version":1,"hiddenProviders":[],"hiddenAccountIds":[]}'),
    },
    {
      name: 'invalid UTF-8',
      rust: false,
      setup: (ccs) =>
        write(
          ccs,
          'account-visibility.json',
          Buffer.concat([
            Buffer.from('{"version":1,"hiddenProviders":[],"hiddenAccountIds":["codex:a'),
            Buffer.from([0xff]),
            Buffer.from('"]}'),
          ])
        ),
    },
    {
      name: 'lone surrogate escape',
      rust: false,
      setup: file('{"version":1,"hiddenProviders":[],"hiddenAccountIds":["codex:\\ud800"]}'),
    },
    { name: 'array document', rust: false, setup: file([1]) },
  ];
}

const SOURCE = {
  id: 'omp-mac',
  tool: 'omp',
  host: 'mac',
  path: '/Users/fixture/.omp/agent/sessions',
};
const JSONL = {
  host: 'ubuntu',
  path: '/srv/logs',
  id: 'generic',
  tool: 'jsonl',
  fieldMapping: { model: 'model', timestamp: 'meta.time', inputTokens: 'usage.input_tokens' },
};
export function preferencesForms(): FileForm[] {
  const primary = (value: unknown) => (ccs: string) =>
    write(
      ccs,
      'dashboard-preferences.json',
      typeof value === 'string' ? value : JSON.stringify(value)
    );
  const extra = (value: unknown) => (ccs: string) =>
    write(
      ccs,
      'dashboard-preferences-extra-sources.json',
      typeof value === 'string' ? value : JSON.stringify(value)
    );
  const prefs = (overrides: Record<string, unknown> = {}) => ({
    timeZone: 'America/New_York',
    snapshotCleanup: { auto: false },
    usageLogSources: [
      SOURCE,
      JSONL,
      { id: 'codex-win', tool: 'codex', host: 'windows', path: 'C:\\Users\\fixture\\.codex' },
    ],
    ...overrides,
  });
  const both = (p: unknown, e: unknown) => (ccs: string) => {
    primary(p)(ccs);
    extra(e)(ccs);
  };
  const alexis = { id: 'zcode-alexis', tool: 'zcode', host: 'alexis', path: 'D:/zcode/db' };
  return [
    { name: 'absent', rust: true, setup: () => {} },
    { name: 'saved, three sources', rust: true, setup: primary(prefs()) },
    {
      name: 'saved, keys in another order',
      rust: true,
      setup: primary({
        usageLogSources: [{ path: '/x', host: 'nas1', tool: 'muse', id: 'muse-nas1' }],
        snapshotCleanup: { auto: true },
        timeZone: 'UTC',
      }),
    },
    {
      name: 'zone Europe/London',
      rust: true,
      setup: primary(prefs({ timeZone: 'Europe/London' })),
    },
    {
      name: 'zone Australia/Adelaide',
      rust: true,
      setup: primary(prefs({ timeZone: 'Australia/Adelaide' })),
    },
    {
      name: 'primary is a symlink (defaults)',
      rust: true,
      setup: (ccs) => {
        write(ccs, 'p.json', JSON.stringify(prefs()));
        fs.symlinkSync(path.join(ccs, 'p.json'), path.join(ccs, 'dashboard-preferences.json'));
      },
    },
    {
      name: 'companion adds Alexis',
      rust: true,
      setup: both(prefs(), { usageLogSources: [alexis] }),
    },
    { name: 'companion only', rust: true, setup: extra({ usageLogSources: [alexis] }) },
    { name: 'companion empty object', rust: true, setup: both(prefs(), {}) },
    {
      name: 'companion with a legacy host',
      rust: true,
      setup: both(prefs(), { usageLogSources: [{ ...alexis, host: 'mac', path: '/zcode' }] }),
    },
    {
      name: 'companion with another key',
      rust: true,
      setup: both(prefs(), { usageLogSources: [alexis], other: 1 }),
    },
    { name: 'companion null', rust: true, setup: both(prefs(), 'null') },
    {
      name: 'companion repeats a primary id',
      rust: true,
      setup: both(prefs(), { usageLogSources: [{ ...alexis, id: 'omp-mac' }] }),
    },
    {
      name: 'companion past 64 sources',
      rust: true,
      setup: both(
        prefs({
          usageLogSources: Array.from({ length: 64 }, (_, i) => ({ ...SOURCE, id: `s${i}` })),
        }),
        { usageLogSources: [alexis] }
      ),
    },
    {
      // The admit table is the engine's IANA list now, so this zone is Rust-served.
      name: 'zone outside the admitted list',
      rust: true,
      setup: primary(prefs({ timeZone: 'Asia/Kolkata' })),
    },
    {
      name: 'zone the engine refuses',
      rust: false,
      setup: primary(prefs({ timeZone: 'Mars/Olympus_Mons' })),
    },
    {
      name: 'zone with a space',
      rust: false,
      setup: primary(prefs({ timeZone: 'America/New York' })),
    },
    { name: 'truncated', rust: false, setup: primary('{"timeZone":"UTC",') },
    {
      name: 'remote JSONL migrated',
      rust: false,
      setup: primary(prefs({ usageLogSources: [SOURCE, { ...JSONL, host: 'mac' }] })),
    },
    {
      name: 'duplicate key',
      rust: true,
      setup: primary(
        '{"timeZone":"UTC","timeZone":"UTC","snapshotCleanup":{"auto":true},"usageLogSources":[]}'
      ),
    },
    {
      name: 'parent folder in a path',
      rust: false,
      setup: primary(prefs({ usageLogSources: [{ ...SOURCE, path: '/a/../b' }] })),
    },
    {
      name: 'field mapping null',
      rust: false,
      setup: primary(prefs({ usageLogSources: [{ ...SOURCE, fieldMapping: null }] })),
    },
    {
      name: 'path of 1025 characters',
      rust: false,
      setup: primary(prefs({ usageLogSources: [{ ...SOURCE, path: '/' + 'p'.repeat(1024) }] })),
    },
    {
      name: 'Windows path on a Linux host',
      rust: false,
      setup: primary(prefs({ usageLogSources: [{ ...SOURCE, host: 'nas1', path: 'C:\\logs' }] })),
    },
    { name: 'oversized', rust: false, setup: primary(prefs({ pad: 'x'.repeat(70_000) })) },
    { name: 'companion invalid JSON', rust: false, setup: both(prefs(), '{"usageLogSources":') },
  ];
}

/**
 * The zones the engine admits, from its own IANA table. The Rust reader validates the same
 * table (`chrono-tz`), so the gate is the engine's set rather than a scraped hand list.
 */
export function admittedZones(): string[] {
  const supported = (
    Intl as unknown as { supportedValuesOf?: (key: string) => string[] }
  ).supportedValuesOf?.('timeZone');
  return [...new Set(['UTC', ...(supported ?? [])])];
}

export function writeMeasureFiles(ccs: string): void {
  visibilityForms()[2].setup(ccs);
  preferencesForms()[6].setup(ccs);
  write(ccs, 'account-refresh-settings.json', JSON.stringify({ refreshIntervalSeconds: 60 }));
}

/** RSS of this process's Rust service child (Linux), for the measurement rows. */
export function childRss(_bridge: unknown): number {
  let total = 0;
  try {
    for (const task of fs.readdirSync(`/proc/${process.pid}/task`)) {
      const children = fs.readFileSync(`/proc/${process.pid}/task/${task}/children`, 'utf8');
      for (const pid of children.split(/\s+/).filter(Boolean)) {
        if (fs.readFileSync(`/proc/${pid}/comm`, 'utf8').trim() !== 'aac-http') continue;
        const match = /VmRSS:\s+(\d+) kB/.exec(fs.readFileSync(`/proc/${pid}/status`, 'utf8'));
        if (match) total += Number(match[1]) * 1024;
      }
    }
  } catch {
    /* Not Linux, or the child exited. */
  }
  return total;
}

// ---------------------------------------------------------------------------
// Dashboard rows (shared by registry and Analytics)
// ---------------------------------------------------------------------------

function window(
  overrides: Partial<DashboardAccountWindow> & Record<string, unknown>
): DashboardAccountWindow {
  return {
    key: 'primary',
    label: 'Primary',
    usedPercent: 33.333333333333336,
    remainingPercent: 66.66666666666666,
    resetAt: '2026-11-15T02:00:00.000Z',
    windowMinutes: 300,
    used: null,
    limit: null,
    unit: 'percent',
    kind: 'rate_limit',
    ...overrides,
  } as DashboardAccountWindow;
}

function account(overrides: Partial<DashboardAccount> & Record<string, unknown>): DashboardAccount {
  return {
    id: ID,
    provider: 'codex',
    providerLabel: 'Codex',
    label: 'Work',
    email: 'Work@Example.com',
    plan: 'pro',
    // A load-bearing own undefined member (the projection's `planClass ?? undefined`).
    planClass: undefined,
    platform: 'mac',
    source: 'Codex on Mac',
    status: 'ok',
    message: null,
    fetchedAt: new Date(NOW - 60_000).toISOString(),
    sampledAt: new Date(NOW - 60_000).toISOString(),
    isActive: false,
    windows: [],
    capabilities: { codexProfile: null, claudeProfileId: null, claudePlatforms: [] },
    ...overrides,
  } as DashboardAccount;
}

export function dashboardAccounts(): DashboardAccount[] {
  return [
    account({
      isActive: true,
      capabilities: { codexProfile: 'work', claudeProfileId: null, claudePlatforms: [] },
      windows: [
        window({ unlimited: undefined }),
        window({
          key: 'secondary',
          label: 'Weekly',
          usedPercent: 0.1 + 0.2,
          remainingPercent: 99.7,
          windowMinutes: 10080,
          status: 'cached',
          sampledAt: new Date(NOW - 3 * HOUR).toISOString(),
        }),
      ],
    }),
    account({
      id: 'claude:me',
      provider: 'claude',
      providerLabel: 'Claude',
      label: 'me@example.com',
      email: null,
      status: 'cached',
      source: 'Claude on Mac',
      capabilities: {
        codexProfile: null,
        claudeProfileId: 'me',
        claudePlatforms: ['mac', 'windows'],
      },
      windows: [
        window({ key: 'five_hour', usedPercent: 50, remainingPercent: 50, poolId: 'pool-a' }),
      ],
    }),
    account({
      id: 'antigravity:profile:p1',
      provider: 'antigravity',
      providerLabel: 'Antigravity',
      email: 'agy@example.com',
      status: 'error',
      isActive: true,
      capabilities: {
        codexProfile: null,
        claudeProfileId: null,
        claudePlatforms: [],
        antigravityProfileId: 'p1',
      },
    }),
    account({
      id: 'cursor:acct:0a1b2c3d',
      provider: 'cursor',
      providerLabel: 'Cursor',
      email: 'cursor@example.com',
      hidden: true,
      lifecycle: { state: 'signing_in', jobId: 'job-1' },
      windows: [
        window({
          key: 'credits',
          label: 'Credits',
          kind: 'balance',
          unit: 'usd',
          usedPercent: null,
          remainingPercent: null,
          remaining: 12.5,
          limit: 20,
          used: 7.5,
          unlimited: false,
          enabled: true,
        }),
      ],
    }),
    account({
      id: 'muse:usage',
      provider: 'muse',
      providerLabel: 'Muse',
      email: 'muse@example.com',
      trayHidden: true,
    }),
    account({
      id: 'zai:acct:00000001',
      provider: 'zai',
      providerLabel: 'Z.ai',
      email: null,
      label: 'Z',
    }),
    account({
      id: 'plan-opencode-go-console-mac-0123456789ab',
      provider: 'opencode-go',
      providerLabel: 'OpenCode Go',
      email: null,
    }),
  ];
}

export function dashboard(accounts = dashboardAccounts()) {
  const facts = {
    lifecycleRoutes: true,
    secureTransport: false,
    flows: { muse: 'not_implemented' as const },
  };
  return {
    schemaVersion: 1 as const,
    updatedAt: new Date(NOW).toISOString(),
    settings: { hiddenProviders: ['muse'], hiddenAccountIds: ['claude:me'] },
    providers: buildDashboardProviders(accounts, ['muse'], [], facts),
    accounts,
  };
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

type SpyOn = typeof import('bun:test').spyOn;

export function registryFixture(spyOn: SpyOn, accountsModule: object) {
  const state = {
    dashboard: dashboard(),
    running: new Set<string>(),
    jobs: [] as unknown[],
    entries: [] as unknown[],
  };
  const baseEntries = [
    {
      id: 'cursor:acct:0a1b2c3d',
      provider: 'cursor',
      platform: 'mac',
      sshHost: 'mac-host',
      label: null,
      credential: { kind: 'discover' },
      createdAt: null,
      createdBy: null,
    },
    {
      id: 'zai:acct:00000001',
      provider: 'zai',
      platform: 'windows',
      sshHost: null,
      label: 'Z work',
      credential: { kind: 'aac-key', keyId: 'k1' },
      createdAt: null,
      createdBy: 'dashboard',
    },
    {
      id: 'muse:usage',
      provider: 'muse',
      platform: 'mac',
      sshHost: 'mac-host',
      label: null,
      credential: { kind: 'config-home', homeId: 'h1' },
      createdAt: null,
      createdBy: null,
    },
  ];
  const job = {
    id: 'job-1',
    provider: 'cursor',
    kind: 'device-code',
    mode: 'signin-again',
    accountId: 'cursor:acct:0a1b2c3d',
    profileName: null,
    platform: 'mac',
    state: 'waiting',
    verification: { url: 'https://example.invalid/device', userCode: 'ABCD', expiresAt: null },
    result: { accountId: 'cursor:acct:0a1b2c3d', email: 'cursor@example.com', plan: null },
    error: null,
    startedAt: new Date(NOW - 60_000).toISOString(),
    updatedAt: new Date(NOW).toISOString(),
    expiresAt: new Date(NOW + 600_000).toISOString(),
  };
  spyOn(accountsModule as never, 'readAdditionalEntries' as never).mockImplementation(
    (async () => ({
      mode: 'v2',
      entries: state.entries,
    })) as never
  );
  const env = {
    ccsDir: () => '/nonexistent-fixture',
    runner: () => ({
      runningForAccount: (id: string) => (state.running.has(id) ? { id: 'job-1' } : null),
      list: () => state.jobs,
    }),
    codex: () => ({
      removeRefusal: async (name: string) => (name === 'work' ? null : 'account_active'),
    }),
    claude: () => ({
      findProfile: async (id: string) => (id === 'me' ? { isDefault: true, email: null } : null),
      listTrash: async () => [
        {
          trashId: 'claude-old-20261101T000000Z',
          provider: 'claude',
          label: 'old',
          trashedAt: '2026-11-01T00:00:00Z',
          purgeAfter: '2026-12-01T00:00:00Z',
          state: 'trashed',
        },
      ],
    }),
    antigravity: () => ({ removeRefusal: async () => null }),
    muse: () => ({}),
    providerFacts: () => ({
      lifecycleRoutes: true,
      secureTransport: false,
      flows: { codex: 'tool_missing' },
      remove: { claude: true },
    }),
    getDashboard: async () => state.dashboard,
    keyStore: () => ({
      info: async (_provider: string, keyId: string) =>
        keyId === 'k1'
          ? { fingerprint: 'sha256:0123456789abcdef', last4: 'x7Qa', storedOn: 'windows' }
          : null,
    }),
  };
  const forms = [
    {
      name: 'mixed rows',
      rust: true,
      setup: () => {
        state.dashboard = dashboard();
        state.entries = baseEntries;
        state.running.clear();
        state.jobs = [];
      },
    },
    {
      name: 'running sign-ins and a job',
      rust: true,
      setup: () => {
        state.running = new Set([
          'cursor:acct:0a1b2c3d',
          'plan-opencode-go-console-mac-0123456789ab',
        ]);
        state.jobs = [job];
      },
    },
    {
      name: 'no registry entries',
      rust: true,
      setup: () => {
        state.entries = [];
        state.running.clear();
        state.jobs = [];
      },
    },
    {
      name: 'empty dashboard',
      rust: true,
      setup: () => {
        state.dashboard = dashboard([]);
      },
    },
    {
      name: 'Claude platforms not a list (TS throws)',
      rust: false,
      compared: false,
      setup: () => {
        state.dashboard = dashboard([
          account({
            id: 'claude:me',
            provider: 'claude',
            capabilities: {
              codexProfile: null,
              claudeProfileId: 'me',
              claudePlatforms: 7 as never,
            },
          }),
        ]);
      },
    },
  ];
  return { env: env as never, forms };
}

/** Generated registry facts: every row kind, refusals, nullish chains and key metadata. */
export function registryTable(): RegistryListingFacts[] {
  const tables: RegistryListingFacts[] = [];
  const providers = dashboard().providers as never;
  const rows = dashboardAccounts();
  const refusals = [null, 'account_active', 'unavailable', 'signin_running'];
  for (let i = 0; i < 48; i++) {
    const row = {
      ...rows[i % rows.length],
      ...(i % 3 === 0 ? { label: undefined } : {}),
      ...(i % 4 === 0 ? { hidden: true } : {}),
      ...(i % 5 === 0 ? { lifecycle: undefined } : {}),
    } as DashboardAccount;
    const kind = (['codex', 'claude', 'antigravity', 'entry', 'other'] as const)[i % 5];
    const refusal = refusals[i % refusals.length];
    const fact =
      kind === 'codex'
        ? { kind, row, refusal, available: i % 2 === 0 }
        : kind === 'claude'
          ? {
              kind,
              row: {
                ...row,
                capabilities: { ...row.capabilities, claudePlatforms: i % 2 ? ['mac'] : [] },
              },
              profile: i % 3 ? { isDefault: i % 2 === 0, email: i % 4 ? null : 'a@b.c' } : null,
              removeAllowed: i % 7 !== 0,
            }
          : kind === 'antigravity'
            ? {
                kind,
                row,
                refusal,
                reason: [null, 'tool_missing', 'not_implemented', 'secure_transport_required'][
                  i % 4
                ],
                removeAllowed: i % 2 === 0,
              }
            : kind === 'entry'
              ? {
                  kind,
                  row,
                  entry: {
                    id: row.id,
                    provider: (['cursor', 'qwen', 'zai', 'muse', 'kimi-code'] as const)[i % 5],
                    platform: i % 2 ? 'mac' : 'windows',
                    sshHost: i % 3 ? 'h' : null,
                    label: i % 2 ? 'L' : null,
                    credential: i % 2 ? { kind: 'aac-key', keyId: 'k' } : { kind: 'discover' },
                    createdAt: null,
                    createdBy: null,
                  },
                  key:
                    i % 4 === 1
                      ? { fingerprint: 'sha256:00', last4: '1234', storedOn: 'mac' }
                      : null,
                  museSignIn: i % 3 === 0 ? true : i % 3 === 1 ? false : null,
                  running: i % 2 === 0,
                }
              : {
                  kind,
                  row: i % 2 ? row : { ...row, id: 'plan-opencode-go-console-mac-0123456789ab' },
                  running: i % 3 === 0,
                  walletRemove: i % 2 === 0,
                };
    tables.push({ providers, rows: [fact as never], jobs: [], trash: [] });
  }
  return tables;
}

// ---------------------------------------------------------------------------
// Analytics
// ---------------------------------------------------------------------------

function historyData(accounts: DashboardAccount[]): AccountAnalyticsHistoryData {
  let data: AccountAnalyticsHistoryData | null = null;
  const old = accounts.map((a) =>
    a.provider === 'codex'
      ? {
          ...a,
          windows: [...a.windows, window({ key: 'retired', label: 'Retired', usedPercent: 12 })],
        }
      : a
  );
  for (const [index, at] of [
    NOW - 6 * DAY,
    NOW - 3 * DAY,
    NOW - 26 * HOUR,
    NOW - HOUR,
    NOW - 5 * 60_000,
  ].entries())
    data = appendAccountAnalyticsSnapshot(
      data,
      (index < 2 ? old : accounts).map((a) => ({
        ...a,
        sampledAt: new Date(at).toISOString(),
        fetchedAt: new Date(at).toISOString(),
        windows: a.windows.map((w) => ({
          ...w,
          usedPercent: typeof w.usedPercent === 'number' ? w.usedPercent + index : w.usedPercent,
        })),
      })),
      at
    );
  return data as AccountAnalyticsHistoryData;
}

export function analyticsTable(): Array<{
  rangeInput: AccountAnalyticsRangeInput;
  compose: AccountAnalyticsComposeInput;
}> {
  const out: Array<{
    rangeInput: AccountAnalyticsRangeInput;
    compose: AccountAnalyticsComposeInput;
  }> = [];
  const accounts = dashboardAccounts().map((a) =>
    a.provider === 'antigravity'
      ? {
          ...a,
          status: 'ok' as const,
          windows: [
            window({ key: 'gemini-weekly', label: 'Gemini weekly, in the Antigravity app' }),
            window({
              key: 'cli-gemini-weekly',
              label: 'Gemini weekly, in T3 (Antigravity CLI)',
              usedPercent: 34,
              remainingPercent: 66,
            }),
          ],
        }
      : a
  );
  // The deployment adds a current CLI window to a range with only app history.
  const history = historyData(
    accounts.map((a) =>
      a.provider === 'antigravity' ? { ...a, windows: a.windows.slice(0, 1) } : a
    )
  );
  const presets = ['24h', '7d', '30d', '90d', 'all'] as const;
  for (let i = 0; i < 60; i++) {
    const preset = presets[i % presets.length];
    const query: AccountAnalyticsQuery = {
      platform: i % 2 ? 'mac' : 'windows',
      range: preset,
      provider: (['all', 'codex', 'claude', 'cursor'] as const)[i % 4],
      account: i % 6 === 0 ? 'codex:work' : 'all',
      tz: 'UTC',
    };
    const now = NOW - (i % 7) * 37 * 60_000;
    const bounds = i % 3 === 0 ? { oldest: NOW - 120 * DAY } : null;
    const retainedFrom = bounds ? bounds.oldest : now - 31 * DAY;
    const provisional = resolveAccountAnalyticsRange(query, now, retainedFrom, bounds ?? undefined);
    const oldestQuota = i % 4 === 0 ? Infinity : NOW - (10 + i) * DAY;
    const rangeInput: AccountAnalyticsRangeInput = {
      query,
      now,
      retainedFrom,
      bounds,
      oldestQuota,
      coverageOldestHourAt: i % 5 === 0 ? null : NOW - (20 + i) * DAY,
      provisional,
      todayMidnight: () => Date.parse('2026-11-14T00:00:00.000Z'),
    };
    const { availableFrom, range } = analyticsAvailableRange(rangeInput);
    const table = defaultAnalyticsProviderTable();
    const shuffled =
      i % 8 === 0 ? table.map((entry) => ({ ...entry, order: entry.id.length })) : table;
    const variant = i % 9 === 0 ? accounts.map((a) => ({ ...a, hidden: undefined })) : accounts;
    out.push({
      rangeInput,
      compose: {
        query,
        now,
        tz: 'UTC',
        range,
        availableFrom,
        resolution: i % 10 === 0 ? 'day' : undefined,
        available: i % 11 !== 0,
        stored: i % 2 === 0,
        oldestQuota,
        history: i % 13 === 0 ? null : history,
        dashboard: dashboard(variant as DashboardAccount[]) as never,
        providerTable: shuffled,
        providersWithActivity: i % 2 ? ['codex', 'omp'] : [],
      },
    });
  }
  return out;
}
