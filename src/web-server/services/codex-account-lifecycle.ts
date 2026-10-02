import { createHash } from 'crypto';
import fs from 'fs/promises';
import path from 'path';
import * as lockfile from 'proper-lockfile';
import { CodexProfileRegistry } from '../../codex-auth/codex-profile-registry';
import {
  getCodexInstancesDir,
  getSharedCodexConfigPath,
} from '../../codex-auth/codex-profile-paths';
import { getCodexProfileNameError } from '../../codex-auth/types';
import {
  acquireCodexActivationLock,
  getActivationCodexHome,
} from '../../codex-auth/codex-activation-lock';
import {
  CodexProfileRemovalError,
  removeCodexProfileUnderLock,
} from '../../codex-auth/codex-profile-removal';
import {
  getCodexAuthProfilesSummary,
  invalidateCodexAuthProfilesCache,
} from '../../codex-auth/codex-auth-dashboard-service';
import { detectCodexCli } from '../../targets/codex-detector';
import { stateFingerprint } from './account-confirmations';
import { SignInJobError, type SignInFlowSpec } from './signin-jobs';
import { readCodexLogin, type CodexLoginIdentity } from './codex-login-file';

export { readCodexLogin, type CodexLoginIdentity } from './codex-login-file';

/**
 * Codex Add, Sign in again and Remove (CONTRACT-registry-lifecycle 6.2, 6.3,
 * 6.6 and 6.7).
 *
 * - Sign-ins run `codex login --device-auth` on a PTY with CODEX_HOME set to a
 *   private staging folder `codex-instances/.staging-<jobId>` (0700, with the
 *   shared config.toml linked when it exists). The native `~/.codex` login is
 *   never written: only the activation lock file lives there.
 * - Add refuses an email already saved in another profile; Sign in again
 *   refuses a different email or ChatGPT account. Then, under the activation
 *   lock, Add renames the staging folder into place and registers it, and Sign
 *   in again replaces the profile's auth.json atomically.
 * - Remove uses the same staged delete as `codex-auth remove`, and refuses the
 *   live active profile, the saved default while others exist, and the last
 *   profile.
 */
export const CODEX_DEVICE_AUTH_ORIGINS: readonly string[] = Object.freeze([
  'https://auth.openai.com',
]);
export const CODEX_DEVICE_CODE_TIMEOUT_MS = 15 * 60_000;
export const CODEX_STAGING_PREFIX = '.staging-';
const STAGING_MAX_AGE_MS = 60 * 60_000;
const CLI_CACHE_MS = 5 * 60_000;
/** Environment the CLI may see; nothing else from the server's environment. */
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
];

export type CodexRemoveRefusal =
  | 'account_active'
  | 'account_default'
  | 'last_account'
  | 'activation_running'
  | 'signin_running';

export class CodexLifecycleError extends Error {
  constructor(
    readonly code:
      | CodexRemoveRefusal
      | 'unknown_account'
      | 'remove_failed'
      | 'id_in_use'
      | 'tool_missing'
  ) {
    super(code);
    this.name = 'CodexLifecycleError';
  }
}

export interface CodexLifecycleDeps {
  /** The native Codex home: the activation lock and the live login. */
  codexHome?: string;
  sharedConfigPath?: string;
  registry?: () => CodexProfileRegistry;
  instancesDir?: () => string;
  codexCli?: () => string | null;
  /** The profile name whose saved login is the live native login, if any. */
  activeProfile?: () => Promise<string | null>;
  activationLocked?: () => Promise<boolean>;
  env?: NodeJS.ProcessEnv;
  now?: () => number;
}

function same(a: string | null | undefined, b: string | null | undefined): boolean {
  return typeof a === 'string' && typeof b === 'string' && a.toLowerCase() === b.toLowerCase();
}

export class CodexAccountLifecycle {
  private cli: { value: string | null; at: number } | null = null;

  constructor(private readonly deps: CodexLifecycleDeps = {}) {}

  private now(): number {
    return (this.deps.now ?? Date.now)();
  }

  private codexHome(): string {
    return path.resolve(this.deps.codexHome ?? getActivationCodexHome());
  }

  registry(): CodexProfileRegistry {
    return this.deps.registry?.() ?? new CodexProfileRegistry();
  }

  private instancesDir(): string {
    return path.resolve(this.deps.instancesDir?.() ?? getCodexInstancesDir());
  }

  private profileDir(name: string): string {
    return path.join(this.instancesDir(), name);
  }

  private stagingDir(jobId: string): string {
    return path.join(this.instancesDir(), `${CODEX_STAGING_PREFIX}${jobId}`);
  }

  /** The codex binary, cached for five minutes (detection runs `which`). */
  codexCli(): string | null {
    if (this.deps.codexCli) return this.deps.codexCli();
    const now = this.now();
    if (!this.cli || now - this.cli.at >= CLI_CACHE_MS) {
      let value: string | null = null;
      try {
        value = detectCodexCli();
      } catch {
        value = null;
      }
      this.cli = { value, at: now };
    }
    return this.cli.value;
  }

  async activeProfile(): Promise<string | null> {
    if (this.deps.activeProfile) return this.deps.activeProfile();
    const summary = await getCodexAuthProfilesSummary(this.codexHome());
    return summary.activated?.name ?? null;
  }

  async activationRunning(): Promise<boolean> {
    if (this.deps.activationLocked) return this.deps.activationLocked();
    try {
      return await lockfile.check(this.codexHome(), {
        realpath: false,
        lockfilePath: path.join(this.codexHome(), '.ccs-activation.lock'),
        stale: 120_000,
      });
    } catch {
      return false;
    }
  }

  nameError(name: unknown): boolean {
    return typeof name !== 'string' || getCodexProfileNameError(name) !== null;
  }

  async nameInUse(name: string): Promise<boolean> {
    if (this.registry().hasProfile(name)) return true;
    try {
      await fs.lstat(this.profileDir(name));
      return true;
    } catch (error) {
      return (error as NodeJS.ErrnoException).code !== 'ENOENT';
    }
  }

  private async otherProfileWithEmail(email: string, except: string | null): Promise<boolean> {
    const registry = this.registry();
    for (const name of registry.listProfiles()) {
      if (name === except) continue;
      const saved = await readCodexLogin(path.join(this.profileDir(name), 'auth.json'));
      if (same(saved?.email ?? registry.getProfile(name).email ?? null, email)) return true;
    }
    return false;
  }

  private env(staging: string): Record<string, string> {
    const source = this.deps.env ?? process.env;
    const env: Record<string, string> = {};
    for (const key of PASSED_ENV) {
      const value = source[key];
      if (typeof value === 'string') env[key] = value;
    }
    return { ...env, TERM: 'dumb', NO_COLOR: '1', CODEX_HOME: staging };
  }

  private async prepareStaging(jobId: string) {
    const cli = this.codexCli();
    if (!cli) throw new SignInJobError('tool_missing');
    const staging = this.stagingDir(jobId);
    await fs.mkdir(this.instancesDir(), { recursive: true, mode: 0o700 });
    await fs.mkdir(staging, { mode: 0o700 });
    await fs.chmod(staging, 0o700);
    const shared = path.resolve(this.deps.sharedConfigPath ?? getSharedCodexConfigPath());
    try {
      if ((await fs.stat(shared)).isFile())
        await fs.symlink(shared, path.join(staging, 'config.toml'));
    } catch {
      // No shared config yet: Codex signs in without one, and nothing is created in ~/.codex.
    }
    return {
      file: cli,
      args: ['login', '--device-auth'],
      env: this.env(staging),
      pty: true,
    };
  }

  private flow(
    mode: 'add' | 'signin-again',
    name: string,
    complete: (jobId: string) => Promise<CodexLoginIdentity>
  ): SignInFlowSpec {
    return {
      provider: 'codex',
      kind: 'device-code',
      mode,
      accountId: mode === 'signin-again' ? `codex:${name}` : null,
      profileName: name,
      platform: 'ubuntu',
      allowedOrigins: CODEX_DEVICE_AUTH_ORIGINS,
      timeoutMs: CODEX_DEVICE_CODE_TIMEOUT_MS,
      prepare: (jobId) => this.prepareStaging(jobId),
      complete: async (jobId) => {
        const login = await complete(jobId);
        invalidateCodexAuthProfilesCache();
        return { accountId: `codex:${name}`, email: login.email, plan: login.plan };
      },
      cleanup: (jobId) => fs.rm(this.stagingDir(jobId), { recursive: true, force: true }),
    };
  }

  private async underActivationLock<T>(task: () => Promise<T>): Promise<T> {
    const home = this.codexHome();
    let release: (() => Promise<void>) | undefined;
    try {
      await fs.mkdir(home, { recursive: true, mode: 0o700 });
      release = await acquireCodexActivationLock(home);
    } catch {
      throw new SignInJobError('write_failed');
    }
    try {
      return await task();
    } finally {
      await release?.().catch(() => undefined);
    }
  }

  /** Add: device-code sign-in into staging, then rename into `codex-instances/<name>`. */
  addFlow(name: string): SignInFlowSpec {
    return this.flow('add', name, async (jobId) => {
      const staging = this.stagingDir(jobId);
      const login = await readCodexLogin(path.join(staging, 'auth.json'));
      if (!login) throw new SignInJobError('write_failed');
      if (await this.otherProfileWithEmail(login.email, null)) {
        throw new SignInJobError('duplicate_identity');
      }
      return this.underActivationLock(async () => {
        if (await this.nameInUse(name)) throw new SignInJobError('write_failed');
        const target = this.profileDir(name);
        await fs.rename(staging, target);
        try {
          this.registry().createProfile(name, {
            created: new Date(this.now()).toISOString(),
            last_used: null,
            email: login.email,
            plan_type: login.plan,
            ...(login.accountId ? { account_id: login.accountId } : {}),
          });
        } catch {
          await fs.rename(target, staging).catch(() => undefined);
          throw new SignInJobError('write_failed');
        }
        return login;
      });
    });
  }

  /** Sign in again: same email and ChatGPT account only; the saved auth.json is replaced atomically. */
  signInAgainFlow(name: string): SignInFlowSpec {
    return this.flow('signin-again', name, async (jobId) => {
      const login = await readCodexLogin(path.join(this.stagingDir(jobId), 'auth.json'));
      if (!login) throw new SignInJobError('write_failed');
      const registry = this.registry();
      if (!registry.hasProfile(name)) throw new SignInJobError('write_failed');
      const authPath = path.join(this.profileDir(name), 'auth.json');
      const saved = await readCodexLogin(authPath);
      const meta = registry.getProfile(name);
      const knownEmail = saved?.email ?? meta.email ?? null;
      const knownAccount = saved?.accountId ?? meta.account_id ?? null;
      if (
        (knownEmail !== null && !same(knownEmail, login.email)) ||
        (knownAccount !== null && login.accountId !== null && knownAccount !== login.accountId)
      ) {
        throw new SignInJobError('identity_mismatch');
      }
      if (knownEmail === null && (await this.otherProfileWithEmail(login.email, name))) {
        throw new SignInJobError('duplicate_identity');
      }
      return this.underActivationLock(async () => {
        // Activation copies the live login back into the active profile, which would undo this.
        if ((await this.activeProfile()) === name) throw new SignInJobError('write_failed');
        const content = await fs.readFile(path.join(this.stagingDir(jobId), 'auth.json'));
        const temporary = `${authPath}.tmp.${process.pid}.${jobId}`;
        try {
          await fs.writeFile(temporary, content, { mode: 0o600, flag: 'wx' });
          await fs.chmod(temporary, 0o600);
          await fs.rename(temporary, authPath);
        } catch {
          await fs.rm(temporary, { force: true }).catch(() => undefined);
          throw new SignInJobError('write_failed');
        }
        try {
          registry.updateProfile(name, {
            email: login.email,
            plan_type: login.plan,
            ...(login.accountId ? { account_id: login.accountId } : {}),
          });
        } catch {
          // The login is installed; the cached metadata refreshes on the next read.
        }
        return login;
      });
    });
  }

  /** The first refusal a remove would hit now, or null. */
  async removeRefusal(name: string, signinRunning: boolean): Promise<CodexRemoveRefusal | null> {
    const registry = this.registry();
    const profiles = registry.listProfiles();
    if ((await this.activeProfile()) === name) return 'account_active';
    if (registry.getDefault() === name && profiles.length > 1) return 'account_default';
    if (profiles.length <= 1) return 'last_account';
    if (await this.activationRunning()) return 'activation_running';
    if (signinRunning) return 'signin_running';
    return null;
  }

  /** The reviewed state: the profile entry and its saved login, the live active profile and the default. */
  async removeFingerprint(name: string): Promise<string> {
    const registry = this.registry();
    let authHash: string | null = null;
    try {
      authHash = createHash('sha256')
        .update(await fs.readFile(path.join(this.profileDir(name), 'auth.json')))
        .digest('hex');
    } catch {
      authHash = null;
    }
    return stateFingerprint({
      entry: registry.hasProfile(name) ? registry.getProfile(name) : null,
      authHash,
      active: await this.activeProfile(),
      default: registry.getDefault(),
      profiles: registry.listProfiles().sort(),
    });
  }

  /** The shared staged delete, under the activation lock. */
  async remove(name: string): Promise<void> {
    const home = this.codexHome();
    let release: (() => Promise<void>) | undefined;
    try {
      await fs.mkdir(home, { recursive: true, mode: 0o700 });
      release = await acquireCodexActivationLock(home);
    } catch {
      throw new CodexLifecycleError('activation_running');
    }
    try {
      const registry = this.registry();
      if (!registry.hasProfile(name)) throw new CodexLifecycleError('unknown_account');
      if (registry.listProfiles().length <= 1) throw new CodexLifecycleError('last_account');
      await removeCodexProfileUnderLock(registry, name, home, { warn: () => undefined });
    } catch (error) {
      if (error instanceof CodexLifecycleError) throw error;
      if (error instanceof CodexProfileRemovalError) {
        throw new CodexLifecycleError(
          error.reason === 'account_active'
            ? 'account_active'
            : error.reason === 'account_default'
              ? 'account_default'
              : error.reason === 'not_found'
                ? 'unknown_account'
                : 'remove_failed'
        );
      }
      throw new CodexLifecycleError('remove_failed');
    } finally {
      await release?.().catch(() => undefined);
      invalidateCodexAuthProfilesCache();
    }
  }

  /** Startup: staging folders older than one hour are removed (jobs never resume). */
  async sweepStaging(): Promise<number> {
    let names: string[];
    try {
      names = await fs.readdir(this.instancesDir());
    } catch {
      return 0;
    }
    let removed = 0;
    for (const name of names) {
      if (!/^\.staging-job_[a-f0-9]{16}$/.test(name)) continue;
      const target = path.join(this.instancesDir(), name);
      try {
        const stat = await fs.lstat(target);
        if (!stat.isDirectory() || this.now() - stat.mtimeMs < STAGING_MAX_AGE_MS) continue;
        await fs.rm(target, { recursive: true, force: true });
        removed += 1;
      } catch {
        /* Retried at the next start. */
      }
    }
    return removed;
  }
}
