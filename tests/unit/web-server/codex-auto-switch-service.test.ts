import { afterEach, describe, expect, it, mock } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodexActivationError } from '../../../src/codex-auth/activate-codex-profile';
import type { CodexAuthProfilesSummary } from '../../../src/codex-auth/codex-auth-dashboard-service';
import type { BarSummaryRow } from '../../../src/web-server/routes/bar-routes';
import {
  CodexAutoSwitchService,
  getCodexAutoSwitchService,
  getCodexAutoSwitchAuthSnapshotFromContents,
} from '../../../src/web-server/services/codex-auto-switch-service';
import type { CodexAutoSwitchDeps } from '../../../src/web-server/services/codex-auto-switch-service';
import type { CodexAutoSwitchAuthSnapshot } from '../../../src/web-server/services/codex-auto-switch-service';
import { runWithScopedConfigDir } from '../../../src/utils/config-manager';

const NOW = Date.parse('2026-10-01T12:00:00Z');
const directories: string[] = [];

afterEach(() => {
  for (const directory of directories.splice(0))
    fs.rmSync(directory, { recursive: true, force: true });
});

function temporaryDirectory(): string {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-native-auto-'));
  directories.push(directory);
  return directory;
}

function summary(active: string | null = 'alpha'): CodexAuthProfilesSummary {
  return {
    active: { name: 'gamma', source: 'default', codexHome: '/fake/default' },
    default: 'gamma',
    activated: active
      ? { name: active, email: `${active}@example.test`, plan: 'pro', codexHome: '/fake/.codex' }
      : null,
    profiles: ['alpha', 'beta', 'gamma'].map((name) => ({
      name,
      email: `${name}@example.test`,
      accountId: name,
      plan: 'pro',
      codexHome: `/fake/${name}`,
      lastUsed: null,
      authValid: true,
    })),
  };
}

function row(profile: string, used: number, overrides: Partial<BarSummaryRow> = {}): BarSummaryRow {
  return {
    account_id: `ccsx:${profile}`,
    provider: 'codex',
    profile,
    surface: 'ccsx',
    is_subscription: true,
    displayName: profile,
    tier: 'pro',
    paused: false,
    quota_percentage: 100 - used,
    quotaStatus: 'ok',
    quotaSource: 'network',
    next_reset: null,
    is_default: false,
    last_activity_at: null,
    today_cost: null,
    health: 'ok',
    cached: true,
    fetchedAt: new Date(NOW).toISOString(),
    needsReauth: false,
    quotaWindows: [
      {
        key: 'seven_day',
        label: 'week',
        usedPercent: used,
        remainingPercent: 100 - used,
        resetAt: null,
        windowMinutes: 10080,
      },
    ],
    ...overrides,
  };
}

function harness(overrides: CodexAutoSwitchDeps = {}) {
  let enabled = true;
  const getSummary = mock(async () => summary());
  const getRows = mock(async (_names: string[]) => [
    row('alpha', 95),
    row('beta', 20),
    row('gamma', 50),
  ]);
  const activate = mock(async (_name: string) => undefined);
  const service = new CodexAutoSwitchService({
    ccsDir: '/fake/native-auto',
    readConfig: () => ({ enabled }),
    writeConfig: (config) => {
      enabled = config.enabled;
    },
    getSummary,
    getRows,
    activate,
    getAuthSnapshot: () => authSnapshot(),
    now: () => NOW,
    ...overrides,
  });
  return {
    service,
    getSummary,
    getRows,
    activate,
    disable: () => {
      enabled = false;
    },
  };
}

function authSnapshot(): CodexAutoSwitchAuthSnapshot {
  return {
    live: {
      fingerprint: 'private-live-fingerprint',
      email: 'alpha@example.test',
      accountId: 'alpha',
    },
    profiles: {
      alpha: 'private-alpha-fingerprint',
      beta: 'private-beta-fingerprint',
      gamma: 'private-gamma-fingerprint',
    },
  };
}

function authContent(headerWorkspace: unknown, claimWorkspace: unknown = 'alpha'): Buffer {
  const header = Buffer.from(JSON.stringify({ alg: 'none' })).toString('base64url');
  const payload = Buffer.from(
    JSON.stringify({
      email: 'alpha@example.test',
      'https://api.openai.com/auth': { chatgpt_account_id: claimWorkspace },
    })
  ).toString('base64url');
  return Buffer.from(
    JSON.stringify({
      tokens: {
        id_token: `${header}.${payload}.c2ln`,
        account_id: headerWorkspace,
        access_token: 'PRIVATE-TEST-ACCESS-TOKEN',
        refresh_token: 'PRIVATE-TEST-REFRESH-TOKEN',
      },
    })
  );
}

describe('native Codex automatic switching', () => {
  it('retains a private auth snapshot only when quota header and saved JWT workspace agree', () => {
    const value = getCodexAutoSwitchAuthSnapshotFromContents(authContent('alpha'), {
      alpha: authContent('alpha'),
    });
    expect(value.live).toMatchObject({ email: 'alpha@example.test', accountId: 'alpha' });
    expect(value.live?.fingerprint).toMatch(/^[a-f0-9]{64}$/);
    expect(value.profiles.alpha).toBe(value.live?.fingerprint);
    expect(JSON.stringify(value)).not.toContain('PRIVATE-TEST');
  });

  it.each([
    ['different workspace', 'beta', 'alpha'],
    ['missing nested workspace', undefined, 'alpha'],
    ['empty nested workspace', '', 'alpha'],
    ['nonstring nested workspace', 42, 'alpha'],
    ['missing JWT workspace', 'alpha', undefined],
  ])('rejects %s in both live and saved auth snapshots', (_label, nested, claim) => {
    const content = authContent(nested, claim === undefined ? null : claim);
    const value = getCodexAutoSwitchAuthSnapshotFromContents(content, { alpha: content });
    expect(value.live).toBeNull();
    expect(value.profiles.alpha).toBeNull();
    expect(JSON.stringify(value)).not.toContain('PRIVATE-TEST');
  });

  it('does not activate a candidate with an unusable private auth fingerprint', async () => {
    const value = authSnapshot();
    value.profiles.beta = null;
    const h = harness({ getAuthSnapshot: () => value });
    await h.service.runCycle();
    expect(h.activate).not.toHaveBeenCalled();
    expect(h.service.getStatus().outcome).toBe('scheduled');
  });
  it('defaults disabled without creating config or consulting authentication', async () => {
    const ccsDir = temporaryDirectory();
    const getSummary = mock(async () => summary());
    const service = new CodexAutoSwitchService({ ccsDir, getSummary });
    await service.runCycle();
    expect(service.getStatus()).toMatchObject({ enabled: false, outcome: 'disabled' });
    expect(getSummary).not.toHaveBeenCalled();
    expect(fs.existsSync(path.join(ccsDir, 'codex-auto-switch.json'))).toBe(false);
  });

  it('does no quota or credential work while disabled', async () => {
    const h = harness({ readConfig: () => ({ enabled: false }) });
    await h.service.runCycle();
    expect(h.getSummary).not.toHaveBeenCalled();
    expect(h.getRows).not.toHaveBeenCalled();
    expect(h.activate).not.toHaveBeenCalled();
  });

  it('uses the actual activated account, chooses greatest minimum remaining, and accepts weekly-only limits', async () => {
    const h = harness();
    await h.service.runCycle();
    expect(h.getRows).toHaveBeenCalledWith(['alpha', 'beta', 'gamma']);
    expect(h.activate).toHaveBeenCalledWith('beta');
    expect(h.getSummary).toHaveBeenCalledTimes(2);
    expect(h.service.getStatus()).toMatchObject({
      outcome: 'switched',
      lastSwitchedAt: new Date(NOW).toISOString(),
    });
  });

  it('keeps a healthy active account unchanged', async () => {
    const h = harness({ getRows: async () => [row('alpha', 94.99), row('beta', 0)] });
    await h.service.runCycle();
    expect(h.activate).not.toHaveBeenCalled();
    expect(h.service.getStatus().outcome).toBe('healthy');
  });

  it('scores all reported core limits and breaks ties deterministically', async () => {
    const limited = row('gamma', 0);
    limited.quotaWindows?.push({
      key: 'five_hour',
      label: '5h',
      usedPercent: 20,
      remainingPercent: 80,
      resetAt: null,
      windowMinutes: 300,
    });
    const h = harness({ getRows: async () => [limited, row('beta', 20), row('alpha', 96)] });
    await h.service.runCycle();
    expect(h.activate).toHaveBeenCalledWith('beta');
  });

  it('waits when the actual shared login is unknown, even when CCS has a default', async () => {
    const h = harness({ getSummary: async () => summary(null) });
    await h.service.runCycle();
    expect(h.getRows).not.toHaveBeenCalled();
    expect(h.activate).not.toHaveBeenCalled();
    expect(h.service.getStatus().outcome).toBe('no_quota');
  });

  const unusableRows: Array<[string, Partial<BarSummaryRow>]> = [
    ['local fallback', { quotaSource: 'local' }],
    ['missing provenance', { quotaSource: undefined }],
    ['Claude provider', { provider: 'claude-code' }],
    ['Claude surface', { surface: 'ccs' }],
    ['CLIProxy pool', { is_subscription: undefined }],
    ['unknown profile', { profile: 'elsewhere' }],
    ['failed provider', { quotaStatus: 'error' }],
    ['expired login', { needsReauth: true }],
    ['stale network', { fetchedAt: new Date(NOW - 630001).toISOString() }],
    ['future network', { fetchedAt: new Date(NOW + 30001).toISOString() }],
    ['invalid timestamp', { fetchedAt: 'invalid' }],
    ['missing windows', { quotaWindows: [] }],
    [
      'unrecognized window',
      {
        quotaWindows: [
          {
            key: 'unknown',
            label: '?',
            usedPercent: 100,
            remainingPercent: 0,
            resetAt: null,
            windowMinutes: null,
          },
        ],
      },
    ],
    [
      'nonfinite usage',
      {
        quotaWindows: [
          {
            key: 'seven_day',
            label: 'week',
            usedPercent: NaN,
            remainingPercent: 0,
            resetAt: null,
            windowMinutes: 10080,
          },
        ],
      },
    ],
    [
      'negative usage',
      {
        quotaWindows: [
          {
            key: 'seven_day',
            label: 'week',
            usedPercent: -1,
            remainingPercent: 101,
            resetAt: null,
            windowMinutes: 10080,
          },
        ],
      },
    ],
    [
      'crossed reset',
      {
        fetchedAt: new Date(NOW - 1000).toISOString(),
        quotaWindows: [
          {
            key: 'seven_day',
            label: 'week',
            usedPercent: 95,
            remainingPercent: 5,
            resetAt: new Date(NOW - 500).toISOString(),
            windowMinutes: 10080,
          },
        ],
      },
    ],
  ];
  it.each(unusableRows)('does not switch on %s active usage', async (_label, overrides) => {
    const h = harness({ getRows: async () => [row('alpha', 100, overrides), row('beta', 0)] });
    await h.service.runCycle();
    expect(h.activate).not.toHaveBeenCalled();
    expect(h.service.getStatus().outcome).toBe('no_quota');
  });

  it.each(unusableRows)('does not choose a candidate with %s usage', async (_label, overrides) => {
    const h = harness({ getRows: async () => [row('alpha', 100), row('beta', 0, overrides)] });
    await h.service.runCycle();
    expect(h.activate).not.toHaveBeenCalled();
    expect(h.service.getStatus().outcome).toBe('no_candidate');
  });

  it('does not choose unauthenticated or equally exhausted profiles', async () => {
    const value = summary();
    value.profiles[1].authValid = false;
    const getRows = mock(async () => [row('alpha', 100), row('beta', 0), row('gamma', 95)]);
    const h = harness({
      getSummary: async () => value,
      getRows,
    });
    await h.service.runCycle();
    expect(getRows).toHaveBeenCalledWith(['alpha', 'gamma']);
    expect(h.activate).not.toHaveBeenCalled();
    expect(h.service.getStatus().outcome).toBe('no_candidate');
  });

  it('fails closed when same-email live login belongs to a different saved workspace', async () => {
    const value = authSnapshot();
    value.live!.accountId = 'different-workspace';
    const h = harness({ getAuthSnapshot: () => value });
    await h.service.runCycle();
    expect(h.getRows).not.toHaveBeenCalled();
    expect(h.activate).not.toHaveBeenCalled();
    expect(h.service.getStatus().outcome).toBe('no_quota');
  });

  it.each(['live', 'alpha', 'beta'] as const)(
    'does not activate if private %s authentication changes during quota polling',
    async (changed) => {
      let calls = 0;
      const h = harness({
        getAuthSnapshot: () => {
          const value = authSnapshot();
          if (++calls === 2) {
            if (changed === 'live') value.live!.fingerprint = 'replaced-private-live';
            else value.profiles[changed] = 'replaced-private-profile';
          }
          return value;
        },
      });
      await h.service.runCycle();
      expect(h.activate).not.toHaveBeenCalled();
      expect(h.service.getStatus().outcome).toBe('scheduled');
      expect(JSON.stringify(h.service.getStatus())).not.toContain('private');
    }
  );

  it('uses busy errors as pending status and retries without disabling or bypassing activation', async () => {
    let attempts = 0;
    const h = harness({
      activate: async () => {
        if (attempts++ === 0) throw new CodexActivationError('busy', 'PRIVATE PROCESS DETAIL');
      },
    });
    await h.service.runCycle();
    expect(h.service.getStatus()).toMatchObject({
      enabled: true,
      outcome: 'waiting_idle',
      activationInProgress: false,
    });
    expect(JSON.stringify(h.service.getStatus())).not.toContain('PRIVATE PROCESS DETAIL');
    await h.service.runCycle();
    expect(attempts).toBe(2);
    expect(h.service.getStatus().outcome).toBe('switched');
  });

  it('sanitizes unexpected activation failures and applies retry backoff', async () => {
    let now = NOW;
    const activate = mock(async () => {
      throw new Error('SECRET TOKEN AND EMAIL');
    });
    const h = harness({ now: () => now, activate });
    await h.service.runCycle();
    await h.service.runCycle();
    expect(activate).toHaveBeenCalledTimes(1);
    expect(h.service.getStatus().outcome).toBe('error');
    expect(JSON.stringify(h.service.getStatus())).not.toContain('SECRET TOKEN');
    now += 300000;
    await h.service.runCycle();
    expect(activate).toHaveBeenCalledTimes(2);
  });

  it('coalesces overlapping cycles through quota fetch and activation', async () => {
    let release!: (rows: BarSummaryRow[]) => void;
    const pending = new Promise<BarSummaryRow[]>((resolve) => {
      release = resolve;
    });
    const getRows = mock(async () => pending);
    const h = harness({ getRows });
    const first = h.service.runCycle();
    const second = h.service.runCycle();
    expect(first).toBe(second);
    await Promise.resolve();
    release([row('alpha', 100), row('beta', 0)]);
    await Promise.all([first, second]);
    expect(getRows).toHaveBeenCalledTimes(1);
    expect(h.activate).toHaveBeenCalledTimes(1);
  });

  it('enabling config never activates inside the settings request', () => {
    const h = harness({ readConfig: () => ({ enabled: false }) });
    h.service.setEnabled(true);
    expect(h.activate).not.toHaveBeenCalled();
    expect(h.getSummary).not.toHaveBeenCalled();
  });

  it('disabling while a quota decision is queued prevents activation', async () => {
    let release!: (rows: BarSummaryRow[]) => void;
    const pending = new Promise<BarSummaryRow[]>((resolve) => {
      release = resolve;
    });
    const h = harness({ getRows: async () => pending });
    const cycle = h.service.runCycle();
    await Promise.resolve();
    h.service.setEnabled(false);
    release([row('alpha', 100), row('beta', 0)]);
    await cycle;
    expect(h.activate).not.toHaveBeenCalled();
    expect(h.service.getStatus().outcome).toBe('disabled');
  });

  it('rereads config immediately before activation to respect external disabling', async () => {
    let reads = 0;
    const h = harness({ readConfig: () => ({ enabled: ++reads === 1 }) });
    await h.service.runCycle();
    expect(reads).toBe(2);
    expect(h.activate).not.toHaveBeenCalled();
  });

  it('stop cancels a queued decision without interrupting existing account work', async () => {
    let calls = 0;
    const h = harness({
      getSummary: async () => {
        if (++calls === 2) h.service.stop();
        return summary();
      },
    });
    await h.service.runCycle();
    expect(h.activate).not.toHaveBeenCalled();
  });

  it.each(['active', 'target'] as const)(
    'does not activate when %s identity changes during a check',
    async (changed) => {
      let calls = 0;
      const h = harness({
        getSummary: async () => {
          const value = summary();
          if (++calls === 2) {
            if (changed === 'active') value.activated!.name = 'gamma';
            else value.profiles[1].accountId = 'replaced-account';
          }
          return value;
        },
      });
      await h.service.runCycle();
      expect(h.activate).not.toHaveBeenCalled();
      expect(h.service.getStatus().outcome).toBe('scheduled');
    }
  );

  it('reports disabling an already started activation without claiming to interrupt it', async () => {
    let release!: () => void;
    let started!: () => void;
    const start = new Promise<void>((resolve) => {
      started = resolve;
    });
    const pending = new Promise<void>((resolve) => {
      release = resolve;
    });
    const h = harness({
      activate: async () => {
        started();
        return pending;
      },
    });
    const cycle = h.service.runCycle();
    await start;
    h.service.setEnabled(false);
    expect(h.service.getStatus()).toMatchObject({
      enabled: false,
      outcome: 'switching',
      activationInProgress: true,
    });
    expect(h.service.getStatus().message).toContain('cannot interrupt');
    release();
    await cycle;
    expect(h.service.getStatus()).toMatchObject({
      enabled: false,
      outcome: 'disabled',
      activationInProgress: false,
    });
  });

  it('writes only fixed private config and isolates monitor state by CCS scope', async () => {
    const firstDir = temporaryDirectory();
    const secondDir = temporaryDirectory();
    const first = await runWithScopedConfigDir(firstDir, () => getCodexAutoSwitchService());
    const second = await runWithScopedConfigDir(secondDir, () => getCodexAutoSwitchService());
    expect(first).not.toBe(second);
    first.setEnabled(true);
    expect(first.getStatus().enabled).toBe(true);
    expect(second.getStatus().enabled).toBe(false);
    const file = path.join(firstDir, 'codex-auto-switch.json');
    expect(fs.statSync(file).mode & 0o777).toBe(0o600);
    expect(JSON.parse(fs.readFileSync(file, 'utf8'))).toEqual({
      version: 1,
      enabled: true,
      thresholdPercent: 5,
      pollIntervalSeconds: 60,
    });
    expect(fs.existsSync(path.join(secondDir, 'codex-auto-switch.json'))).toBe(false);
  });

  it('fails closed on malformed persisted config without leaking content', async () => {
    const ccsDir = temporaryDirectory();
    fs.writeFileSync(path.join(ccsDir, 'codex-auto-switch.json'), 'PRIVATE SECRET JSON FRAGMENT');
    const activate = mock(async () => undefined);
    const service = new CodexAutoSwitchService({ ccsDir, activate, now: () => NOW });
    await service.runCycle();
    expect(service.getStatus()).toMatchObject({ enabled: false, outcome: 'error' });
    expect(JSON.stringify(service.getStatus())).not.toContain('PRIVATE SECRET');
    expect(activate).not.toHaveBeenCalled();
  });
});
