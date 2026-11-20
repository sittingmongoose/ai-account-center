/**
 * One table pinning the exact ssh argv of every dashboard transport.
 *
 * Written against origin/main (df371d05b) BEFORE ssh-policy.ts existed: each
 * row captures the argv through the transport's existing seam — an exported
 * invocation builder, an injected runner, or a child_process spy of the kind
 * the sibling suites already use. Nothing here reaches a real host or a real
 * home: process starts are replaced and every home is a temporary directory.
 *
 * The same table must pass unchanged after the transports move onto the
 * shared ssh policy; it is the proof that the refactor changed no argv.
 * The remote command builders are not part of the policy: where a command is
 * built by an exported builder the table pins against that builder, and where
 * it is a fixed literal the table spells the literal out.
 */
import { sshOptions } from '../../../src/web-server/services/ssh-policy';
import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import * as childProcess from 'child_process';
import { EventEmitter } from 'events';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { PassThrough } from 'stream';

import {
  appUpdateInvocation,
  helperExtract,
  helperHashQuery,
  localHelperChecksums,
  syncRemoteHelpers,
} from '../../../src/web-server/services/app-update-hosts';
import {
  analyticsHelperCommand,
  runAnalyticsRemoteHelper,
} from '../../../src/web-server/services/analytics-remote-transport';
import { runKeyHelperOverSsh } from '../../../src/web-server/services/account-key-store';
import {
  getLiveClaudeDesktopUsage,
  invalidateClaudeDesktopLiveUsageCache,
} from '../../../src/web-server/services/claude-desktop-live-service';
import {
  readClaudeDesktopUsageHistory,
  runClaudeHistoryHelper,
} from '../../../src/web-server/services/claude-desktop-transport';
import { runClaudeHostOverSsh } from '../../../src/web-server/services/claude-host-transport';
import { SSH_ALIASES } from '../../../src/web-server/services/dashboard-hosts';
import { MuseAccountLifecycle } from '../../../src/web-server/services/muse-account-lifecycle';
import { OpenCodeConsoleWalletService } from '../../../src/web-server/services/opencode-console-wallet-service';
import {
  privateUsageInvocation,
  usageHelperCommand,
} from '../../../src/web-server/services/usage-failover-transport';
import {
  REMOTE_MACHINES,
  machineProbeCommand,
  readRemoteMachine,
  readRemotePins,
  remotePinsCommand,
  type CommandRunner,
} from '../../../src/web-server/status/status-collectors';
import { ANALYTICS_TOOLS } from '../../../src/web-server/usage/analytics-tools';
import type { RegistryAccount } from '../../../src/web-server/services/account-registry-v2';

/** The fixed non-interactive option set every transport shares. */
const keepalive = (interval: number, countMax: number): string[] => [
  '-o',
  `ServerAliveInterval=${interval}`,
  '-o',
  `ServerAliveCountMax=${countMax}`,
];
const expectedArgv = async (
  host: string,
  command: string,
  alive: string[] = []
): Promise<string[]> => ['-T', ...(await sshOptions(host)), ...alive, '--', host, command];

interface ExecCall {
  file: string;
  args: string[];
  input: string;
}

/**
 * Replaces execFile, recording every call and answering immediately. The
 * answer may depend on the exec options (buffer encoding for private pipes).
 */
function captureExec(answer: (options: Record<string, unknown>) => string | Buffer = () => '') {
  const calls: ExecCall[] = [];
  spyOn(childProcess, 'execFile').mockImplementation(((...args: unknown[]) => {
    const [file, argv, options] = args as [string, string[], Record<string, unknown>];
    const callback = args.at(-1) as (error: null, stdout: string | Buffer, stderr: string) => void;
    const call: ExecCall = { file, args: [...argv], input: '' };
    calls.push(call);
    setImmediate(() => callback(null, answer(options), ''));
    return {
      stdin: {
        on: () => undefined,
        end: (data?: string | Buffer) => {
          call.input = data === undefined ? '' : data.toString('utf8');
        },
      },
      on: () => undefined,
      kill: () => undefined,
    };
  }) as never);
  return calls;
}

/** Replaces spawn with settling children, recording every call (tar + ssh push). */
function captureSpawn() {
  const calls: Array<{ file: string; args: string[] }> = [];
  spyOn(childProcess, 'spawn').mockImplementation(((file: string, args: string[]) => {
    calls.push({ file, args: [...args] });
    const child = new EventEmitter() as EventEmitter & {
      stdin: PassThrough;
      stdout: PassThrough;
      kill: () => void;
    };
    child.stdin = new PassThrough();
    child.stdout = new PassThrough();
    child.kill = () => undefined;
    setImmediate(() => {
      child.stdout.end();
      child.emit('close', 0);
    });
    return child;
  }) as never);
  return calls;
}

let home: string;
const previousEnv = { CCS_HOME: process.env.CCS_HOME, CCS_DIR: process.env.CCS_DIR };

beforeEach(() => {
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-ssh-policy-'));
  process.env.CCS_HOME = home;
  process.env.CCS_DIR = home;
  // SSH argv builders need an identity in the home; a placeholder, never a key.
  fs.mkdirSync(path.join(home, '.ssh'), { recursive: true });
  fs.writeFileSync(path.join(home, '.ssh', 'id_ed25519'), 'fixture placeholder, not a key\n');
});

afterEach(() => {
  mock.restore();
  invalidateClaudeDesktopLiveUsageCache();
  if (previousEnv.CCS_HOME === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = previousEnv.CCS_HOME;
  if (previousEnv.CCS_DIR === undefined) delete process.env.CCS_DIR;
  else process.env.CCS_DIR = previousEnv.CCS_DIR;
  fs.rmSync(home, { recursive: true, force: true });
});

const sshCalls = (calls: ExecCall[]): ExecCall[] =>
  calls.filter((call) => /(^|[\/])ssh(\.exe)?$/.test(call.file));

describe('analytics-remote-transport', () => {
  it('streams the packaged helper with interval 5 / count 6', async () => {
    const calls = captureExec(() =>
      JSON.stringify({ version: 1, truncated: false, kinds: {}, rows: [], srows: [] })
    );
    await runAnalyticsRemoteHelper('nas1-agent', 'nas1', {
      kinds: [...ANALYTICS_TOOLS],
      minDateMs: 0,
      fingerprints: {},
    });
    expect(sshCalls(calls)).toHaveLength(1);
    expect(sshCalls(calls)[0]!.args).toEqual(
      await expectedArgv('nas1-agent', analyticsHelperCommand('nas1'), keepalive(5, 6))
    );
  });
});

describe('app-update-hosts', () => {
  it('runs the update helper with interval 10 / count 2 on every remote platform', async () => {
    for (const [platform, helperOs] of [
      ['nas1', 'ubuntu'],
      ['mac', 'mac'],
    ] as const) {
      const invocation = await appUpdateInvocation(platform, '');
      expect(invocation.binary).toMatch(/(^|[\/])ssh(\.exe)?$/);
      expect(invocation.args).toEqual(
        await expectedArgv(
          SSH_ALIASES[platform],
          `AAC_UPDATE_PROGRESS=1 /usr/bin/python3 "$HOME/.ccs/app-updates/app_updates.py" --apply --platform ${helperOs}`,
          keepalive(10, 2)
        )
      );
    }
    const windowsScript = [
      "$ErrorActionPreference='Stop'",
      "$env:PYTHONUTF8='1'",
      "$env:PYTHONIOENCODING='utf-8'",
      "$env:AAC_UPDATE_PROGRESS='1'",
      "$helper=[IO.Path]::Combine($HOME,'.ccs','app-updates','app_updates.py')",
      '& python.exe $helper --apply --platform windows',
      'exit $LASTEXITCODE',
    ].join('; ');
    const windows = await appUpdateInvocation('windows', '');
    expect(windows.binary).toMatch(/(^|[\/])ssh(\.exe)?$/);
    expect(windows.args).toEqual(
      await expectedArgv(
        SSH_ALIASES.windows,
        `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(windowsScript, 'utf16le').toString('base64')}`,
        keepalive(10, 2)
      )
    );
  });

  it('queries and pushes helpers with no keepalive options at all', async () => {
    const execCalls = captureExec(() => '');
    const spawnCalls = captureSpawn();
    await syncRemoteHelpers('nas1');
    const query = sshCalls(execCalls);
    expect(query).toHaveLength(1);
    expect(query[0]!.args).toEqual(await expectedArgv(SSH_ALIASES.nas1, helperHashQuery('nas1')));
    const push = spawnCalls.filter((call) => /(^|[\/])ssh(\.exe)?$/.test(call.file));
    expect(push).toHaveLength(1);
    expect(push[0]!.args).toEqual(
      await expectedArgv(SSH_ALIASES.nas1, helperExtract('nas1', localHelperChecksums()))
    );
  });
});

describe('claude-desktop-live-service', () => {
  it('runs the Windows usage helper with interval 5 / count 1', async () => {
    fs.writeFileSync(
      path.join(home, 'claude-desktop-profiles.json'),
      JSON.stringify({
        version: 1,
        profiles: [
          {
            id: 'gmail',
            email: 'fixture@example.com',
            windows: {
              launcherName: 'Fixture Windows',
              sshHost: 'fixture-windows',
              profilePath: 'C:\\fixture\\claude',
            },
          },
        ],
      })
    );
    const payload = JSON.stringify({
      schemaVersion: 1,
      provider: 'claude',
      profileId: 'gmail',
      email: 'fixture@example.com',
      platform: 'windows',
      status: 'ok',
      plan: 'max',
      accountVerified: true,
      organizationVerified: true,
      sourceContextFingerprint: 'a'.repeat(64),
      fetchedAt: new Date().toISOString(),
      windows: [
        { key: 'five_hour', usedPercent: 0, resetAt: null },
        { key: 'seven_day', usedPercent: 100, resetAt: '2026-10-02T02:59:59.716415Z' },
      ],
    });
    const calls = captureExec(() => payload);
    await getLiveClaudeDesktopUsage('gmail', { refresh: true });
    expect(sshCalls(calls)).toHaveLength(1);
    const script = [
      "$ErrorActionPreference = 'Stop'",
      "$env:PYTHONIOENCODING = 'utf-8'",
      "$env:PYTHONUTF8 = '1'",
      "$helper = [IO.Path]::Combine($HOME, '.ccs', 'account-usage', 'claude_usage.py')",
      "$venv = [IO.Path]::Combine($HOME, '.ccs', 'claude-session-migration', 'venv', 'Scripts', 'python.exe')",
      "$python = if (Test-Path -LiteralPath $venv -PathType Leaf) { $venv } else { 'python.exe' }",
      "& $python $helper --provider 'claude' --profile 'gmail' --platform 'windows' --expected-email 'fixture@example.com' --profile-dir 'C:\\fixture\\claude'",
      'exit $LASTEXITCODE',
    ].join('; ');
    expect(sshCalls(calls)[0]!.args).toEqual(
      await expectedArgv(
        'fixture-windows',
        `powershell.exe -NoProfile -NonInteractive -EncodedCommand ${Buffer.from(script, 'utf16le').toString('base64')}`,
        keepalive(5, 1)
      )
    );
  });
});

describe('claude-desktop-transport', () => {
  const launcher = {
    launcherName: 'Fixture Mac',
    profilePath: '/fixture/claude',
    sshHost: 'fixture-mac',
  };

  it('reads the usage history with interval 5 / count 1', async () => {
    const calls = captureExec(() => '{"version":2,"samples":[]}');
    await readClaudeDesktopUsageHistory(launcher, 'mac');
    expect(sshCalls(calls)).toHaveLength(1);
    const history = "'/fixture/claude/plan-usage-history.json'";
    const command = [
      `if [ ! -f ${history} ]; then printf '%s' 'CCS_DESKTOP_USAGE_MISSING'; exit 0; fi`,
      `history_size=$(/usr/bin/stat -f '%z' ${history}) || exit 1`,
      'test "$history_size" -le 1048576 || exit 1',
      `/bin/cat ${history}`,
    ].join('\n');
    expect(sshCalls(calls)[0]!.args).toEqual(
      await expectedArgv('fixture-mac', command, keepalive(5, 1))
    );
  });

  it('pipes the private history helper with the same interval 5 / count 1', async () => {
    const calls = captureExec((options) =>
      options.encoding === 'buffer' ? Buffer.from('[]') : '[]'
    );
    await runClaudeHistoryHelper(launcher, 'mac', 'gmail', {
      profileId: 'gmail',
      platform: 'mac',
      mode: 'collect',
    });
    expect(sshCalls(calls)).toHaveLength(1);
    const code =
      "import sys,json,io;_p=json.loads(sys.stdin.buffer.read().decode('utf-8'));_s=_p['helperSource'];globals()['__ccs_history_helper_source__']=_s;globals()['__ccs_history_node_bridge_source__']=_p.get('nodeBridgeSource');sys.stdin=io.TextIOWrapper(io.BytesIO(json.dumps(_p['request']).encode('utf-8')),encoding='utf-8');exec(compile(_s,'managed-history-helper','exec'))";
    const command = `/usr/bin/python3 -c '${code.replace(/'/g, `'"'"'`)}'`;
    expect(sshCalls(calls)[0]!.args).toEqual(
      await expectedArgv('fixture-mac', command, keepalive(5, 1))
    );
  });
});

describe('claude-host-transport', () => {
  it('runs a host step with interval 5 / count 1', async () => {
    const calls = captureExec(() => 'ok');
    await runClaudeHostOverSsh('fixture-mac', 'echo ok', 'input');
    expect(sshCalls(calls)).toHaveLength(1);
    expect(sshCalls(calls)[0]!.args).toEqual(
      await expectedArgv('fixture-mac', 'echo ok', keepalive(5, 1))
    );
  });
});

describe('account-key-store', () => {
  it('runs the key helper with interval 5 / count 1', async () => {
    const calls = captureExec(() => 'ok');
    await runKeyHelperOverSsh('fixture-mac', 'key-helper-command', 'input');
    expect(sshCalls(calls)).toHaveLength(1);
    expect(sshCalls(calls)[0]!.args).toEqual(
      await expectedArgv('fixture-mac', 'key-helper-command', keepalive(5, 1))
    );
  });
});

describe('muse-account-lifecycle', () => {
  it('offers muse login with interval 5 / count 1', async () => {
    const entry: RegistryAccount = {
      id: 'muse:usage',
      provider: 'muse',
      platform: 'mac',
      sshHost: 'mac-host',
      label: null,
      credential: { kind: 'discover' },
      createdAt: '2026-10-02T08:00:00Z',
      createdBy: 'migration',
    };
    const spec = new MuseAccountLifecycle({ enabled: true }).signInAgainFlow(entry);
    const command = await spec.prepare('job_0123456789abcdef');
    expect(command.file).toMatch(/(^|[\/])ssh(\.exe)?$/);
    expect(command.args).toEqual(
      await expectedArgv('mac-host', 'PATH="$HOME/.local/bin:$PATH" muse login', keepalive(5, 1))
    );
  });
});

describe('opencode-console-wallet-service', () => {
  it('collects the wallet sample with interval 5 / count 1', async () => {
    const calls = captureExec(() => '');
    const service = new OpenCodeConsoleWalletService({
      ccsDir: home,
      readSource: async () =>
        JSON.stringify({ version: 1, platform: 'mac', sshHost: 'fixture-mac' }),
    });
    await service.get({ refresh: true });
    expect(sshCalls(calls)).toHaveLength(1);
    expect(sshCalls(calls)[0]!.args).toEqual(
      await expectedArgv(
        'fixture-mac',
        'exec "$HOME/.ccs/opencode-usage-bridge/native-host/launch-host.sh" --collect',
        keepalive(5, 1)
      )
    );
  });
});

describe('usage-failover-transport', () => {
  it('builds the private helper invocation with interval 5 / count 3', async () => {
    const invocation = await privateUsageInvocation(
      { host: 'nas1', platform: 'ubuntu', sshHost: 'nas1-agent' },
      { version: 1, action: 'inventory', provider: 'claude' },
      'helper-source'
    );
    expect(invocation.executable).toMatch(/(^|[\/])ssh(\.exe)?$/);
    expect(invocation.args).toEqual(
      await expectedArgv('nas1-agent', usageHelperCommand('ubuntu'), keepalive(5, 3))
    );
  });
});

describe('status-collectors', () => {
  const captureRun = (captured: Array<{ file: string; args: string[] }>): CommandRunner => {
    return async (file, args) => {
      captured.push({ file, args: [...args] });
      return { code: 1, stdout: '', timedOut: false, missing: false };
    };
  };

  it('probes a remote machine with interval 5 / count 2', async () => {
    const captured: Array<{ file: string; args: string[] }> = [];
    expect(readRemoteMachine('nas1', captureRun(captured))).rejects.toThrow();
    expect(captured).toHaveLength(1);
    expect(captured[0]!.file).toMatch(/(^|[\/])ssh(\.exe)?$/);
    expect(captured[0]!.args).toEqual(
      await expectedArgv('nas1-agent', machineProbeCommand(REMOTE_MACHINES.nas1), keepalive(5, 2))
    );
  });

  it('reads remote pins with interval 5 / count 2', async () => {
    const captured: Array<{ file: string; args: string[] }> = [];
    expect(readRemotePins('nas1', captureRun(captured))).rejects.toThrow();
    expect(captured).toHaveLength(1);
    expect(captured[0]!.file).toMatch(/(^|[\/])ssh(\.exe)?$/);
    expect(captured[0]!.args).toEqual(
      await expectedArgv('nas1-agent', remotePinsCommand(REMOTE_MACHINES.nas1), keepalive(5, 2))
    );
  });
});
