import { isSafeUsageSshAlias } from './additional-usage-transport';
import type { RegistryAccount } from './account-registry-v2';
import { runClaudeHostOverSsh } from './claude-host-transport';
import {
  SignInJobError,
  SignInJobStopped,
  type SignInCompleteControl,
  type SignInFlowSpec,
} from './signin-jobs';

/**
 * Muse Code Sign in again (CONTRACT-registry-lifecycle 6.3): `muse login` on
 * the Mac in the account's existing config home, run through the supervised
 * sign-in job runner, with the URL and code parsed against the allowlist.
 *
 * UNVERIFIED: the Mac CLI path and its device-code output have never been
 * observed live. This lane is built fully but served only when
 * `CCS_MUSE_SIGNIN=on` (see `museSignInEnabled()`); by default the provider
 * reads `not_implemented` and Sign in again answers the same. The live step to
 * verify it is in status/MUSE-LIFECYCLE.md ("Muse live-enable step"): run
 * `muse login` on the Mac, confirm the URL origin is on
 * `MUSE_DEVICE_AUTH_ORIGINS` and the code matches the job parser, then set the
 * flag for the service.
 *
 * - `discover` accounts sign in with the Mac's default config home
 *   (`~/.config/muse/auth.json`). `config-home` accounts use
 *   `~/.ccs/muse-homes/<homeId>` via `XDG_CONFIG_HOME` (no account uses it in
 *   v1; the collector does not read it yet).
 * - The job runs `ssh -t <alias> <fixed remote command>` on a PTY; the remote
 *   command is `muse login`, optionally prefixed with a fixed `env`
 *   assignment for a validated hex home id. Nothing else crosses.
 * - Success is verified by reading only the login email over ssh (a fixed
 *   Python program that prints `providers.meta.user_email`); tokens never
 *   leave the Mac. A missing or unreadable login fails `write_failed`.
 * - Single account in v1: any verified email is accepted (no
 *   `identity_mismatch`; a new login replaces the one slot).
 */
export const MUSE_DEVICE_AUTH_ORIGINS: readonly string[] = Object.freeze([
  'https://dev.meta.ai',
  'https://auth.meta.com',
  'https://www.meta.ai',
]);
export const MUSE_DEVICE_CODE_TIMEOUT_MS = 15 * 60_000;
const MUSE_HOMES_DIR = '.ccs/muse-homes';
const HOME_ID = /^[a-f0-9]{8}$/;

export const MUSE_SIGNIN_ENABLED = false;
export function museSignInEnabled(
  environment: Record<string, string | undefined> = process.env
): boolean {
  return MUSE_SIGNIN_ENABLED || environment.CCS_MUSE_SIGNIN === 'on';
}

export type MuseLifecycleCode = 'not_implemented' | 'not_configured' | 'unknown_account';

export class MuseLifecycleError extends Error {
  constructor(readonly code: MuseLifecycleCode) {
    super(code);
    this.name = 'MuseLifecycleError';
  }
}

export type MuseSshRunner = (sshHost: string, command: string, input: string) => Promise<string>;

export interface MuseLifecycleDeps {
  enabled: boolean;
  ssh?: MuseSshRunner;
}

const SSH_ARGV = [
  '-t',
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
] as const;

const PASSED_ENV = [
  'PATH',
  'HOME',
  'USER',
  'LOGNAME',
  'LANG',
  'LC_ALL',
  'TMPDIR',
  'HTTPS_PROXY',
  'HTTP_PROXY',
  'NO_PROXY',
  'https_proxy',
  'http_proxy',
  'no_proxy',
  'SSL_CERT_FILE',
  'SSL_CERT_DIR',
] as const;

/** Fixed remote Python: print only the Muse login email, never tokens. */
const EMAIL_PROGRAM =
  'import json,os,sys; p=os.path.join(os.environ.get("XDG_CONFIG_HOME") or os.path.join(os.path.expanduser("~"), ".config"), "muse", "auth.json"); ' +
  'v=json.load(open(p)); m=(v.get("providers") or {}).get("meta") or {}; e=m.get("user_email"); ' +
  'sys.stdout.write(e if isinstance(e, str) and e else "")';

function remoteLoginCommand(homeId: string | null): string {
  if (homeId === null) return 'muse login';
  return `env XDG_CONFIG_HOME="$HOME/${MUSE_HOMES_DIR}/${homeId}" muse login`;
}

function remoteEmailCommand(homeId: string | null): string {
  const prefix = homeId === null ? '' : `XDG_CONFIG_HOME="$HOME/${MUSE_HOMES_DIR}/${homeId}" `;
  return `${prefix}/usr/bin/python3 -c '${EMAIL_PROGRAM.replace(/'/g, "'\"'\"'")}'`;
}

export class MuseAccountLifecycle {
  constructor(private readonly deps: MuseLifecycleDeps) {}

  get enabled(): boolean {
    return this.deps.enabled;
  }

  private ssh(): MuseSshRunner {
    return this.deps.ssh ?? runClaudeHostOverSsh;
  }

  /** The account's config home id, or null for the Mac default. */
  private homeId(entry: RegistryAccount): string | null {
    if (entry.credential.kind === 'config-home') {
      if (!HOME_ID.test(entry.credential.homeId)) throw new MuseLifecycleError('not_configured');
      return entry.credential.homeId;
    }
    if (entry.credential.kind !== 'discover') throw new MuseLifecycleError('not_configured');
    return null;
  }

  signInAgainFlow(entry: RegistryAccount): SignInFlowSpec {
    if (!this.deps.enabled) throw new MuseLifecycleError('not_implemented');
    if (entry.provider !== 'muse') throw new MuseLifecycleError('unknown_account');
    if (entry.platform !== 'mac' || entry.sshHost === null || !isSafeUsageSshAlias(entry.sshHost)) {
      throw new MuseLifecycleError('not_configured');
    }
    const sshHost = entry.sshHost;
    const homeId = this.homeId(entry);
    const accountId = entry.id;
    const remoteLogin = remoteLoginCommand(homeId);
    const remoteEmail = remoteEmailCommand(homeId);
    const ssh = this.ssh();
    return {
      provider: 'muse',
      kind: 'device-code',
      mode: 'signin-again',
      accountId,
      profileName: accountId,
      platform: 'mac',
      allowedOrigins: MUSE_DEVICE_AUTH_ORIGINS,
      timeoutMs: MUSE_DEVICE_CODE_TIMEOUT_MS,
      prepare: async () => {
        const env: Record<string, string> = { TERM: 'dumb', NO_COLOR: '1' };
        for (const name of PASSED_ENV) {
          const value = process.env[name];
          if (typeof value === 'string' && value) env[name] = value;
        }
        return {
          file: 'ssh',
          args: [...SSH_ARGV, '--', sshHost, remoteLogin],
          env,
          pty: true,
        };
      },
      complete: async (_jobId, control: SignInCompleteControl) => {
        if (control.stopped()) throw new SignInJobStopped();
        let email = '';
        try {
          email = (await ssh(sshHost, remoteEmail, '')).trim();
        } catch {
          throw new SignInJobError('write_failed');
        }
        if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email) || email.length > 254) {
          throw new SignInJobError('write_failed');
        }
        return { accountId, email, plan: null };
      },
      cleanup: async () => undefined,
    };
  }
}
