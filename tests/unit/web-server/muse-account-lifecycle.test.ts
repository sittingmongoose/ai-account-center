/**
 * Muse Code Sign in again (`muse login` on the Mac) through the sign-in job
 * runner, behind `CCS_MUSE_SIGNIN=on` (CLI behaviour verified read-only from
 * --help, docs and the binary's strings; never logged in). No live login, no
 * ssh to a real host: the ssh transport is faked.
 */
import { describe, expect, it } from 'bun:test';
import { SignInJobStopped } from '../../../src/web-server/services/signin-jobs';
import {
  MUSE_DEVICE_AUTH_ORIGINS,
  MUSE_DEVICE_CODE_TIMEOUT_MS,
  museSignInEnabled,
  MuseAccountLifecycle,
} from '../../../src/web-server/services/muse-account-lifecycle';
import type { RegistryAccount } from '../../../src/web-server/services/account-registry-v2';

function entry(over: Partial<RegistryAccount> = {}): RegistryAccount {
  return {
    id: 'muse:usage',
    provider: 'muse',
    platform: 'mac',
    sshHost: 'mac-host',
    label: null,
    credential: { kind: 'discover' },
    createdAt: '2026-10-02T08:00:00Z',
    createdBy: 'migration',
    ...over,
  };
}

describe('muse sign-in flag', () => {
  it('is off by default and on with CCS_MUSE_SIGNIN=on only', () => {
    expect(museSignInEnabled({})).toBe(false);
    expect(museSignInEnabled({ CCS_MUSE_SIGNIN: 'on' })).toBe(true);
    expect(museSignInEnabled({ CCS_MUSE_SIGNIN: 'off' })).toBe(false);
    expect(museSignInEnabled({ CCS_MUSE_SIGNIN: '1' })).toBe(false);
  });
});

describe('muse sign-in flow', () => {
  it('runs muse login on the Mac over ssh with the allowlist and a 15-minute timeout', async () => {
    const lifecycle = new MuseAccountLifecycle({ enabled: true });
    const spec = lifecycle.signInAgainFlow(entry());
    expect(spec.provider).toBe('muse');
    expect(spec.kind).toBe('device-code');
    expect(spec.mode).toBe('signin-again');
    expect(spec.accountId).toBe('muse:usage');
    expect(spec.platform).toBe('mac');
    expect([...spec.allowedOrigins]).toEqual([...MUSE_DEVICE_AUTH_ORIGINS]);
    expect(spec.timeoutMs).toBe(MUSE_DEVICE_CODE_TIMEOUT_MS);
    const command = await spec.prepare('job_0123456789abcdef');
    expect(command.file).toBe('ssh');
    expect(command.args).toEqual([
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
      'mac-host',
      'PATH="$HOME/.local/bin:$PATH" muse login',
    ]);
    expect(command.pty).toBe(false);
    expect(command.env.TERM).toBe('dumb');
    await spec.cleanup('job_0123456789abcdef');
  });

  it('runs a config-home account in its own XDG home with a validated id', async () => {
    const lifecycle = new MuseAccountLifecycle({ enabled: true });
    const spec = lifecycle.signInAgainFlow(
      entry({
        id: 'muse:acct:9f2c41d0',
        credential: { kind: 'config-home', homeId: '9f2c41d0' },
      })
    );
    const command = await spec.prepare('job_0123456789abcdef');
    expect(command.args.at(-1)).toBe(
      'env XDG_CONFIG_HOME="$HOME/.ccs/muse-homes/9f2c41d0" PATH="$HOME/.local/bin:$PATH" muse login'
    );
  });

  it('refuses while off, off-Mac, without an alias, or with a bad credential', () => {
    const off = new MuseAccountLifecycle({ enabled: false });
    expect(() => off.signInAgainFlow(entry())).toThrow('not_implemented');
    const on = new MuseAccountLifecycle({ enabled: true });
    expect(() => on.signInAgainFlow(entry({ platform: 'ubuntu' }))).toThrow('not_configured');
    expect(() => on.signInAgainFlow(entry({ sshHost: null }))).toThrow('not_configured');
    expect(() => on.signInAgainFlow(entry({ sshHost: 'evil;id' }))).toThrow('not_configured');
    expect(() =>
      on.signInAgainFlow(entry({ credential: { kind: 'aac-key', keyId: '9f2c41d0' } as never }))
    ).toThrow('not_configured');
  });

  it('verifies only the login email over ssh; tokens never cross', async () => {
    const seen: Array<[string, string, string]> = [];
    const lifecycle = new MuseAccountLifecycle({
      enabled: true,
      ssh: async (host, command, input) => {
        seen.push([host, command, input]);
        expect(command).toContain('user_email');
        expect(command).not.toContain('access_token');
        return '  muse-user@example.test\n';
      },
    });
    const spec = lifecycle.signInAgainFlow(entry());
    const result = await spec.complete('job_1', { stopped: () => false });
    expect(result).toEqual({
      accountId: 'muse:usage',
      email: 'muse-user@example.test',
      plan: null,
    });
    expect(seen).toHaveLength(1);
    expect(seen[0][0]).toBe('mac-host');
  });

  it('fails write_failed when the login email is missing or unreadable', async () => {
    for (const answer of ['', 'not-an-email', 'x'.repeat(300)]) {
      const lifecycle = new MuseAccountLifecycle({
        enabled: true,
        ssh: async () => answer,
      });
      const spec = lifecycle.signInAgainFlow(entry());
      await expect(spec.complete('job_1', { stopped: () => false })).rejects.toMatchObject({
        code: 'write_failed',
      });
    }
    const failing = new MuseAccountLifecycle({
      enabled: true,
      ssh: async () => {
        throw new Error('ssh failed');
      },
    });
    await expect(
      failing.signInAgainFlow(entry()).complete('job_1', { stopped: () => false })
    ).rejects.toMatchObject({ code: 'write_failed' });
  });

  it('stops before verifying when the job was stopped', async () => {
    let calls = 0;
    const lifecycle = new MuseAccountLifecycle({
      enabled: true,
      ssh: async () => {
        calls++;
        return 'a@b.test';
      },
    });
    const spec = lifecycle.signInAgainFlow(entry());
    await expect(spec.complete('job_1', { stopped: () => true })).rejects.toBeInstanceOf(
      SignInJobStopped
    );
    expect(calls).toBe(0);
  });
});
