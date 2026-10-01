import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as childProcess from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as claudeProfiles from '../../../src/web-server/services/claude-desktop-profile-service';
import {
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
    expect(script).toContain("'.ccs', 'claude-session-migration', 'venv', 'Scripts', 'python.exe'");
    expect(script).toContain('Test-Path -LiteralPath $venv -PathType Leaf');
    expect(script).toContain("else { 'python.exe' }");
    expect(script).toContain('& $python $helper');
    expect(script).not.toContain('C:\\fixture');
    expect(script).not.toContain('Invoke-Expression');
    expect(script).not.toContain('fixture@example.com');
    expect(options).toMatchObject({ timeout: 20000, maxBuffer: 65536, windowsHide: true });
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
});
