import { afterEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as childProcess from 'child_process';
import path from 'path';
import {
  ADDITIONAL_PROVIDERS,
  AdditionalUsageTransportError,
  collectorAccountArguments,
  isAdditionalProvider,
  isSafeUsageSshAlias,
  runAdditionalUsageSource,
  type AdditionalUsageSource,
} from '../../../src/web-server/services/additional-usage-transport';

const localPlatform =
  process.platform === 'win32' ? 'windows' : process.platform === 'darwin' ? 'mac' : 'ubuntu';

function mockProcess(stdout = '', failure?: Partial<childProcess.ExecFileException>) {
  return spyOn(childProcess, 'execFile').mockImplementation((...args: unknown[]) => {
    const callback = args.at(-1) as (
      error: childProcess.ExecFileException | null,
      stdout: string,
      stderr: string
    ) => void;
    callback(
      failure ? Object.assign(new Error('PRIVATE_EXEC_ERROR'), failure) : null,
      stdout,
      'PRIVATE_STDERR'
    );
    return {} as childProcess.ChildProcess;
  });
}

afterEach(() => mock.restore());

describe('additional account usage transport', () => {
  it.each(ADDITIONAL_PROVIDERS)(
    'runs the fixed local helper with enumerated arguments for %s',
    async (provider) => {
      const sample = '{"accounts":[]}\n';
      const exec = mockProcess(sample);
      expect(await runAdditionalUsageSource({ provider, platform: localPlatform })).toBe(sample);
      expect(exec).toHaveBeenCalledTimes(1);
      const [binary, args, options] = exec.mock.calls[0]!;
      expect(binary).toBe(localPlatform === 'windows' ? 'python.exe' : '/usr/bin/python3');
      const helper = ['antigravity', 'muse', 'cursor'].includes(provider)
        ? 'desktop_usage.py'
        : 'plan_usage.py';
      expect(args).toEqual([
        path.resolve(__dirname, '../../../scripts/account-usage', helper),
        '--provider',
        provider,
        '--platform',
        localPlatform,
      ]);
      expect(options).toMatchObject({
        encoding: 'utf8',
        timeout: 25_000,
        maxBuffer: 64 * 1024,
        windowsHide: true,
        env: { PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1' },
      });
    }
  );

  it.each(['mac', 'ubuntu'] as const)(
    'uses bounded SSH and a quoted fixed HOME helper on %s',
    async (platform) => {
      const exec = mockProcess('{}');
      await runAdditionalUsageSource({ provider: 'cursor', platform, sshHost: 'work-machine' });
      const [binary, args, options] = exec.mock.calls[0]!;
      expect(binary).toBe('ssh');
      expect(args).toEqual([
        '-T',
        '-o',
        'BatchMode=yes',
        '-o',
        'ConnectTimeout=5',
        '-o',
        'ConnectionAttempts=1',
        '-o',
        'ServerAliveInterval=5',
        '-o',
        'ServerAliveCountMax=1',
        '--',
        'work-machine',
        `/usr/bin/python3 "$HOME/.ccs/account-usage/desktop_usage.py" --provider 'cursor' --platform '${platform}'`,
      ]);
      expect(options).toMatchObject({ timeout: 25_000, maxBuffer: 64 * 1024 });
    }
  );

  it('uses the fixed plan helper for remote plan providers', async () => {
    const exec = mockProcess('{}');
    await runAdditionalUsageSource({ provider: 'qwen', platform: 'mac', sshHost: 'work-mac' });
    expect((exec.mock.calls[0]![1] as string[]).at(-1)).toBe(
      `/usr/bin/python3 "$HOME/.ccs/account-usage/plan_usage.py" --provider 'qwen' --platform 'mac'`
    );
  });

  it.each([
    ['antigravity', 'desktop_usage.py'],
    ['opencode-go', 'plan_usage.py'],
  ] as const)('uses a fixed encoded Windows helper for %s', async (provider, helper) => {
    const exec = mockProcess('{}');
    await runAdditionalUsageSource({ provider, platform: 'windows', sshHost: 'work-windows' });
    const [binary, args, options] = exec.mock.calls[0]!;
    expect(binary).toBe('ssh');
    expect(args).toContain('BatchMode=yes');
    expect(args).toContain('ConnectTimeout=5');
    expect(args).toContain('--');
    expect(args).toContain('work-windows');
    const command = (args as string[]).at(-1)!;
    expect(command).toMatch(
      /^powershell\.exe -NoProfile -NonInteractive -EncodedCommand [A-Za-z0-9+/=]+$/
    );
    const script = Buffer.from(command.split(' ').at(-1)!, 'base64').toString('utf16le');
    expect(script).toBe(
      [
        "$ErrorActionPreference = 'Stop'",
        "$env:PYTHONIOENCODING = 'utf-8'",
        "$env:PYTHONUTF8 = '1'",
        `$helper = [IO.Path]::Combine($HOME, '.ccs', 'account-usage', '${helper}')`,
        `& python.exe $helper --provider '${provider}' --platform 'windows'`,
        'exit $LASTEXITCODE',
      ].join('; ')
    );
    expect(script).not.toContain('Invoke-Expression');
    expect(options).toMatchObject({ timeout: 25_000, maxBuffer: 64 * 1024 });
  });

  it.each(['cursor; anything', "qwen' ; anything", '../cursor', '', null, undefined])(
    'rejects an invalid provider before execution (%s)',
    async (provider) => {
      const exec = mockProcess();
      const source = { provider, platform: 'mac', sshHost: 'work-mac' };
      await expect(
        runAdditionalUsageSource(source as unknown as AdditionalUsageSource)
      ).rejects.toBeInstanceOf(AdditionalUsageTransportError);
      expect(exec).not.toHaveBeenCalled();
      expect(isAdditionalProvider(provider)).toBe(false);
    }
  );

  it.each(['darwin', "mac'; anything", 'mac\nanything', '', null, undefined])(
    'rejects an invalid platform before execution (%s)',
    async (platform) => {
      const exec = mockProcess();
      const source = { provider: 'cursor', platform, sshHost: 'work-mac' };
      await expect(
        runAdditionalUsageSource(source as unknown as AdditionalUsageSource)
      ).rejects.toBeInstanceOf(AdditionalUsageTransportError);
      expect(exec).not.toHaveBeenCalled();
    }
  );

  it.each([
    '-oProxyCommand=anything',
    'work;anything',
    'user@work',
    'work\nanything',
    'work other',
    '',
    'a'.repeat(129),
    null,
    42,
  ])('rejects an unsafe SSH alias before execution (%s)', async (sshHost) => {
    const exec = mockProcess();
    const source = { provider: 'cursor', platform: 'mac', sshHost };
    await expect(
      runAdditionalUsageSource(source as unknown as AdditionalUsageSource)
    ).rejects.toBeInstanceOf(AdditionalUsageTransportError);
    expect(exec).not.toHaveBeenCalled();
    expect(isSafeUsageSshAlias(sshHost)).toBe(false);
  });

  it('accepts bounded SSH aliases and known provider names', () => {
    expect(isSafeUsageSshAlias('work-mac_1.example')).toBe(true);
    expect(isSafeUsageSshAlias('a'.repeat(128))).toBe(true);
    for (const provider of ADDITIONAL_PROVIDERS) {
      expect(isAdditionalProvider(provider)).toBe(true);
    }
  });

  it('rejects a local platform mismatch before execution', async () => {
    const exec = mockProcess();
    const platform = localPlatform === 'mac' ? 'ubuntu' : 'mac';
    await expect(runAdditionalUsageSource({ provider: 'muse', platform })).rejects.toThrow(
      'Account usage request failed.'
    );
    expect(exec).not.toHaveBeenCalled();
  });

  it('sanitizes private execution errors and stderr', async () => {
    mockProcess('PRIVATE_STDOUT', { code: 255 });
    const error = await runAdditionalUsageSource({
      provider: 'zai',
      platform: localPlatform,
    }).catch((failure) => failure);
    expect(error).toBeInstanceOf(AdditionalUsageTransportError);
    expect(error.name).toBe('AdditionalUsageTransportError');
    expect(error.message).toBe('Account usage request failed.');
    expect(error.timedOut).toBe(false);
    expect(error.stack).not.toContain('PRIVATE');
    expect(JSON.stringify(error)).not.toContain('PRIVATE');
    expect(error).not.toHaveProperty('cause');
  });

  it.each([
    [{ killed: true, signal: 'SIGTERM' }, true],
    [{ killed: false, signal: 'SIGTERM' }, false],
    [{ killed: true, signal: 'SIGKILL' }, false],
  ] as const)(
    'classifies a process failure as a timeout only when appropriate (%j)',
    async (failure, timedOut) => {
      mockProcess('', failure);
      const error = await runAdditionalUsageSource({
        provider: 'kimi-code',
        platform: localPlatform,
      }).catch((reason) => reason);
      expect(error).toBeInstanceOf(AdditionalUsageTransportError);
      expect(error.timedOut).toBe(timedOut);
      expect(error.message).toBe(
        timedOut ? 'Account usage request timed out.' : 'Account usage request failed.'
      );
    }
  );

  it('accepts stdout exactly at the byte limit and ignores private stderr', async () => {
    const sample = 'a'.repeat(64 * 1024);
    mockProcess(sample);
    expect(await runAdditionalUsageSource({ provider: 'zai', platform: localPlatform })).toBe(
      sample
    );
  });

  it.each(['a'.repeat(64 * 1024 + 1), 'é'.repeat(32 * 1024 + 1)])(
    'rejects oversized stdout even without an execution error',
    async (stdout) => {
      mockProcess(stdout);
      expect(stdout.length).toBeLessThanOrEqual(64 * 1024 + 1);
      expect(Buffer.byteLength(stdout, 'utf8')).toBeGreaterThan(64 * 1024);
      const error = await runAdditionalUsageSource({
        provider: 'cursor',
        platform: localPlatform,
      }).catch((failure) => failure);
      expect(error).toBeInstanceOf(AdditionalUsageTransportError);
      expect(error.timedOut).toBe(false);
      expect(error.message).toBe('Account usage request failed.');
    }
  );
});

describe('registry v2 collector arguments', () => {
  const keyAccount: AdditionalUsageSource = {
    provider: 'zai',
    platform: localPlatform,
    account: {
      id: 'zai:acct:9f2c41d0',
      label: 'Work label with spaces',
      credential: { kind: 'aac-key', keyId: '9f2c41d0' },
    },
  };

  it('passes exactly the enumerated account arguments for a key account', async () => {
    const exec = mockProcess('{}');
    await runAdditionalUsageSource(keyAccount);
    const [binary, args] = exec.mock.calls[0]!;
    expect(binary).toBe(localPlatform === 'windows' ? 'python.exe' : '/usr/bin/python3');
    expect((args as string[]).slice(1)).toEqual([
      '--provider',
      'zai',
      '--platform',
      localPlatform,
      '--account',
      'zai:acct:9f2c41d0',
      '--credential',
      'aac-key',
      '--key-id',
      '9f2c41d0',
    ]);
    // No path, host, label or secret beyond the fixed helper path.
    const joined = (args as string[]).slice(1).join(' ');
    expect(joined).not.toContain('/');
    expect(joined).not.toContain('Work label');
  });

  it('keeps the exact version 1 call for a discover account', async () => {
    const exec = mockProcess('{}');
    await runAdditionalUsageSource({
      provider: 'cursor',
      platform: 'mac',
      sshHost: 'work-mac',
      account: { id: 'cursor:usage', label: null, credential: { kind: 'discover' } },
    });
    expect((exec.mock.calls[0]![1] as string[]).at(-1)).toBe(
      `/usr/bin/python3 "$HOME/.ccs/account-usage/desktop_usage.py" --provider 'cursor' --platform 'mac'`
    );
  });

  it('quotes the account arguments in the remote Mac and Windows commands', async () => {
    const exec = mockProcess('{}');
    await runAdditionalUsageSource({ ...keyAccount, platform: 'mac', sshHost: 'work-mac' });
    expect((exec.mock.calls[0]![1] as string[]).at(-1)).toBe(
      `/usr/bin/python3 "$HOME/.ccs/account-usage/plan_usage.py" --provider 'zai' --platform 'mac' --account 'zai:acct:9f2c41d0' --credential 'aac-key' --key-id '9f2c41d0'`
    );
    await runAdditionalUsageSource({
      provider: 'qwen',
      platform: 'windows',
      sshHost: 'work-windows',
      account: {
        id: 'qwen:acct:0a1b2c3d',
        label: null,
        credential: { kind: 'browser-capsule', capsuleId: '0a1b2c3d' },
      },
    });
    const command = (exec.mock.calls[1]![1] as string[]).at(-1)!;
    const script = Buffer.from(command.split(' ').at(-1)!, 'base64').toString('utf16le');
    expect(script).toContain(
      "& python.exe $helper --provider 'qwen' --platform 'windows' --account 'qwen:acct:0a1b2c3d' --credential 'browser-capsule' --capsule-id '0a1b2c3d'"
    );
  });

  it.each([
    [
      'an id of another provider',
      {
        id: 'kimi-code:acct:9f2c41d0',
        label: null,
        credential: { kind: 'aac-key', keyId: '9f2c41d0' },
      },
    ],
    [
      'a quote in the id',
      {
        id: "zai:acct:9f2c41d0' ; x",
        label: null,
        credential: { kind: 'aac-key', keyId: '9f2c41d0' },
      },
    ],
    [
      'a path as key id',
      { id: 'zai:acct:9f2c41d0', label: null, credential: { kind: 'aac-key', keyId: '../../x' } },
    ],
    [
      'an extra credential field',
      {
        id: 'zai:acct:9f2c41d0',
        label: null,
        credential: { kind: 'aac-key', keyId: '9f2c41d0', path: '/x' },
      },
    ],
    [
      'a capsule on Z.ai',
      {
        id: 'zai:acct:9f2c41d0',
        label: null,
        credential: { kind: 'browser-capsule', capsuleId: 'default' },
      },
    ],
    [
      'a config home, not read by helpers yet',
      {
        id: 'zai:acct:9f2c41d0',
        label: null,
        credential: { kind: 'config-home', homeId: '9f2c41d0' },
      },
    ],
  ])('rejects %s before execution', async (_name, account) => {
    const exec = mockProcess('{}');
    await expect(
      runAdditionalUsageSource({
        provider: 'zai',
        platform: localPlatform,
        account,
      } as unknown as AdditionalUsageSource)
    ).rejects.toBeInstanceOf(AdditionalUsageTransportError);
    expect(exec).not.toHaveBeenCalled();
  });

  it('flags exit status 2 as an outdated helper only when account arguments were sent', async () => {
    mockProcess('', { code: 2 });
    const outdated = await runAdditionalUsageSource(keyAccount).catch((failure) => failure);
    expect(outdated).toBeInstanceOf(AdditionalUsageTransportError);
    expect(outdated.helperOutdated).toBe(true);
    expect(outdated.message).toBe('Account usage request failed.');
    const legacy = await runAdditionalUsageSource({
      provider: 'zai',
      platform: localPlatform,
    }).catch((failure) => failure);
    expect(legacy.helperOutdated).toBe(false);
    mock.restore();
    mockProcess('', { code: 1 });
    expect(
      (await runAdditionalUsageSource(keyAccount).catch((failure) => failure)).helperOutdated
    ).toBe(false);
  });

  it('builds no account arguments for version 1 sources', () => {
    expect(collectorAccountArguments({ provider: 'zai', platform: 'ubuntu' })).toEqual([]);
  });
});
