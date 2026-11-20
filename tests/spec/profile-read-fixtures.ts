/**
 * Fixture forms for tests/spec/profile-read-parity.ts (M3 part 4): the Claude desktop manifest
 * and remote history texts, the Codex registry, logins, collector rows and renewal notes, and
 * the Antigravity runtime's inventory and accounts. Everything lives in the worker's temporary
 * CCS home; remote reads, collectors and the runtime are fixture closures. Each form says
 * whether Rust must answer it (`rust`) or the TS reader/composer (with a logged fallback).
 */
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import yaml from 'js-yaml';

/** Empty the fixture CCS folder, keeping the Rust service's private run folder. */
export function resetCcs(ccs: string): void {
  fs.chmodSync(ccs, 0o700);
  for (const entry of fs.readdirSync(ccs))
    if (entry !== 'run') fs.rmSync(path.join(ccs, entry), { recursive: true, force: true });
}

/** RSS of this process's Rust service children (Linux), for the measurement rows. */
export function childRss(): number {
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

/** What the fixture closures answer; each form sets it before its request. */
export interface FixtureState {
  /** Remote history text by launcher profile path; an Error makes the SSH read fail. */
  histories: Map<string, string | null | Error>;
  inventory: unknown;
  accounts: unknown;
  rows: unknown[];
  renewals: Array<{ name: string; state: string; message?: string }>;
  /** Open progress to seed for this form (profile id), or none. */
  openProfile: string | null;
}

export function emptyState(): FixtureState {
  return {
    histories: new Map(),
    inventory: { schemaVersion: 1, hostId: 'ubuntu', profiles: [] },
    accounts: [],
    rows: [],
    renewals: [],
    openProfile: null,
  };
}

export interface Form {
  name: string;
  route: string;
  rust: boolean;
  /** False when shadow mode has nothing to compare (TS answered an error). */
  compared?: boolean;
  /** False when a TS owner read fails before any composition (neither side composes). */
  reached?: boolean;
  role?: 'session' | 'device';
  setup: (ccs: string, home: string, state: FixtureState) => void;
}

function write(file: string, contents: string | Buffer, mode = 0o600): void {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  fs.writeFileSync(file, contents, { mode });
  fs.chmodSync(file, mode);
}

// ── Claude desktop ─────────────────────────────────────────────────────────

const MANIFEST = 'claude-desktop-profiles.json';

function manifest(ccs: string, profiles: unknown[], extra: Record<string, unknown> = {}): void {
  write(path.join(ccs, MANIFEST), JSON.stringify({ version: 1, profiles, ...extra }, null, 2));
}

const WORK = {
  id: 'work',
  email: 'work@example.com',
  mac: {
    launcherName: 'Claude — Wörk 🚀',
    launcherPath: '/Applications/Claude Work.app',
    profilePath: '/Users/fixture/Library/Application Support/Claude-Work',
    sshHost: 'mac',
    isDefault: true,
  },
  windows: {
    launcherName: 'クロード (仕事)',
    launcherPath: 'C:\\Program Files\\Claude\\Claude.exe',
    profilePath: 'C:\\Users\\fixture\\AppData\\Roaming\\Claude-Work',
    startMenuPath: 'C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Claude Work.lnk',
    sshHost: 'pc.lan',
  },
};
const HOME = {
  id: 'home_2',
  email: 'jürgen@exämple.de',
  mac: {
    launcherName: 'Claude Home',
    launcherPath: '/Applications/Claude Home',
    profilePath: '/Users/fixture/Library/Application Support/Claude-Home',
    sshHost: 'mac',
  },
  windows: { launcherName: 'Home', profilePath: 'C:\\Users\\fixture\\Claude-Home' },
};
const PLAIN = { email: 'plain@example.com', mac: { launcherName: 'Claude' } };

export function desktopProfileForms(): Form[] {
  const route = '/api/claude/desktop-profiles';
  const form = (name: string, rust: boolean, setup: Form['setup'], extra: Partial<Form> = {}) => ({
    name,
    route,
    rust,
    setup,
    ...extra,
  });
  return [
    form('absent manifest (empty registry)', true, () => {}),
    form('three profiles, Unicode launchers', true, (ccs) => manifest(ccs, [WORK, HOME, PLAIN])),
    form('device token reads the tray route', true, (ccs) => manifest(ccs, [WORK, PLAIN]), {
      role: 'device',
    }),
    form('an Open in progress (TS adds openOperation)', true, (ccs, _home, state) => {
      manifest(ccs, [WORK, HOME]);
      state.openProfile = 'work';
    }),
    form('duplicate JSON keys (JSON.parse keeps the last)', true, (ccs) =>
      write(
        path.join(ccs, MANIFEST),
        '{"version":1,"profiles":[{"email":"a@example.com","email":"b@example.com","mac":{"launcherName":"A"}}]}'
      )
    ),
    form('mode 0644 (TS does not check the mode)', true, (ccs) => {
      manifest(ccs, [PLAIN]);
      fs.chmodSync(path.join(ccs, MANIFEST), 0o644);
    }),
    form('duplicate profile ids (TS 500)', false, (ccs) => manifest(ccs, [WORK, WORK])),
    form('invalid email (TS 500)', false, (ccs) =>
      manifest(ccs, [{ email: 'nobody', mac: { launcherName: 'X' } }])
    ),
    form('version 2 (TS 500)', false, (ccs) =>
      write(path.join(ccs, MANIFEST), JSON.stringify({ version: 2, profiles: [] }))
    ),
    form('control character in a launcher name (TS 500)', false, (ccs) =>
      manifest(ccs, [{ email: 'a@example.com', mac: { launcherName: 'A\u0007' } }])
    ),
    form('ideographic-space launcher name (TS 500)', false, (ccs) =>
      manifest(ccs, [{ email: 'a@example.com', mac: { launcherName: '\u3000' } }])
    ),
    form('1001 profiles (TS 500)', false, (ccs) =>
      manifest(
        ccs,
        Array.from({ length: 1001 }, (_, i) => ({
          email: `p${i}@example.com`,
          mac: { launcherName: 'P' },
        }))
      )
    ),
    form('byte-order mark (TS 500)', false, (ccs) =>
      write(path.join(ccs, MANIFEST), '\ufeff' + JSON.stringify({ version: 1, profiles: [] }))
    ),
    form('over 1 MiB (TS 500)', false, (ccs) =>
      manifest(ccs, [], { pad: 'x'.repeat(1024 * 1024 + 16) })
    ),
    form('invalid UTF-8 inside a name (TS decodes U+FFFD)', false, (ccs) =>
      write(
        path.join(ccs, MANIFEST),
        Buffer.concat([
          Buffer.from('{"version":1,"profiles":[{"email":"a@example.com","mac":{"launcherName":"A'),
          Buffer.from([0xff]),
          Buffer.from('"}}]}'),
        ])
      )
    ),
    form('symlinked manifest (TS follows it)', false, (ccs) => {
      write(
        path.join(ccs, 'manifest-target.json'),
        JSON.stringify({ version: 1, profiles: [PLAIN] })
      );
      fs.symlinkSync(path.join(ccs, 'manifest-target.json'), path.join(ccs, MANIFEST));
    }),
    form('a folder in place of the manifest (TS 500)', false, (ccs) =>
      fs.mkdirSync(path.join(ccs, MANIFEST))
    ),
  ];
}

function history(samples: unknown[], version = 2): string {
  return JSON.stringify({ version, samples });
}

export function desktopUsageForms(): Form[] {
  const mac = '/api/claude/desktop-profiles/usage?platform=mac';
  const windows = '/api/claude/desktop-profiles/usage?platform=windows';
  const remote = (id: string) => ({
    launcherName: id,
    profilePath: `/Users/fixture/${id}`,
    sshHost: 'mac',
  });
  const profiles = [
    { id: 'cached', email: 'cached@example.com', mac: remote('cached') },
    { id: 'missing', email: 'missing@example.com', mac: remote('missing') },
    { id: 'empty', email: 'empty@example.com', mac: remote('empty') },
    { id: 'failed', email: 'failed@example.com', mac: remote('failed') },
    { id: 'v1', email: 'v1@example.com', mac: remote('v1') },
    { id: 'negative', email: 'negative@example.com', mac: remote('negative') },
    { id: 'huge', email: 'huge@example.com', mac: remote('huge') },
    { id: 'badtimes', email: 'badtimes@example.com', mac: remote('badtimes') },
    { email: 'unconfigured@example.com', mac: { launcherName: 'No SSH' } },
    { id: 'pc', email: 'pc@example.com', windows: { launcherName: 'PC' } },
  ];
  const histories = (state: FixtureState) => {
    state.histories.set(
      '/Users/fixture/cached',
      history([
        { t: 1_700_000_000_000, u: { fh: 99 } },
        { t: 1_793_612_345_678, u: { fh: 12.5, sd: 40, so: 0, sn: 7.25, xu: -1 } },
        { t: 1_793_612_345_678.5, u: { fh: 1 } },
        { t: 1, u: { fh: 3 } },
      ])
    );
    state.histories.set('/Users/fixture/missing', null);
    state.histories.set('/Users/fixture/empty', history([]));
    state.histories.set('/Users/fixture/failed', new Error('ssh: connect to host mac: refused'));
    state.histories.set('/Users/fixture/v1', history([{ t: 1, u: { fh: 1 } }], 1));
    state.histories.set('/Users/fixture/negative', history([{ t: 5, u: { fh: -1, sd: 'x' } }]));
    state.histories.set('/Users/fixture/huge', 'x'.repeat(1024 * 1024 + 1));
    state.histories.set(
      '/Users/fixture/badtimes',
      history([{ t: -1, u: { fh: 1 } }, { t: 8.64e15 + 1, u: { fh: 1 } }, { u: { fh: 1 } }])
    );
  };
  return [
    { name: 'usage: no manifest', route: mac, rust: true, setup: () => {} },
    {
      name: 'usage: cached, signed out, empty, failed, unsupported, unconfigured',
      route: mac,
      rust: true,
      setup: (ccs, _home, state) => {
        manifest(ccs, profiles);
        histories(state);
      },
    },
    {
      name: 'usage: Windows launchers without SSH (unavailable)',
      route: windows,
      rust: true,
      setup: (ccs, _home, state) => {
        manifest(ccs, profiles);
        histories(state);
      },
    },
    {
      name: 'usage: a corrupt history (TS decides JSON.parse)',
      route: mac,
      rust: false,
      setup: (ccs, _home, state) => {
        manifest(ccs, [profiles[0]]);
        state.histories.set('/Users/fixture/cached', '{"version":2,"samples":[');
      },
    },
    {
      name: 'usage: a sample past year 9999 (TS formats the extended year)',
      route: mac,
      rust: false,
      setup: (ccs, _home, state) => {
        manifest(ccs, [profiles[0]]);
        state.histories.set('/Users/fixture/cached', history([{ t: 8.64e15, u: { fh: 5 } }]));
      },
    },
    {
      name: 'usage: an invalid manifest (TS 500 before any read)',
      route: mac,
      rust: false,
      compared: false,
      reached: false,
      setup: (ccs) => manifest(ccs, [WORK, WORK]),
    },
  ];
}

// ── Codex ──────────────────────────────────────────────────────────────────

function base64url(value: unknown): string {
  return Buffer.from(JSON.stringify(value)).toString('base64url');
}

/** An unsigned fixture JWT; nothing here is a real credential. */
function idToken(claims: Record<string, unknown>): string {
  return `${base64url({ alg: 'none', typ: 'JWT' })}.${base64url(claims)}.fixture`;
}

function login(file: string, claims: Record<string, unknown> | string | null): void {
  const token = typeof claims === 'string' || claims === null ? claims : idToken(claims);
  write(file, JSON.stringify({ tokens: { id_token: token, account_id: null } }));
}

const AUTH = 'https://api.openai.com/auth';

function registry(
  ccs: string,
  profiles: Array<[string, string | null]>,
  defaultName: string | null
): void {
  write(
    path.join(ccs, 'codex-profiles.yaml'),
    yaml.dump(
      {
        version: '1.0',
        default: defaultName,
        profiles: Object.fromEntries(
          profiles.map(([name, lastUsed]) => [
            name,
            { type: 'codex', created: '2026-01-01T00:00:00.000Z', last_used: lastUsed },
          ])
        ),
      },
      { indent: 2, lineWidth: -1 }
    )
  );
}

/** Saved logins: valid, signed out (no file), sparse, malformed, Unicode, numeric name. */
function codexSetup(ccs: string, home: string): void {
  registry(
    ccs,
    [
      ['work', '2026-11-10T08:00:00.000Z'],
      ['home', null],
      ['sparse', '2026-11-01T00:00:00.000Z'],
      ['broken', null],
      ['umlaut', '2026-11-12T00:00:00.000Z'],
      ['123', null],
    ],
    'work'
  );
  const instances = path.join(ccs, 'codex-instances');
  const work = {
    email: 'work@example.com',
    sub: 'user-work',
    [AUTH]: { chatgpt_plan_type: 'pro', chatgpt_account_id: 'acct-work', chatgpt_user_id: 'u-1' },
  };
  login(path.join(instances, 'work', 'auth.json'), work);
  login(path.join(instances, 'sparse', 'auth.json'), { sub: 'user-sparse' });
  login(path.join(instances, 'broken', 'auth.json'), 'not-a-jwt');
  login(path.join(instances, 'umlaut', 'auth.json'), {
    email: 'jürgen@exämple.de',
    [AUTH]: { chatgpt_plan_type: 'plus', chatgpt_account_id: 'acct-umlaut' },
  });
  login(path.join(instances, '123', 'auth.json'), {
    email: 'numbers@example.com',
    [AUTH]: { chatgpt_plan_type: 'team', chatgpt_account_id: 'acct-123' },
  });
  // The live login is the saved "work" login, so "activated" names it.
  login(path.join(home, '.codex', 'auth.json'), work);
}

function env(values: { CODEX_HOME?: string; CCS_CODEX_PROFILE?: string }): void {
  delete process.env.CODEX_HOME;
  delete process.env.CCS_CODEX_PROFILE;
  Object.assign(process.env, values);
}

export function codexProfileForms(): Form[] {
  const route = '/api/codex/profiles';
  return [
    {
      name: 'codex: empty registry, no live login',
      route,
      rust: true,
      setup: () => env({}),
    },
    {
      name: 'codex: saved logins, registry default, live login activated',
      route,
      rust: true,
      setup: (ccs, home) => {
        codexSetup(ccs, home);
        env({});
      },
    },
    {
      name: 'codex: CODEX_HOME names a saved profile',
      route,
      rust: true,
      setup: (ccs, home) => {
        codexSetup(ccs, home);
        env({ CODEX_HOME: ` ${path.join(ccs, 'codex-instances', 'home')}\t` });
      },
    },
    {
      name: 'codex: CODEX_HOME outside the saved profiles',
      route,
      rust: true,
      setup: (ccs, home) => {
        codexSetup(ccs, home);
        env({ CODEX_HOME: path.join(home, '.codex') });
      },
    },
    {
      name: 'codex: whitespace CODEX_HOME, CCS_CODEX_PROFILE wins',
      route,
      rust: true,
      setup: (ccs, home) => {
        codexSetup(ccs, home);
        env({ CODEX_HOME: '\u3000 ', CCS_CODEX_PROFILE: 'umlaut' });
      },
    },
    {
      name: 'codex: stale CCS_CODEX_PROFILE (TS logs it)',
      route,
      rust: false,
      setup: (ccs, home) => {
        codexSetup(ccs, home);
        env({ CCS_CODEX_PROFILE: 'gone' });
      },
    },
    {
      name: 'codex: unreadable registry (TS 500)',
      route,
      rust: false,
      compared: false,
      reached: false,
      setup: (ccs) => {
        write(path.join(ccs, 'codex-profiles.yaml'), 'profiles: [unclosed');
        env({});
      },
    },
  ];
}

const FUTURE = '2099-01-01T00:00:00Z';

function quotaRow(profile: string, overrides: Record<string, unknown> = {}): unknown {
  return {
    account_id: `ccsx:${profile}`,
    profile,
    provider: 'codex',
    displayName: profile,
    quotaStatus: 'ok',
    fetchedAt: '2026-11-15T09:00:00.000Z',
    needsReauth: false,
    quotaWindows: [
      { key: 'five_hour', label: '5h', usedPercent: 25, resetAt: FUTURE, windowMinutes: 300 },
      { key: 'seven_day', label: 'week', usedPercent: 60.5, resetAt: null },
    ],
    ...overrides,
  };
}

export function codexQuotaForms(): Form[] {
  const route = '/api/codex/profiles/quotas';
  const setup =
    (rows: unknown[], renewals: FixtureState['renewals'] = []): Form['setup'] =>
    (ccs, home, state) => {
      codexSetup(ccs, home);
      env({});
      state.rows = rows;
      state.renewals = renewals;
    };
  return [
    { name: 'quotas: empty registry', route, rust: true, setup: () => env({}) },
    {
      name: 'quotas: available, stale cached, passed resets, expired, unavailable, retrieving',
      route,
      rust: true,
      setup: setup(
        [
          quotaRow('work'),
          // A stale local reading sampled before its reset, which has now passed.
          quotaRow('sparse', {
            staleAsOf: '2020-01-01T04:00:00+05:30',
            quotaWindows: [
              {
                key: 'five_hour',
                label: '5h',
                usedPercent: 80,
                resetAt: '2020-01-01T00:00:00.500Z',
              },
              { key: 'Límite 週', label: 'Límite 週 ✨', usedPercent: 3, resetAt: '' },
              { key: 'seven_day', usedPercent: 0, resetAt: '2020-01-01T00:00:00Z' },
            ],
          }),
          // A reading taken after a reset that has passed is current, not history.
          quotaRow('123', {
            fetchedAt: '2026-11-15T09:00:00Z',
            quotaWindows: [
              { key: 'five_hour', label: '', usedPercent: 1, resetAt: '2026-01-01T00:00:00-08:00' },
            ],
          }),
          quotaRow('umlaut', { quotaStatus: 'error', needsReauth: true }),
          quotaRow('broken', { quotaStatus: 'error', needsReauth: 0 }),
          // A later row for the same profile wins (Map semantics).
          quotaRow('work', {
            quotaWindows: [{ key: 'seven_day', usedPercent: 12, resetAt: FUTURE }],
          }),
        ],
        [
          { name: 'umlaut', state: 'ok' },
          {
            name: 'sparse',
            state: 'failed',
            message: 'OpenAI rejected the saved login; sign in again.',
          },
        ]
      ),
    },
    {
      name: 'quotas: renewal failed without a row, ok row without windows',
      route,
      rust: true,
      setup: setup(
        [quotaRow('work', { quotaWindows: [] })],
        [
          { name: 'home', state: 'failed', message: 'Sign in again.' },
          { name: '123', state: 'failed' },
        ]
      ),
    },
    {
      name: 'quotas: a reset time TS parses with its legacy parser',
      route,
      rust: false,
      setup: setup([
        quotaRow('work', {
          quotaWindows: [{ key: 'five_hour', usedPercent: 5, resetAt: 'Jan 1 2020 00:00 GMT' }],
        }),
      ]),
    },
    {
      name: 'quotas: a non-finite reading JSON cannot carry',
      route,
      rust: false,
      // The input never leaves TS, so shadow has nothing to compare.
      compared: false,
      setup: setup([quotaRow('work', { quotaWindows: [{ key: 'five_hour', usedPercent: NaN }] })]),
    },
  ];
}

// ── Antigravity ────────────────────────────────────────────────────────────

function agProfile(id: string, overrides: Record<string, unknown> = {}): unknown {
  return {
    id,
    email: `${id}@example.com`,
    plan: 'Google AI Pro',
    hostId: 'ubuntu',
    available: true,
    selected: false,
    runtimeVerified: true,
    verifiedAt: '2026-11-15T08:00:00.000Z',
    ...overrides,
  };
}

export function antigravityInventoryForms(): Form[] {
  const route = '/api/antigravity/profiles';
  const inventory =
    (value: unknown): Form['setup'] =>
    (_ccs, _home, state) => {
      state.inventory = value;
    };
  const full = {
    schemaVersion: 1,
    hostId: 'ubuntu',
    nativeUpdatePaused: { installedVersion: '1.20.3-rc.1_b' },
    runtimeServiceProblem: {
      reason: 'parser-mismatch',
      module: 'antigravity_parser.v2',
      python: '3.12',
      builtFor: '3.11',
      exitStatus: 1,
      detail: 'dropped',
    },
    profiles: [
      agProfile('main', { plan: '  Google AI Pro ✨ ', selected: true }),
      agProfile('tz', { verifiedAt: '2026-11-15T13:30:00.123456+05:30', available: 'yes' }),
      agProfile('old', { verifiedAt: '1999-12-31T23:59:59Z', plan: 'sk-live-token' }),
      agProfile('legacy', { verifiedAt: 'Nov 15 2026', plan: null }),
      agProfile('bad-date', { verifiedAt: '2026-13-45T99:99:99Z', email: ' ünï@exämple.de ' }),
    ],
  };
  return [
    {
      name: 'inventory: empty (no runtime)',
      route,
      rust: true,
      setup: inventory({ schemaVersion: 1, hostId: 'ubuntu', profiles: [] }),
    },
    {
      name: 'inventory: leftover paused marker ignored, runtime problem, Unicode, offsets',
      route,
      rust: true,
      setup: inventory(full),
    },
    { name: 'inventory: device token', route, rust: true, role: 'device', setup: inventory(full) },
    {
      name: 'inventory: truthy leftover pause ignored, unknown problem reason',
      route,
      rust: true,
      setup: inventory({
        schemaVersion: 1,
        hostId: 'ubuntu',
        nativeUpdatePaused: true,
        runtimeServiceProblem: { reason: 'disk-full', exitStatus: 300 },
        profiles: [agProfile('main')],
      }),
    },
    {
      name: 'inventory: duplicate ids (TS 500)',
      route,
      rust: false,
      compared: false,
      setup: inventory({
        schemaVersion: 1,
        hostId: 'ubuntu',
        profiles: [agProfile('a'), agProfile('a')],
      }),
    },
    {
      name: 'inventory: another host (TS 500)',
      route,
      rust: false,
      compared: false,
      setup: inventory({ schemaVersion: 1, hostId: 'mac', profiles: [] }),
    },
    {
      name: 'inventory: 17 profiles (TS 500)',
      route,
      rust: false,
      compared: false,
      setup: inventory({
        schemaVersion: 1,
        hostId: 'ubuntu',
        profiles: Array.from({ length: 17 }, (_, i) => agProfile(`p${i}`)),
      }),
    },
  ];
}

function agAccount(id: string, overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: `antigravity:profile:${id}`,
    provider: 'antigravity',
    providerLabel: 'private label',
    label: 'private',
    email: `${id}@example.com`,
    plan: 'pro',
    platform: 'ubuntu',
    source: 'private',
    status: 'ok',
    message: 'private',
    fetchedAt: '2026-11-15T09:00:00.000Z',
    sampledAt: '2026-11-15T08:59:00.000Z',
    isActive: true,
    windows: [],
    capabilities: {
      codexProfile: 'leak',
      claudeProfileId: null,
      claudePlatforms: [],
      antigravityProfileId: id,
      antigravityHostIds: ['ubuntu'],
    },
    ...overrides,
  };
}

function agWindow(key: string, overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    key,
    label: 'Gemini Models',
    usedPercent: 40,
    resetAt: '2099-01-01T00:00:00Z',
    windowMinutes: 300,
    ...overrides,
  };
}

/** Account rows covering every status, cache reason, plan family and window member. */
export function antigravityAccounts(): unknown[] {
  return [
    agAccount('ok', {
      windows: [
        agWindow('gemini-models', {
          poolId: 'pool/gemini@models:1',
          poolIdSource: 'provider-id',
          poolLabel: ' Gemini ✨ pool ',
          modelIds: ['gemini-3.8-flash', 'gemini-3.8-flash', 'bad id', 'gemini-3.1-pro'],
          enabled: true,
        }),
        agWindow('google-ai-credits', {
          label: 'whatever',
          kind: 'balance',
          used: 12.5,
          limit: 100,
          remaining: null,
          unit: 'credits',
          unlimited: false,
          resetAt: null,
        }),
        agWindow('gemini-4-argon', {
          usedPercent: undefined,
          remainingPercent: 70,
          expiresAt: '2099-02-01T00:00:00+01:00',
        }),
        agWindow('https://leak', {}),
        agWindow('weekly', { label: 'https://example.com/x', windowMinutes: 0, kind: 'nonsense' }),
        { key: 'empty-window' },
        agWindow('passed', { resetAt: '2020-01-01T00:00:00Z', label: 'Límite semanal 週' }),
        agWindow('spend', { kind: 'spend', windowMinutes: 600000, usedPercent: 101 }),
      ],
    }),
    agAccount('rate', {
      status: 'cached',
      cacheReason: 'rate_limited',
      plan: 'Google AI Ultra 5x',
    }),
    agAccount('changed', {
      status: 'cached',
      cacheReason: 'source_changed',
      plan: 'ANTIGRAVITY  starter_quota',
    }),
    agAccount('failed', {
      status: 'cached',
      cacheReason: 'read_failed',
      plan: 'Gemini Code Assist in Google One AI Pro',
    }),
    agAccount('pending', {
      status: 'cached',
      cacheReason: 'refresh_pending',
      plan: 'Workspace Business Plus',
    }),
    agAccount('unknown-cache', {
      status: 'cached',
      cacheReason: 'teapot',
      plan: 'Mystery Tier ✨',
    }),
    agAccount('signin', {
      status: 'needs_sign_in',
      windows: [agWindow('w')],
      sampledAt: null,
      fetchedAt: null,
      plan: 'pro',
    }),
    agAccount('down', { status: 'unavailable', plan: null }),
    agAccount('unbound', {
      status: 'error',
      statusReason: 'identity_unbound',
      plan: 'Google Workspace',
    }),
    agAccount('weird', {
      status: 'refreshing',
      statusReason: 'identity_unbound',
      email: ' Weird@Example.org ',
      isActive: 'yes',
      plan: 'Google AI Pro (trial)',
      sampledAt: '2026-10-01T00:00:00-07:00',
      capabilities: { antigravityProfileId: 'weird', antigravityHostIds: 'ubuntu-box' },
    }),
    agAccount('before-removal', {
      plan: 'free',
      sampledAt: '2026-11-01T23:59:59.999Z',
      windows: [agWindow('passed', { resetAt: '2026-11-01T00:00:00Z' })],
    }),
    agAccount('ultra', {
      plan: 'Gemini Code Assist in Google One AI Ultra',
      sampledAt: '2026-10-15T12:00:00Z',
      capabilities: { antigravityProfileId: 'ultra' },
    }),
  ];
}

export function antigravityQuotaForms(): Form[] {
  const route = '/api/antigravity/profiles/quotas';
  const accounts =
    (value: unknown): Form['setup'] =>
    (_ccs, _home, state) => {
      state.accounts = value;
    };
  return [
    { name: 'ag quotas: no accounts', route, rust: true, setup: accounts([]) },
    {
      name: 'ag quotas: every status, cache reason, plan and window form',
      route,
      rust: true,
      setup: accounts(antigravityAccounts()),
    },
    {
      name: 'ag quotas: refresh=true (the runtime refreshes; TS reads)',
      route: `${route}?refresh=true`,
      rust: true,
      setup: accounts(antigravityAccounts().slice(0, 2)),
    },
    {
      name: 'ag quotas: another provider (TS 500)',
      route,
      rust: false,
      compared: false,
      setup: accounts([agAccount('x', { provider: 'codex' })]),
    },
    {
      name: 'ag quotas: kind as an array (TS coerces it)',
      route,
      rust: false,
      setup: accounts([agAccount('x', { windows: [agWindow('w', { kind: ['spend'] })] })]),
    },
    {
      name: 'ag quotas: own remaining member left undefined',
      route,
      rust: false,
      // The input never leaves TS, so shadow has nothing to compare.
      compared: false,
      setup: accounts([agAccount('x', { windows: [agWindow('w', { remaining: undefined })] })]),
    },
  ];
}

// ── Part 3 files read through the capped descriptor ────────────────────────

export interface HardenedForm {
  name: string;
  route: string;
  rust: boolean;
  setup: (ccs: string) => void;
}

function fifo(file: string): void {
  execFileSync('mkfifo', ['-m', '600', file]);
}

/** Visibility and preference files that only a descriptor check can classify safely. */
export function hardenedForms(): HardenedForm[] {
  const visibility = '/api/accounts/visibility';
  const preferences = '/api/accounts/preferences';
  const companion = 'dashboard-preferences-extra-sources.json';
  const primary = 'dashboard-preferences.json';
  const saved = JSON.stringify({
    timeZone: 'Europe/Berlin',
    snapshotCleanup: { auto: false },
    usageLogSources: [],
  });
  return [
    {
      name: 'visibility: a FIFO (never opened)',
      route: visibility,
      rust: false,
      setup: (ccs) => fifo(path.join(ccs, 'account-visibility.json')),
    },
    {
      name: 'preferences: a FIFO primary',
      route: preferences,
      rust: false,
      setup: (ccs) => fifo(path.join(ccs, primary)),
    },
    {
      name: 'preferences: a folder primary',
      route: preferences,
      rust: false,
      setup: (ccs) => fs.mkdirSync(path.join(ccs, primary)),
    },
    {
      name: 'preferences: a FIFO companion',
      route: preferences,
      rust: false,
      setup: (ccs) => {
        write(path.join(ccs, primary), saved);
        fifo(path.join(ccs, companion));
      },
    },
    {
      name: 'preferences: an oversized companion (primary kept)',
      route: preferences,
      rust: true,
      setup: (ccs) => {
        write(path.join(ccs, primary), saved);
        write(
          path.join(ccs, companion),
          JSON.stringify({ usageLogSources: [], pad: 'x'.repeat(70_000) })
        );
      },
    },
    {
      name: 'preferences: a dangling companion symlink (primary kept)',
      route: preferences,
      rust: true,
      setup: (ccs) => {
        write(path.join(ccs, primary), saved);
        fs.symlinkSync(path.join(ccs, 'nowhere.json'), path.join(ccs, companion));
      },
    },
  ];
}
