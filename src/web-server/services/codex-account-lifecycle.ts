import { createHash } from 'crypto';
import fs from 'fs/promises';
import { getCcsDir } from '../../utils/config-manager';
import {
  canonicalLeaseRoot,
  signInStaging,
  writeProviderFile,
} from '../../shared/private-file-lease';
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
  savedLoginIsLive,
} from '../../codex-auth/codex-profile-removal';
import {
  getCodexAuthProfilesSummary,
  invalidateCodexAuthProfilesCache,
} from '../../codex-auth/codex-auth-dashboard-service';
import { detectCodexCli } from '../../targets/codex-detector';
import { stateFingerprint } from './account-confirmations';
import {
  SignInJobError,
  SignInJobStopped,
  type SignInCompleteControl,
  type SignInFlowSpec,
} from './signin-jobs';
import {
  readCodexLogin,
  readCodexLoginIdentity,
  readCodexLoginSnapshot,
  sameCodexIdentity,
  sameText as same,
  type CodexLoginIdentity,
} from './codex-login-file';

export { readCodexLogin, type CodexLoginIdentity } from './codex-login-file';

/**
 * Codex Add, Sign in again and Remove (CONTRACT-registry-lifecycle 6.2, 6.3,
 * 6.6 and 6.7).
 *
 * - Sign-ins run `codex login --device-auth` on a PTY with CODEX_HOME set to a
 *   private staging folder `codex-instances/.staging-<jobId>` (0700, with the
 *   shared config.toml linked when it exists). The CLI never writes the native
 *   `~/.codex` login itself.
 * - Add refuses an email already saved in another profile. Sign in again
 *   applies Codex's activation identity rules (codex-activation-identity.ts):
 *   the new login must carry a workspace claim, keep the known ChatGPT
 *   account, and keep any known person (user id, or subject plus issuer); the
 *   email must match too. Then, under the activation lock, Add renames the
 *   staging folder into place and registers it, and Sign in again replaces the
 *   profile's auth.json atomically. When the live native login (`~/.codex`) is
 *   this same account, read fresh under the lock, Sign in again replaces it
 *   too, both or neither: switching was retired (2026-11-13), so the live
 *   account can only be signed in again in place. A live login of another
 *   account, or one that cannot be read, is never written.
 * - A stop requested by the job runner (cancel, shutdown) is checked under the
 *   activation lock before anything is committed.
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
    return this.deps.registry?.() ?? new CodexProfileRegistry(undefined, true);
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
    return (await this.liveLogin()).name;
  }

  /** The live native login as the summary reads it: the profile it names, and its email. */
  private async liveLogin(): Promise<{ name: string | null; email: string | null }> {
    if (this.deps.activeProfile) return { name: await this.deps.activeProfile(), email: null };
    const summary = await getCodexAuthProfilesSummary(this.codexHome());
    return { name: summary.activated?.name ?? null, email: summary.activated?.email ?? null };
  }

  /**
   * The summary names the live profile by workspace and principal only. A refusal also
   * counts a saved login the removal guard would call live (the email decides when a
   * binding cannot be read), so these checks never get looser than that guard.
   */
  async isLiveProfile(name: string): Promise<boolean> {
    if ((await this.activeProfile()) === name) return true;
    return savedLoginIsLive(
      path.join(this.codexHome(), 'auth.json'),
      path.join(this.profileDir(name), 'auth.json')
    );
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
    const shared = path.resolve(this.deps.sharedConfigPath ?? getSharedCodexConfigPath());
    const root = this.nativeStagingRoot();
    if (
      !root ||
      signInStaging(root, 'codex', ['prepare', path.basename(staging), shared]) === undefined
    ) {
      await fs.mkdir(this.instancesDir(), { recursive: true, mode: 0o700 });
      await fs.mkdir(staging, { mode: 0o700 });
      await fs.chmod(staging, 0o700);
      try {
        if ((await fs.stat(shared)).isFile())
          await fs.symlink(shared, path.join(staging, 'config.toml'));
      } catch {
        // No shared config yet: Codex signs in without one.
      }
    }
    return {
      file: cli,
      args: ['login', '--device-auth'],
      env: this.env(staging),
      pty: true,
    };
  }

  private nativeStagingRoot(): string | null {
    const root = canonicalLeaseRoot(getCcsDir());
    const instances = path.resolve(this.instancesDir());
    return instances === path.join(path.resolve(getCcsDir()), 'codex-instances') ? root : null;
  }

  private async cleanupStaging(jobId: string): Promise<void> {
    const target = this.stagingDir(jobId);
    const root = this.nativeStagingRoot();
    if (root && signInStaging(root, 'codex', ['cleanup', path.basename(target)]) !== undefined)
      return;
    await fs.rm(target, { recursive: true, force: true });
  }

  private flow(
    mode: 'add' | 'signin-again',
    name: string,
    complete: (jobId: string, control: SignInCompleteControl) => Promise<CodexLoginIdentity>
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
      complete: async (jobId, control) => {
        const login = await complete(jobId, control);
        invalidateCodexAuthProfilesCache();
        return { accountId: `codex:${name}`, email: login.email, plan: login.plan };
      },
      cleanup: (jobId) => this.cleanupStaging(jobId),
    };
  }

  /** Runs `task` under the activation lock, unless the job was stopped first. */
  private async commitUnderLock<T>(
    control: SignInCompleteControl,
    task: () => Promise<T>
  ): Promise<T> {
    return this.underActivationLock(async () => {
      if (control.stopped()) throw new SignInJobStopped();
      return task();
    });
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

  private moveProfile(from: string, to: string): boolean {
    const root = canonicalLeaseRoot(getCcsDir());
    const instances = path.join(root, 'codex-instances');
    if (
      canonicalLeaseRoot(path.dirname(from)) !== instances ||
      canonicalLeaseRoot(path.dirname(to)) !== instances
    )
      return false;
    const home = canonicalLeaseRoot(this.codexHome());
    return (
      writeProviderFile(
        root,
        path.join(home, '.ccs-activation.lock'),
        ['codex-move', home, path.basename(from), path.basename(to)],
        ''
      ) === 'committed'
    );
  }

  /** Add: device-code sign-in into staging, then rename into `codex-instances/<name>`. */
  addFlow(name: string): SignInFlowSpec {
    return this.flow('add', name, async (jobId, control) => {
      const staging = this.stagingDir(jobId);
      const login = await readCodexLogin(path.join(staging, 'auth.json'));
      if (!login) throw new SignInJobError('write_failed');
      return this.commitUnderLock(control, async () => {
        if (await this.otherProfileWithEmail(login.email, null)) {
          throw new SignInJobError('duplicate_identity');
        }
        if (await this.nameInUse(name)) throw new SignInJobError('write_failed');
        const target = this.profileDir(name);
        if (!this.moveProfile(staging, target)) await fs.rename(staging, target);
        try {
          this.registry().createProfile(name, {
            created: new Date(this.now()).toISOString(),
            last_used: null,
            email: login.email,
            plan_type: login.plan,
            ...(login.accountId ? { account_id: login.accountId } : {}),
          });
        } catch {
          try {
            if (!this.moveProfile(target, staging)) await fs.rename(target, staging);
          } catch {
            /* The job remains failed and the owned folder is preserved for recovery. */
          }
          throw new SignInJobError('write_failed');
        }
        return login;
      });
    });
  }

  /**
   * Sign in again: the same account and person only. The saved auth.json is replaced
   * atomically, and the live native login too when it is this same account.
   */
  signInAgainFlow(name: string): SignInFlowSpec {
    return this.flow('signin-again', name, async (jobId, control) => {
      const staged = path.join(this.stagingDir(jobId), 'auth.json');
      const snapshot = await readCodexLoginSnapshot(staged);
      if (!snapshot) throw new SignInJobError('write_failed');
      const { identity: login, bytes: content } = snapshot;
      return this.commitUnderLock(control, async () => {
        const registry = this.registry();
        if (!registry.hasProfile(name)) throw new SignInJobError('write_failed');
        const authPath = path.join(this.profileDir(name), 'auth.json');
        const saved = await readCodexLogin(authPath);
        const meta = registry.getProfile(name);
        if (!sameCodexIdentity(saved, meta, login)) throw new SignInJobError('identity_mismatch');
        if (await this.otherProfileWithEmail(login.email, name)) {
          throw new SignInJobError('duplicate_identity');
        }
        const liveAuth = await this.liveLoginToRenew(name, authPath, login);
        await this.installLogin(`${process.pid}.${jobId}`, content, authPath, liveAuth);
        try {
          registry.updateProfile(name, {
            last_used: new Date(this.now()).toISOString(),
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

  /**
   * The live native login's path when it holds this profile's account, read fresh under
   * the lock (no cached answer counts), or null to leave it alone: no live login, one
   * whose id token cannot be read, or another account. It is this account when the
   * summary names this profile (workspace and principal), when the removal guard calls
   * the saved login live, or when it holds the new login's own identity. Whichever rule
   * names it, the new login must keep the live login's account and person, so a sign-in
   * never puts another identity into the live login.
   */
  private async liveLoginToRenew(
    name: string,
    authPath: string,
    fresh: CodexLoginIdentity
  ): Promise<string | null> {
    const liveAuth = path.join(this.codexHome(), 'auth.json');
    const live = await readCodexLoginIdentity(liveAuth);
    if (!live) return null;
    const keepsLive = sameCodexIdentity(live, {}, fresh);
    invalidateCodexAuthProfilesCache();
    const namesThis = (await this.activeProfile()) === name || savedLoginIsLive(liveAuth, authPath);
    if (!namesThis && !keepsLive) return null;
    if (!keepsLive) throw new SignInJobError('identity_mismatch');
    return liveAuth;
  }

  /**
   * Installs a new login into the profile and, when `liveAuth` is given, into the live
   * login, both or neither. Each file goes through a 0600 temporary file in its own
   * folder ('wx', so a planted file or link is never followed), synced, then renamed
   * into place. Every temporary file (and a copy of the profile's previous login) is
   * written before the first rename, so a full disk or a permission problem changes
   * nothing. If the live rename still fails, the previous profile login goes back; only
   * when that also fails does the job report that the profile was saved and the live
   * login was not.
   */
  private async installLogin(
    tag: string,
    content: Buffer,
    authPath: string,
    liveAuth: string | null
  ): Promise<void> {
    const root = canonicalLeaseRoot(getCcsDir());
    const profileFolder = path.dirname(authPath);
    if (canonicalLeaseRoot(path.dirname(profileFolder)) === path.join(root, 'codex-instances')) {
      const home = canonicalLeaseRoot(this.codexHome());
      try {
        const result = writeProviderFile(
          root,
          path.join(home, '.ccs-activation.lock'),
          ['codex-login', home, path.basename(profileFolder), liveAuth ? 'live' : 'saved'],
          content
        );
        if (result === 'committed') return;
        if (result === 'partial') throw new SignInJobError('live_login_not_updated');
      } catch (error) {
        if (error instanceof SignInJobError) throw error;
        throw new SignInJobError('write_failed');
      }
    }
    const temporaries: string[] = [];
    const stage = async (target: string, bytes: Buffer): Promise<string> => {
      const temporary = `${target}.tmp.${tag}.${temporaries.length}`;
      const handle = await fs.open(temporary, 'wx', 0o600);
      // Only a file this call created is ever cleaned up, never one planted at its name.
      temporaries.push(temporary);
      try {
        await handle.writeFile(bytes);
        await handle.chmod(0o600);
        await handle.sync();
      } finally {
        await handle.close();
      }
      return temporary;
    };
    const cleanup = () =>
      Promise.all(temporaries.map((file) => fs.rm(file, { force: true }).catch(() => undefined)));
    let liveTemporary: string | null = null;
    let restore: string | null = null;
    try {
      // Read before anything changes: an unreadable previous login cannot be put back.
      const previous = liveAuth ? await previousLogin(authPath) : null;
      const profileTemporary = await stage(authPath, content);
      if (liveAuth) {
        liveTemporary = await stage(liveAuth, content);
        if (previous) restore = await stage(authPath, previous);
      }
      await fs.rename(profileTemporary, authPath);
    } catch {
      await cleanup();
      throw new SignInJobError('write_failed');
    }
    if (!liveAuth || !liveTemporary) return;
    try {
      await fs.rename(liveTemporary, liveAuth);
    } catch {
      try {
        if (restore) await fs.rename(restore, authPath);
        else await fs.rm(authPath);
      } catch {
        await cleanup();
        throw new SignInJobError('live_login_not_updated');
      }
      await cleanup();
      throw new SignInJobError('write_failed');
    }
    await cleanup();
  }

  /** The first refusal a remove would hit now, or null. */
  async removeRefusal(name: string, signinRunning: boolean): Promise<CodexRemoveRefusal | null> {
    const registry = this.registry();
    const profiles = registry.listProfiles();
    if (await this.isLiveProfile(name)) return 'account_active';
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
      // The live login, not only the profile it names: a login whose binding cannot be
      // read names no profile, and switching it must still make a reviewed remove stale.
      active: await this.liveLogin(),
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
      await removeCodexProfileUnderLock(registry, name, home, {
        warn: () => undefined,
        move: (from, to) => this.moveProfile(from, to),
      });
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
    const root = this.nativeStagingRoot();
    if (root) {
      const removed = signInStaging(root, 'codex', ['sweep', String(this.now())]);
      if (removed !== undefined) return removed;
    }
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

/** A profile's saved login bytes, or null when there is none; any other read error throws. */
async function previousLogin(authPath: string): Promise<Buffer | null> {
  try {
    return await fs.readFile(authPath);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw error;
  }
}
