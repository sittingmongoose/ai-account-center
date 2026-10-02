import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as childProcess from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as claudeProfiles from '../../../src/web-server/services/claude-desktop-profile-service';
import {
  applyClaudeLiveUsage,
  claudeAccount,
} from '../../../src/web-server/services/account-dashboard-projection';
import {
  ClaudeDesktopLiveUsageError,
  getCachedClaudeDesktopLiveUsage,
  getLiveClaudeDesktopUsage,
  invalidateClaudeDesktopLiveUsageCache,
} from '../../../src/web-server/services/claude-desktop-live-service';

const profile = {
  id: 'gmail',
  email: 'fixture@example.com',
  mac: { launcherName: 'Fixture Mac', sshHost: 'fixture-mac', profilePath: '/fixture/claude' },
  windows: {
    launcherName: 'Fixture Windows',
    sshHost: 'fixture-windows',
    profilePath: 'C:\\fixture\\claude',
  },
};

function payload(overrides: Record<string, unknown> = {}): string {
  return JSON.stringify({
    schemaVersion: 1,
    provider: 'claude',
    profileId: 'gmail',
    email: profile.email,
    platform: 'windows',
    status: 'ok',
    plan: 'max',
    accountVerified: true,
    organizationVerified: true,
    sourceContextFingerprint: 'a'.repeat(64),
    fetchedAt: new Date(Date.now()).toISOString(),
    windows: [
      { key: 'five_hour', usedPercent: 0, resetAt: null },
      { key: 'seven_day', usedPercent: 100, resetAt: '2026-10-02T02:59:59.716415Z' },
    ],
    private: 'credential-sentinel',
    ...overrides,
  });
}

async function waitForReadStarted(started: Promise<void>): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      started,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error('Expected helper read did not start.')), 2000);
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

describe('identity-bound Claude Desktop live usage', () => {
  let root: string;
  let previousCcsDir: string | undefined;
  let exec: ReturnType<typeof spyOn<typeof childProcess, 'execFile'>>;
  let output: string;

  function writeProfiles(profiles: unknown[] = [profile]): void {
    fs.mkdirSync(process.env.CCS_DIR!, { recursive: true });
    fs.writeFileSync(
      path.join(process.env.CCS_DIR!, 'claude-desktop-profiles.json'),
      JSON.stringify({ version: 1, profiles })
    );
  }

  function holdHelperReads(): { started: Promise<void>; release: () => void } {
    let notifyStarted: (() => void) | undefined;
    const started = new Promise<void>((resolve) => {
      notifyStarted = resolve;
    });
    const callbacks: Array<(error: null, stdout: string) => void> = [];
    let releasing = false;
    exec.mockImplementation((...args: unknown[]) => {
      const callback = args.at(-1) as (error: null, stdout: string) => void;
      if (releasing) callback(null, output);
      else callbacks.push(callback);
      notifyStarted?.();
      return {} as childProcess.ChildProcess;
    });
    return {
      started,
      release: () => {
        releasing = true;
        for (const callback of callbacks.splice(0)) callback(null, output);
      },
    };
  }

  beforeEach(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-claude-live-'));
    previousCcsDir = process.env.CCS_DIR;
    process.env.CCS_DIR = path.join(root, 'first');
    writeProfiles();
    invalidateClaudeDesktopLiveUsageCache();
    output = payload();
    exec = spyOn(childProcess, 'execFile').mockImplementation((...args: unknown[]) => {
      const callback = args.at(-1) as (
        error: childProcess.ExecFileException | null,
        stdout: string,
        stderr: string
      ) => void;
      callback(null, output, 'PRIVATE_SSH_STDERR');
      return {} as childProcess.ChildProcess;
    });
  });

  afterEach(() => {
    invalidateClaudeDesktopLiveUsageCache();
    mock.restore();
    if (previousCcsDir === undefined) delete process.env.CCS_DIR;
    else process.env.CCS_DIR = previousCcsDir;
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('returns live Windows quota for the verified account, preserving zero and actual reset', async () => {
    const result = await getLiveClaudeDesktopUsage('gmail');
    expect(result).toMatchObject({
      profileId: 'gmail',
      email: profile.email,
      platform: 'windows',
      source: 'Claude Desktop live quota on Windows',
      plan: 'max',
      windows: [
        { key: 'five_hour', usedPercent: 0, remainingPercent: 100, resetAt: null },
        {
          key: 'seven_day',
          usedPercent: 100,
          remainingPercent: 0,
          resetAt: '2026-10-02T02:59:59.716Z',
        },
      ],
    });
    expect(JSON.stringify(result)).not.toContain('credential-sentinel');
    expect(JSON.stringify(result)).not.toContain('fixture-windows');
  });

  it('uses only the fixed Windows helper and enumerated profile on the manifest SSH alias', async () => {
    await getLiveClaudeDesktopUsage('gmail');
    const [binary, args, options] = exec.mock.calls[0]!;
    expect(binary).toBe('ssh');
    expect(args).toContain('BatchMode=yes');
    expect(args).toContain('--');
    expect(args).toContain('fixture-windows');
    const command = (args as string[]).at(-1)!;
    expect(command).toStartWith('powershell.exe -NoProfile -NonInteractive -EncodedCommand ');
    const script = Buffer.from(command.split(' ').at(-1)!, 'base64').toString('utf16le');
    expect(script).toContain(
      "[IO.Path]::Combine($HOME, '.ccs', 'account-usage', 'claude_usage.py')"
    );
    expect(script).toContain("--provider 'claude' --profile 'gmail' --platform 'windows'");
    expect(script).toContain("--expected-email 'fixture@example.com'");
    expect(script).toContain("--profile-dir 'C:\\fixture\\claude'");
    expect(script).toContain("'.ccs', 'claude-session-migration', 'venv', 'Scripts', 'python.exe'");
    expect(script).toContain('Test-Path -LiteralPath $venv -PathType Leaf');
    expect(script).toContain("else { 'python.exe' }");
    expect(script).toContain('& $python $helper');
    expect(script).not.toContain('Invoke-Expression');
    expect(options).toMatchObject({ timeout: 20000, maxBuffer: 65536, windowsHide: true });
  });

  it('preserves genuine Fable zero and reset alongside core quota and all existing balances across a cold cache', async () => {
    output = payload({
      windows: [
        { key: 'five_hour', usedPercent: 11, resetAt: '2026-10-01T21:20:00.499Z' },
        { key: 'seven_day', usedPercent: 9, resetAt: '2026-10-08T12:00:00.499Z' },
        {
          key: 'seven_day_fable',
          usedPercent: 0,
          resetAt: '2026-10-08T12:00:00Z',
          label: 'untrusted upstream text',
        },
        {
          key: 'extra_usage',
          enabled: false,
          usedPercent: 0,
          used: 0,
          limit: 50,
          remaining: 50,
          unit: 'USD',
        },
        { key: 'reset_credits_available', kind: 'balance', remaining: 0, unit: 'resets' },
        {
          key: 'reset_credit_used_grant_1',
          kind: 'balance',
          remaining: 0,
          used: 1,
          limit: 1,
          unit: 'resets',
          expiresAt: '2026-10-22T16:00:00Z',
        },
        { key: 'prepaid_balance', kind: 'balance', remaining: 0, unit: 'USD' },
      ],
    });
    const live = await getLiveClaudeDesktopUsage('gmail');
    expect(live?.windows).toHaveLength(7);
    expect(live?.windows[2]).toMatchObject({
      key: 'seven_day_fable',
      label: 'Weekly Fable usage',
      kind: 'rate_limit',
      usedPercent: 0,
      remainingPercent: 100,
      resetAt: '2026-10-08T12:00:00.000Z',
      windowMinutes: 10080,
    });
    expect(live?.windows[2]).not.toHaveProperty('enabled');
    expect(live?.windows[5]?.expiresAt).toBe('2026-10-22T16:00:00.000Z');
    expect(live?.windows[6]?.remaining).toBe(0);
    invalidateClaudeDesktopLiveUsageCache();
    const retained = await getCachedClaudeDesktopLiveUsage('gmail');
    expect(retained).toEqual(live);
    expect(exec).toHaveBeenCalledTimes(1);
    expect(JSON.stringify(retained)).not.toContain('untrusted upstream text');
  });

  it.each([
    { schemaVersion: 2 },
    { provider: 'codex' },
    { profileId: 'platyr' },
    { email: 'someone-else@example.com' },
    { platform: 'mac' },
    { status: 'cached' },
    { accountVerified: false },
    { organizationVerified: false },
    { fetchedAt: '2020-01-01T00:00:00Z' },
    { fetchedAt: '2026-10-01' },
    { windows: [] },
    { windows: [{ key: 'unknown-private-field', usedPercent: 20 }] },
    { windows: [{ key: 'five_hour', usedPercent: -1 }] },
    { windows: [{ key: 'five_hour', usedPercent: true }] },
    { windows: [{ key: 'five_hour', usedPercent: 0, resetAt: 'private-reset-sentinel' }] },
    { windows: [{ key: 'five_hour', usedPercent: 0, expiresAt: 'private-expiry-sentinel' }] },
    {
      windows: [
        {
          key: 'prepaid_balance',
          kind: 'balance',
          remaining: 5,
          unit: 'USD',
          status: 'cached',
          sampledAt: '2026-10-01T12:00:00Z',
        },
      ],
    },
    {
      windows: [
        { key: 'five_hour', usedPercent: 0 },
        { key: 'five_hour', usedPercent: 10 },
      ],
    },
  ])('rejects mismatched or untrustworthy helper results (%j)', async (overrides) => {
    output = payload(overrides);
    expect(await getLiveClaudeDesktopUsage('gmail')).toBeNull();
  });

  it.each(['{broken', ' '.repeat(65537)])(
    'rejects malformed or oversized output',
    async (value) => {
      output = value;
      expect(await getLiveClaudeDesktopUsage('gmail')).toBeNull();
    }
  );

  it('preserves disabled extra usage and unknown budget/reset instead of fabricating zero', async () => {
    output = payload({
      windows: [{ key: 'extra_usage', enabled: false, usedPercent: null, resetAt: null }],
    });
    expect((await getLiveClaudeDesktopUsage('gmail'))?.windows).toEqual([
      {
        key: 'extra_usage',
        label: 'Extra usage',
        kind: 'extra_usage',
        usedPercent: null,
        remainingPercent: null,
        resetAt: null,
        expiresAt: null,
        windowMinutes: null,
        used: null,
        limit: null,
        remaining: null,
        unit: null,
        enabled: false,
      },
    ]);
  });

  it('whitelists balances, actual expiry, and currency for extra usage', async () => {
    output = payload({
      windows: [
        {
          key: 'extra_usage',
          enabled: true,
          unlimited: false,
          usedPercent: 25,
          used: 5,
          limit: 20,
          remaining: 15,
          unit: 'USD',
          resetAt: '2026-10-02T01:00:00Z',
          expiresAt: '2026-11-01T01:00:00Z',
          token: 'extra-private-sentinel',
        },
      ],
    });
    const result = await getLiveClaudeDesktopUsage('gmail');
    expect(result?.windows[0]).toMatchObject({
      kind: 'extra_usage',
      used: 5,
      limit: 20,
      remaining: 15,
      unit: 'USD',
      enabled: true,
      unlimited: false,
      expiresAt: '2026-11-01T01:00:00.000Z',
    });
    expect(JSON.stringify(result)).not.toContain('extra-private-sentinel');
  });

  it('keeps true zero available resets separate from the used grant expiry and prepaid balance', async () => {
    output = payload({
      windows: [
        { key: 'five_hour', usedPercent: 7 },
        {
          key: 'reset_credits_available',
          kind: 'balance',
          remaining: 0,
          unit: 'resets',
          enabled: true,
        },
        {
          key: 'reset_credit_used_grant_1',
          kind: 'balance',
          remaining: 0,
          used: 1,
          limit: 1,
          unit: 'resets',
          enabled: false,
          expiresAt: '2026-10-22T16:00:00Z',
          label: 'private-label-sentinel',
          cookie: 'private-cookie-sentinel',
        },
        { key: 'prepaid_balance', kind: 'balance', remaining: 0, unit: 'USD', expiresAt: null },
      ],
    });
    const result = await getLiveClaudeDesktopUsage('gmail');
    expect(result?.windows).toMatchObject([
      { key: 'five_hour', usedPercent: 7 },
      {
        key: 'reset_credits_available',
        label: 'Rate-limit resets available',
        remaining: 0,
        expiresAt: null,
      },
      {
        key: 'reset_credit_used_grant_1',
        label: 'Used rate-limit reset grant 1',
        remaining: 0,
        used: 1,
        expiresAt: '2026-10-22T16:00:00.000Z',
        enabled: false,
      },
      {
        key: 'prepaid_balance',
        label: 'Prepaid balance',
        remaining: 0,
        unit: 'USD',
        expiresAt: null,
      },
    ]);
    expect(JSON.stringify(result)).not.toContain('private-');
  });

  it('preserves actual available grant expiry and raw credits when currency is absent', async () => {
    output = payload({
      windows: [
        { key: 'reset_credits_available', kind: 'balance', remaining: 2, unit: 'resets' },
        {
          key: 'reset_credit_available_grant_1',
          kind: 'balance',
          remaining: 2,
          used: 1,
          limit: 3,
          unit: 'resets',
          enabled: true,
          expiresAt: '2026-10-22T16:00:00Z',
        },
        { key: 'prepaid_balance', kind: 'balance', remaining: 0, unit: 'credits' },
      ],
    });
    const result = await getLiveClaudeDesktopUsage('gmail');
    expect(result?.windows[1]).toMatchObject({
      label: 'Available rate-limit reset grant 1',
      remaining: 2,
      expiresAt: '2026-10-22T16:00:00.000Z',
    });
    expect(result?.windows[2]).toMatchObject({
      label: 'Prepaid credit balance',
      unit: 'credits',
      remaining: 0,
    });
  });

  it.each([
    {
      key: 'reset_credits_available',
      kind: 'balance',
      remaining: 0,
      unit: 'resets',
      expiresAt: '2026-10-22T16:00:00Z',
    },
    { key: 'reset_credits_available', kind: 'balance', remaining: 0.5, unit: 'resets' },
    { key: 'reset_credit_used_grant_21', kind: 'balance', remaining: 0, unit: 'resets' },
    { key: 'reset_credit_used_grant_1', kind: 'balance', remaining: -1, unit: 'resets' },
    { key: 'prepaid_balance', kind: 'balance', remaining: 0, unit: 'private-currency-sentinel' },
    { key: 'prepaid_balance', kind: 'balance', remaining: true, unit: 'USD' },
    { key: 'prepaid_balance', kind: 'balance', remaining: 0, unit: 'USD', usedPercent: 0 },
    {
      key: 'prepaid_balance',
      kind: 'balance',
      remaining: 0,
      unit: 'USD',
      resetAt: '2026-10-22T16:00:00Z',
    },
  ])('rejects untrustworthy optional balance metadata (%j)', async (window) => {
    output = payload({ windows: [window] });
    expect(await getLiveClaudeDesktopUsage('gmail')).toBeNull();
  });

  it('keeps native quota when no optional web balances were retrieved', async () => {
    const result = await getLiveClaudeDesktopUsage('gmail');
    expect(result?.windows.map((window) => window.key)).toEqual(['five_hour', 'seven_day']);
    expect(result?.windows.some((window) => window.key === 'prepaid_balance')).toBe(false);
  });

  it('preserves mixed over-limit quota, actual resets, and extra balances in one verified sample', async () => {
    output = payload({
      windows: [
        { key: 'five_hour', usedPercent: 125.5, resetAt: '2026-10-02T01:00:00Z' },
        { key: 'seven_day', usedPercent: 8, resetAt: '2026-10-06T20:00:00Z' },
        {
          key: 'extra_usage',
          enabled: true,
          usedPercent: 112.25,
          used: 22.45,
          limit: 20,
          remaining: 0,
          unit: 'USD',
          resetAt: null,
          expiresAt: '2026-10-22T16:00:00Z',
        },
        { key: 'reset_credits_available', kind: 'balance', remaining: 0, unit: 'resets' },
        { key: 'prepaid_balance', kind: 'balance', remaining: 5, unit: 'USD' },
      ],
    });
    const result = await getLiveClaudeDesktopUsage('gmail');
    expect(result?.windows).toMatchObject([
      {
        key: 'five_hour',
        usedPercent: 125.5,
        remainingPercent: 0,
        resetAt: '2026-10-02T01:00:00.000Z',
      },
      {
        key: 'seven_day',
        usedPercent: 8,
        remainingPercent: 92,
        resetAt: '2026-10-06T20:00:00.000Z',
      },
      {
        key: 'extra_usage',
        usedPercent: 112.25,
        remainingPercent: 0,
        used: 22.45,
        limit: 20,
        remaining: 0,
        expiresAt: '2026-10-22T16:00:00.000Z',
      },
      { key: 'reset_credits_available', remaining: 0 },
      { key: 'prepaid_balance', remaining: 5, unit: 'USD' },
    ]);
    expect(result?.windows).toHaveLength(5);
  });

  it('never executes for unknown profiles or unconfigured Windows sources', async () => {
    expect(await getLiveClaudeDesktopUsage('gmail; bad')).toBeNull();
    writeProfiles([{ ...profile, windows: undefined }]);
    expect(await getLiveClaudeDesktopUsage('gmail')).toBeNull();
    expect(exec).not.toHaveBeenCalled();
  });

  it('accepts a new manifest ID live and cached while an absent ID is refused', async () => {
    writeProfiles([{ ...profile, id: 'added-profile', email: 'added@example.com' }]);
    output = payload({ profileId: 'added-profile', email: 'added@example.com' });
    const live = await getLiveClaudeDesktopUsage('added-profile');
    expect(live?.profileId).toBe('added-profile');
    expect(live?.email).toBe('added@example.com');
    invalidateClaudeDesktopLiveUsageCache();
    expect(await getCachedClaudeDesktopLiveUsage('added-profile')).toEqual(live);
    expect(await getLiveClaudeDesktopUsage('gmail')).toBeNull();
    expect(await getCachedClaudeDesktopLiveUsage('gmail')).toBeNull();
    expect(exec).toHaveBeenCalledTimes(1);
  });

  function remoteScript(call: readonly unknown[]): string {
    const command = (call[1] as string[]).at(-1)!;
    return Buffer.from(command.split(' ').at(-1)!, 'base64').toString('utf16le');
  }

  function argparseFailure(stderr: string): Error {
    return Object.assign(new Error('Command failed with exit code 2'), { code: 2, stderr });
  }

  it('retries an old installed collector once with the old argument set', async () => {
    exec.mockImplementation((...args: unknown[]) => {
      const callback = args.at(-1) as (
        error: Error | null,
        stdout: string,
        stderr: string
      ) => void;
      if (exec.mock.calls.length === 1) {
        callback(
          argparseFailure(
            "claude_usage.py: error: unrecognized arguments: --expected-email 'fixture@example.com'"
          ),
          '',
          "claude_usage.py: error: unrecognized arguments: --expected-email 'fixture@example.com'"
        );
      } else {
        callback(null, output, '');
      }
      return {} as childProcess.ChildProcess;
    });
    const result = await getLiveClaudeDesktopUsage('gmail');
    expect(result?.profileId).toBe('gmail');
    expect(result?.windows[0].usedPercent).toBe(0);
    expect(exec).toHaveBeenCalledTimes(2);
    expect(remoteScript(exec.mock.calls[0]!)).toContain('--expected-email');
    const retry = remoteScript(exec.mock.calls[1]!);
    expect(retry).toContain("--profile 'gmail' --platform 'windows'");
    expect(retry).not.toContain('--expected-email');
    expect(retry).not.toContain('--profile-dir');
  });

  it('reports an outdated collector for a new ID the old copy cannot know', async () => {
    writeProfiles([{ ...profile, id: 'added-profile', email: 'added@example.com' }]);
    exec.mockImplementation((...args: unknown[]) => {
      const callback = args.at(-1) as (
        error: Error | null,
        stdout: string,
        stderr: string
      ) => void;
      const stderr = remoteScript(args).includes('--expected-email')
        ? 'claude_usage.py: error: unrecognized arguments: --expected-email'
        : "claude_usage.py: error: argument --profile: invalid choice: 'added-profile'";
      callback(argparseFailure(stderr), '', stderr);
      return {} as childProcess.ChildProcess;
    });
    const failure = await getLiveClaudeDesktopUsage('added-profile').then(
      () => null,
      (error: unknown) => error
    );
    expect(failure).toBeInstanceOf(ClaudeDesktopLiveUsageError);
    expect((failure as ClaudeDesktopLiveUsageError).helperOutdated).toBe(true);
    expect(exec).toHaveBeenCalledTimes(2);
  });

  it('does not retry when exit 2 carries no argparse rejection', async () => {
    exec.mockImplementation((...args: unknown[]) => {
      const callback = args.at(-1) as (
        error: Error | null,
        stdout: string,
        stderr: string
      ) => void;
      const stderr = "/usr/bin/python3: can't open file: [Errno 2] No such file";
      callback(argparseFailure(stderr), '', stderr);
      return {} as childProcess.ChildProcess;
    });
    expect(await getLiveClaudeDesktopUsage('gmail')).toBeNull();
    expect(exec).toHaveBeenCalledTimes(1);
  });

  it('never executes for a malformed manifest and does not serve stale identity', async () => {
    await getLiveClaudeDesktopUsage('gmail');
    fs.writeFileSync(path.join(process.env.CCS_DIR!, 'claude-desktop-profiles.json'), '{broken');
    expect(await getLiveClaudeDesktopUsage('gmail')).toBeNull();
    expect(exec).toHaveBeenCalledTimes(1);
  });

  it('coalesces simultaneous requests, including manual refresh', async () => {
    // Both callers share an already-resolved manifest read so this fixture
    // holds the helper boundary, rather than racing independent filesystem I/O.
    spyOn(claudeProfiles, 'listClaudeDesktopProfiles').mockResolvedValue([profile]);
    const held = holdHelperReads();
    const first = getLiveClaudeDesktopUsage('gmail');
    const second = getLiveClaudeDesktopUsage('gmail', { refresh: true });
    try {
      await waitForReadStarted(held.started);
      expect(exec).toHaveBeenCalledTimes(1);
      held.release();
      expect(await first).toBe(await second);
    } finally {
      held.release();
      await Promise.allSettled([first, second]);
    }
  });

  it('reuses live quota for two minutes then refreshes; manual refresh bypasses successful cache', async () => {
    let now = Date.now();
    spyOn(Date, 'now').mockImplementation(() => now);
    const first = await getLiveClaudeDesktopUsage('gmail');
    now += 119999;
    expect(await getLiveClaudeDesktopUsage('gmail')).toBe(first);
    now += 1;
    output = payload();
    expect(await getLiveClaudeDesktopUsage('gmail')).not.toBe(first);
    await getLiveClaudeDesktopUsage('gmail', { refresh: true });
    expect(exec).toHaveBeenCalledTimes(3);
  });

  it('recovers a cold transient failure after thirty seconds while coalescing manual retries', async () => {
    spyOn(claudeProfiles, 'listClaudeDesktopProfiles').mockResolvedValue([profile]);
    let now = Date.now();
    spyOn(Date, 'now').mockImplementation(() => now);
    exec.mockImplementation((...args: unknown[]) => {
      const callback = args.at(-1) as (error: Error, stdout: string, stderr: string) => void;
      callback(new Error('PRIVATE_CREDENTIAL_ERROR'), '', 'PRIVATE_CREDENTIAL_STDERR');
      return {} as childProcess.ChildProcess;
    });
    expect(await getLiveClaudeDesktopUsage('gmail')).toBeNull();
    now += 29999;
    expect(await getLiveClaudeDesktopUsage('gmail', { refresh: true })).toBeNull();
    expect(exec).toHaveBeenCalledTimes(1);
    now += 1;
    const held = holdHelperReads();
    output = payload({
      windows: [
        { key: 'seven_day', usedPercent: 100, resetAt: '2026-10-02T03:00:00Z' },
        { key: 'prepaid_balance', kind: 'balance', remaining: 0, unit: 'USD' },
      ],
    });
    const regular = getLiveClaudeDesktopUsage('gmail');
    const manual = getLiveClaudeDesktopUsage('gmail', { refresh: true });
    try {
      await waitForReadStarted(held.started);
      expect(exec).toHaveBeenCalledTimes(2);
      held.release();
      const recovered = await regular;
      expect(await manual).toBe(recovered);
      expect(recovered?.windows).toMatchObject([
        { key: 'seven_day', usedPercent: 100, resetAt: '2026-10-02T03:00:00.000Z' },
        { key: 'prepaid_balance', remaining: 0, unit: 'USD' },
      ]);
      expect(await getLiveClaudeDesktopUsage('gmail')).toBe(recovered);
      expect(exec).toHaveBeenCalledTimes(2);
      expect(JSON.stringify(recovered)).not.toContain('PRIVATE_CREDENTIAL');
    } finally {
      held.release();
      await Promise.allSettled([regular, manual]);
    }
  });

  it('binds cache to the current email, configured SSH host and CCS directory', async () => {
    const first = await getLiveClaudeDesktopUsage('gmail');
    writeProfiles([{ ...profile, email: 'different@example.com' }]);
    expect(await getLiveClaudeDesktopUsage('gmail')).toBeNull();
    writeProfiles([{ ...profile, windows: { ...profile.windows, sshHost: 'changed-windows' } }]);
    expect(await getLiveClaudeDesktopUsage('gmail')).not.toBe(first);
    process.env.CCS_DIR = path.join(root, 'second');
    writeProfiles();
    expect(await getLiveClaudeDesktopUsage('gmail')).not.toBe(first);
    expect(exec).toHaveBeenCalledTimes(4);
  });

  it('retains verified windows across cold module caches without stamping them fresh or running SSH', async () => {
    const verified = await getLiveClaudeDesktopUsage('gmail');
    invalidateClaudeDesktopLiveUsageCache();
    let now = Date.now();
    spyOn(Date, 'now').mockImplementation(() => now);
    now += 5 * 60_000;
    const retained = await getCachedClaudeDesktopLiveUsage('gmail');
    expect(retained).toEqual(verified);
    expect(retained?.fetchedAt).toBe(verified?.fetchedAt);
    expect(retained?.windows[1].resetAt).toBe('2026-10-02T02:59:59.716Z');
    expect(exec).toHaveBeenCalledTimes(1);
    now += 24 * 60 * 60_000;
    expect(await getCachedClaudeDesktopLiveUsage('gmail')).toBeNull();
  });

  it('keeps valid live quota available when protected cache storage cannot be written', async () => {
    fs.mkdirSync(path.join(process.env.CCS_DIR!, 'claude-desktop-live-cache'), { mode: 0o755 });
    fs.chmodSync(path.join(process.env.CCS_DIR!, 'claude-desktop-live-cache'), 0o755);
    const current = await getLiveClaudeDesktopUsage('gmail');
    expect(current?.windows[0].usedPercent).toBe(0);
    expect(current?.windows[1].resetAt).toBe('2026-10-02T02:59:59.716Z');
    expect(await getCachedClaudeDesktopLiveUsage('gmail')).toBeNull();
    expect(exec).toHaveBeenCalledTimes(1);
  });

  it('rejects persisted quota after source or account identity changes', async () => {
    await getLiveClaudeDesktopUsage('gmail');
    invalidateClaudeDesktopLiveUsageCache();
    writeProfiles([{ ...profile, windows: { ...profile.windows, sshHost: 'other-source' } }]);
    expect(await getCachedClaudeDesktopLiveUsage('gmail')).toBeNull();
    writeProfiles([{ ...profile, email: 'other@example.com' }]);
    expect(await getCachedClaudeDesktopLiveUsage('gmail')).toBeNull();
    expect(exec).toHaveBeenCalledTimes(1);
  });

  it('does not let an old-manifest helper finishing late displace the current protected snapshot', async () => {
    let finishOld: (error: null, stdout: string) => void = () => {};
    let notifyStarted: () => void = () => {};
    const started = new Promise<void>((resolve) => {
      notifyStarted = resolve;
    });
    const olderBody = payload({ windows: [{ key: 'seven_day', usedPercent: 42, resetAt: null }] });
    let calls = 0;
    exec.mockImplementation((...args: unknown[]) => {
      const callback = args.at(-1) as (error: null, stdout: string) => void;
      if (++calls === 1) {
        finishOld = callback;
        notifyStarted();
      } else {
        callback(
          null,
          payload({ windows: [{ key: 'seven_day', usedPercent: 77, resetAt: null }] })
        );
      }
      return {} as childProcess.ChildProcess;
    });
    const olderRead = getLiveClaudeDesktopUsage('gmail');
    await waitForReadStarted(started);
    writeProfiles([{ ...profile, windows: { ...profile.windows, sshHost: 'new-source' } }]);
    const current = await getLiveClaudeDesktopUsage('gmail');
    finishOld(null, olderBody);
    await olderRead;
    invalidateClaudeDesktopLiveUsageCache();
    expect(await getCachedClaudeDesktopLiveUsage('gmail')).toEqual(current);
    expect(current?.windows[0].usedPercent).toBe(77);
    expect(exec).toHaveBeenCalledTimes(2);
  });

  it('retains prior optional extras with original times when fresh Fable succeeds but web extras are unavailable', async () => {
    let now = Date.now();
    spyOn(Date, 'now').mockImplementation(() => now);
    const originalTime = new Date(now).toISOString();
    output = payload({
      windows: [
        { key: 'five_hour', usedPercent: 45 },
        { key: 'seven_day', usedPercent: 99 },
        { key: 'reset_credits_available', kind: 'balance', remaining: 0, unit: 'resets' },
        {
          key: 'reset_credit_used_grant_1',
          kind: 'balance',
          remaining: 0,
          used: 1,
          limit: 1,
          unit: 'resets',
          expiresAt: '2026-10-22T16:00:00Z',
        },
        { key: 'prepaid_balance', kind: 'balance', remaining: 50, unit: 'USD' },
      ],
    });
    await getLiveClaudeDesktopUsage('gmail');
    now += 300_000;
    output = payload({
      optionalExtras: { resetCredits: 'unavailable', prepaidBalance: 'unavailable' },
      windows: [
        { key: 'five_hour', usedPercent: 0, resetAt: null },
        { key: 'seven_day', usedPercent: 0, resetAt: null },
        { key: 'seven_day_fable', usedPercent: 0, resetAt: '2026-10-08T12:00:00Z' },
      ],
    });
    const current = await getLiveClaudeDesktopUsage('gmail');
    expect(current?.fetchedAt).toBe(new Date(now).toISOString());
    expect(current?.windows).toHaveLength(6);
    expect(current?.windows[2]).toMatchObject({
      key: 'seven_day_fable',
      usedPercent: 0,
      remainingPercent: 100,
    });
    expect(current?.windows[2]).not.toHaveProperty('status');
    expect(current?.windows.find((window) => window.key === 'prepaid_balance')).toMatchObject({
      remaining: 50,
      unit: 'USD',
      status: 'cached',
      sampledAt: originalTime,
    });
    expect(
      current?.windows.find((window) => window.key === 'reset_credit_used_grant_1')
    ).toMatchObject({
      remaining: 0,
      expiresAt: '2026-10-22T16:00:00.000Z',
      status: 'cached',
      sampledAt: originalTime,
    });
    const publicAccount = applyClaudeLiveUsage(claudeAccount(profile, 'mac'), current);
    expect(publicAccount.fetchedAt).toBe(current?.fetchedAt);
    expect(publicAccount).not.toHaveProperty('sourceContextFingerprint');
    expect(JSON.stringify(publicAccount)).not.toContain('a'.repeat(64));
    expect(publicAccount.windows.find((window) => window.key === 'prepaid_balance')).toMatchObject({
      status: 'cached',
      sampledAt: originalTime,
    });
    invalidateClaudeDesktopLiveUsageCache();
    expect(await getCachedClaudeDesktopLiveUsage('gmail')).toEqual(current);
    now += 300_000;
    output = payload({
      optionalExtras: { resetCredits: 'unavailable', prepaidBalance: 'unavailable' },
      windows: [
        { key: 'five_hour', usedPercent: 0, resetAt: null },
        { key: 'seven_day', usedPercent: 0, resetAt: null },
        { key: 'seven_day_fable', usedPercent: 7, resetAt: null },
      ],
    });
    const again = await getLiveClaudeDesktopUsage('gmail');
    expect(again?.windows.find((window) => window.key === 'prepaid_balance')).toMatchObject({
      sampledAt: originalTime,
      status: 'cached',
    });
    now += 24 * 60 * 60 * 1000;
    output = payload({
      optionalExtras: { resetCredits: 'unavailable', prepaidBalance: 'unavailable' },
      windows: [{ key: 'seven_day_fable', usedPercent: 8, resetAt: null }],
    });
    const expired = await getLiveClaudeDesktopUsage('gmail');
    expect(expired?.windows.map((window) => window.key)).toEqual(['seven_day_fable']);
  });

  it('clears retained extras on successful empty inventory and keeps independent failed groups cached', async () => {
    let now = Date.now();
    spyOn(Date, 'now').mockImplementation(() => now);
    const originalTime = new Date(now).toISOString();
    output = payload({
      windows: [
        { key: 'five_hour', usedPercent: 12 },
        { key: 'reset_credits_available', kind: 'balance', remaining: 1, unit: 'resets' },
        {
          key: 'reset_credit_available_grant_1',
          kind: 'balance',
          remaining: 1,
          used: 0,
          limit: 1,
          unit: 'resets',
          expiresAt: '2026-10-22T16:00:00Z',
        },
        { key: 'prepaid_balance', kind: 'balance', remaining: 50, unit: 'USD' },
      ],
    });
    await getLiveClaudeDesktopUsage('gmail');
    now += 300_000;
    output = payload({
      optionalExtras: { resetCredits: 'ok', prepaidBalance: 'unavailable' },
      windows: [{ key: 'seven_day_fable', usedPercent: 0, resetAt: null }],
    });
    const partial = await getLiveClaudeDesktopUsage('gmail');
    expect(partial?.windows.map((window) => window.key)).toEqual([
      'seven_day_fable',
      'prepaid_balance',
    ]);
    expect(partial?.windows[1]).toMatchObject({
      status: 'cached',
      sampledAt: originalTime,
      remaining: 50,
    });
    now += 300_000;
    output = payload({
      optionalExtras: { resetCredits: 'ok', prepaidBalance: 'ok' },
      windows: [{ key: 'seven_day_fable', usedPercent: 0, resetAt: null }],
    });
    const empty = await getLiveClaudeDesktopUsage('gmail');
    expect(empty?.windows.map((window) => window.key)).toEqual(['seven_day_fable']);
    invalidateClaudeDesktopLiveUsageCache();
    expect(await getCachedClaudeDesktopLiveUsage('gmail')).toEqual(empty);
  });

  it('fresh reported provider zero/null updates replace richer retained metadata normally', async () => {
    let now = Date.now();
    spyOn(Date, 'now').mockImplementation(() => now);
    output = payload({
      windows: [
        { key: 'five_hour', usedPercent: 45, resetAt: '2026-10-02T03:00:00Z' },
        { key: 'seven_day', usedPercent: 99, resetAt: '2026-10-06T20:00:00Z' },
        { key: 'prepaid_balance', kind: 'balance', remaining: 50, unit: 'USD' },
      ],
    });
    await getLiveClaudeDesktopUsage('gmail');
    now += 1000;
    output = payload({
      windows: [
        { key: 'five_hour', usedPercent: 0, resetAt: null },
        { key: 'seven_day', usedPercent: 0, resetAt: null },
        { key: 'prepaid_balance', kind: 'balance', remaining: 0, unit: 'USD' },
      ],
    });
    const current = await getLiveClaudeDesktopUsage('gmail', { refresh: true });
    invalidateClaudeDesktopLiveUsageCache();
    const retained = await getCachedClaudeDesktopLiveUsage('gmail');
    expect(retained).toEqual(current);
    expect(retained?.windows).toHaveLength(3);
    expect(
      retained?.windows.slice(0, 2).map((window) => [window.usedPercent, window.resetAt])
    ).toEqual([
      [0, null],
      [0, null],
    ]);
    expect(retained?.windows[2]?.remaining).toBe(0);
    expect(retained?.windows[2]).not.toHaveProperty('status');
    const contents = fs.readFileSync(
      path.join(process.env.CCS_DIR!, 'claude-desktop-live-cache', 'gmail.json'),
      'utf8'
    );
    expect(contents).not.toContain('credential-sentinel');
  });

  it.each([
    'plan',
    'source',
    'unknown-availability',
    'organization-context',
    'client-context',
    'legacy-unbound-cache',
  ])('never inherits optional extras across changed %s', async (changed) => {
    output = payload({
      ...(changed === 'legacy-unbound-cache' ? { sourceContextFingerprint: undefined } : {}),
      windows: [
        { key: 'five_hour', usedPercent: 7 },
        { key: 'prepaid_balance', kind: 'balance', remaining: 50, unit: 'USD' },
      ],
    });
    await getLiveClaudeDesktopUsage('gmail');
    if (changed === 'source')
      writeProfiles([
        { ...profile, windows: { ...profile.windows, sshHost: 'different-windows' } },
      ]);
    output = payload({
      ...(changed === 'plan' ? { plan: 'pro' } : {}),
      ...(changed === 'organization-context' ? { sourceContextFingerprint: 'b'.repeat(64) } : {}),
      ...(changed === 'client-context' ? { sourceContextFingerprint: 'c'.repeat(64) } : {}),
      ...(changed === 'unknown-availability'
        ? {}
        : { optionalExtras: { resetCredits: 'unavailable', prepaidBalance: 'unavailable' } }),
      windows: [{ key: 'seven_day_fable', usedPercent: 0, resetAt: null }],
    });
    const current = await getLiveClaudeDesktopUsage('gmail', { refresh: true });
    expect(current?.windows.map((window) => window.key)).toEqual(['seven_day_fable']);
  });

  it('retains the latest verified memory extras when protected storage cannot be written', async () => {
    let now = Date.now();
    spyOn(Date, 'now').mockImplementation(() => now);
    const originalTime = new Date(now).toISOString();
    fs.writeFileSync(path.join(process.env.CCS_DIR!, 'claude-desktop-live-cache'), 'blocked');
    output = payload({
      windows: [
        { key: 'five_hour', usedPercent: 7 },
        { key: 'prepaid_balance', kind: 'balance', remaining: 50, unit: 'USD' },
      ],
    });
    await getLiveClaudeDesktopUsage('gmail');
    now += 300_000;
    output = payload({
      optionalExtras: { resetCredits: 'unavailable', prepaidBalance: 'unavailable' },
      windows: [{ key: 'seven_day_fable', usedPercent: 0, resetAt: null }],
    });
    const retained = await getLiveClaudeDesktopUsage('gmail');
    expect(retained?.windows[1]).toMatchObject({
      remaining: 50,
      status: 'cached',
      sampledAt: originalTime,
    });
    now += 300_000;
    output = payload({
      optionalExtras: { resetCredits: 'ok', prepaidBalance: 'ok' },
      windows: [
        { key: 'seven_day_fable', usedPercent: 0, resetAt: null },
        { key: 'prepaid_balance', kind: 'balance', remaining: 0, unit: 'USD' },
      ],
    });
    await getLiveClaudeDesktopUsage('gmail');
    const zeroTime = new Date(now).toISOString();
    now += 300_000;
    output = payload({
      optionalExtras: { resetCredits: 'unavailable', prepaidBalance: 'unavailable' },
      windows: [{ key: 'seven_day_fable', usedPercent: 0, resetAt: null }],
    });
    const latest = await getLiveClaudeDesktopUsage('gmail');
    expect(latest?.windows[1]).toMatchObject({
      remaining: 0,
      status: 'cached',
      sampledAt: zeroTime,
    });
    expect(
      fs.readFileSync(path.join(process.env.CCS_DIR!, 'claude-desktop-live-cache'), 'utf8')
    ).toBe('blocked');
  });
});
