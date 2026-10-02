import * as fs from 'fs';
import * as path from 'path';
import { randomBytes } from 'crypto';
import { CodexProfileRegistry } from './codex-profile-registry';
import { resolveCodexProfileDir } from './codex-profile-paths';
import { decodeIdToken, hasStructurallyValidIdToken } from './decode-id-token';
import {
  decodeCodexActivationIdentity,
  matchesCodexActivationIdentity,
  type CodexActivationIdentity,
} from './codex-activation-identity';
import { getCodexProfileNameError } from './types';
import type { CodexAccountIdentity } from './types';
import { acquireCodexActivationLock, getActivationCodexHome } from './codex-activation-lock';
export { getActivationCodexHome } from './codex-activation-lock';
import {
  createCodexActivationRuntime,
  CodexActivationRuntimeError,
} from './codex-activation-runtime';
import { invalidateCodexAuthProfilesCache } from './codex-auth-dashboard-service';
import {
  codexAuthHash,
  consumeCodexActivationConfirmation,
  issueCodexActivationConfirmation,
  type CodexActivationConfirmation,
  type CodexActivationStopPlan,
} from './codex-activation-confirmation';

export type CodexActivationErrorCode =
  | 'busy'
  | 'confirmation_stale'
  | 'invalid_profile'
  | 'invalid_codex_home'
  | 'auth_read_failed'
  | 'auth_write_failed'
  | 'restart_failed'
  | 'verification_failed';

/** Contains only messages assembled from safe identity fields and operation names. */
export class CodexActivationError extends Error {
  readonly code: CodexActivationErrorCode;

  constructor(
    code: CodexActivationErrorCode,
    message: string,
    public readonly details?: {
      reason: 'activation_running' | 'running_processes' | 'unsupported_process';
      confirmation?: CodexActivationConfirmation;
    }
  ) {
    super(message);
    this.name = 'CodexActivationError';
    this.code = code;
  }
}

export interface CodexActivationRuntime {
  stop(approval?: CodexActivationStopPlan): Promise<void>;
  start(): Promise<void>;
  dispose?(): Promise<void>;
}

export interface CodexActivationOptions {
  registry?: CodexProfileRegistry;
  /** Explicit dependency injection for tests; never read from CODEX_HOME. */
  codexHome?: string;
  runtime?: CodexActivationRuntime;
  /** Opaque, one-shot server-owned permission; never client-supplied process IDs. */
  confirmationToken?: string;
}

export interface CodexActivationResult {
  name: string;
  email: string;
  plan: string | null;
  codexHome: string;
  previousEmail: string | null;
}

interface AuthSnapshot {
  content: Buffer;
  identity: CodexAccountIdentity & { email: string };
  binding: CodexActivationIdentity;
}

function readAuth(authPath: string, label: string, requireCredentials = false): AuthSnapshot {
  let content: Buffer;
  try {
    content = fs.readFileSync(authPath);
  } catch {
    throw new CodexActivationError('auth_read_failed', `Could not read ${label} auth.json.`);
  }
  let parsed: {
    tokens?: {
      id_token?: unknown;
      access_token?: unknown;
      refresh_token?: unknown;
      account_id?: unknown;
    };
  };
  try {
    parsed = JSON.parse(content.toString('utf8')) as typeof parsed;
  } catch {
    // JSON parser errors can embed token fragments; never propagate them.
    throw new CodexActivationError('auth_read_failed', `${label} auth.json is not valid JSON.`);
  }
  const token = parsed?.tokens?.id_token;
  const identity = typeof token === 'string' ? decodeIdToken(token) : {};
  if (typeof token !== 'string' || !hasStructurallyValidIdToken(token) || !identity.email) {
    throw new CodexActivationError(
      'invalid_profile',
      `${label} auth.json needs a valid Codex login with a decoded email.`
    );
  }
  const binding = decodeCodexActivationIdentity(token, parsed.tokens?.account_id);
  if (!binding) {
    throw new CodexActivationError(
      'invalid_profile',
      `${label} auth.json needs a consistent Codex workspace and account identity.`
    );
  }
  if (
    requireCredentials &&
    (typeof parsed.tokens?.access_token !== 'string' ||
      parsed.tokens.access_token.length === 0 ||
      typeof parsed.tokens.refresh_token !== 'string' ||
      parsed.tokens.refresh_token.length === 0)
  ) {
    throw new CodexActivationError(
      'invalid_profile',
      `${label} auth.json needs access and refresh tokens from a Codex login.`
    );
  }
  return { content, identity: { ...identity, email: identity.email }, binding };
}

function atomicReplace(authPath: string, content: Buffer): void {
  const temporary = `${authPath}.tmp.${process.pid}.${randomBytes(8).toString('hex')}`;
  let fd: number | undefined;
  try {
    fd = fs.openSync(temporary, 'wx', 0o600);
    fs.writeFileSync(fd, content);
    fs.fsyncSync(fd);
    fs.closeSync(fd);
    fd = undefined;
    fs.renameSync(temporary, authPath);
    const directoryFd = fs.openSync(path.dirname(authPath), 'r');
    try {
      fs.fsyncSync(directoryFd);
    } finally {
      fs.closeSync(directoryFd);
    }
  } catch {
    throw new CodexActivationError('auth_write_failed', 'Could not atomically save auth.json.');
  } finally {
    if (fd !== undefined) fs.closeSync(fd);
    try {
      fs.unlinkSync(temporary);
    } catch {
      // Successful rename leaves no temporary file.
    }
  }
}

function asSafeActivationError(error: unknown): CodexActivationError {
  if (error instanceof CodexActivationError) return error;
  if (error instanceof Error && error.name === 'CodexActivationRuntimeError') {
    const runtimeCode = (error as Error & { code?: string }).code;
    return new CodexActivationError(
      runtimeCode === 'busy'
        ? 'busy'
        : runtimeCode === 'confirmation_stale'
          ? 'confirmation_stale'
          : 'restart_failed',
      error.message
    );
  }
  return new CodexActivationError('restart_failed', 'Could not stop or restart Codex safely.');
}

function saveLiveProfile(
  registry: CodexProfileRegistry,
  requestedName: string,
  live: AuthSnapshot
): void {
  const candidates = registry.listProfiles().filter((name) => {
    try {
      return matchesCodexActivationIdentity(
        live.binding,
        readAuth(path.join(resolveCodexProfileDir(name), 'auth.json'), 'Profile').binding
      );
    } catch {
      return false;
    }
  });
  // Prefer the requested profile on a same-account activation so its refreshed
  // live tokens replace any old copy before the target is read again.
  const previousName = candidates.includes(requestedName) ? requestedName : candidates[0];
  if (!previousName) {
    throw new CodexActivationError(
      'invalid_profile',
      'The live Codex account has no saved profile. Import it before activating another account.'
    );
  }
  atomicReplace(path.join(resolveCodexProfileDir(previousName), 'auth.json'), live.content);
  registry.updateProfile(previousName, {
    email: live.identity.email,
    plan_type: live.identity.plan_type ?? null,
    account_id: live.identity.account_id,
  });
}

/**
 * Switch the shared VM login in one transaction. All writers exit before the
 * live login is saved, so shutdown refreshes cannot overwrite the new account.
 * The cross-process lock spans stop, save, replace, restart and verification.
 */
export async function activateCodexProfile(
  name: string,
  options: CodexActivationOptions = {}
): Promise<CodexActivationResult> {
  const nameError = getCodexProfileNameError(name);
  if (nameError) throw new CodexActivationError('invalid_profile', nameError);
  const codexHome = path.resolve(options.codexHome ?? getActivationCodexHome());
  const envHome = process.env.CODEX_HOME?.trim();
  if (envHome && path.resolve(envHome) !== codexHome) {
    throw new CodexActivationError(
      'invalid_codex_home',
      'Activation uses the shared ~/.codex directory. Unset the per-profile CODEX_HOME first.'
    );
  }
  const registry = options.registry ?? new CodexProfileRegistry();
  let profileExists: boolean;
  try {
    profileExists = registry.hasProfile(name);
  } catch {
    throw new CodexActivationError(
      'invalid_profile',
      'Codex profile registry could not be read safely.'
    );
  }
  if (!profileExists) {
    throw new CodexActivationError('invalid_profile', `Codex profile '${name}' does not exist.`);
  }
  const targetAuthPath = path.join(resolveCodexProfileDir(name), 'auth.json');
  const expectedTarget = readAuth(targetAuthPath, 'Target profile', true);
  let expectedIdentity = expectedTarget.binding;
  try {
    fs.mkdirSync(codexHome, { recursive: true, mode: 0o700 });
  } catch {
    throw new CodexActivationError(
      'auth_write_failed',
      'Could not prepare the shared Codex directory.'
    );
  }
  let release: () => Promise<void>;
  try {
    release = await acquireCodexActivationLock(codexHome);
  } catch {
    throw new CodexActivationError('busy', 'Another Codex account activation is already running.', {
      reason: 'activation_running',
    });
  }
  const authPath = path.join(codexHome, 'auth.json');
  const runtime = options.runtime ?? createCodexActivationRuntime(codexHome);
  let stopped = false;
  let original: AuthSnapshot | undefined;
  let authReplaced = false;
  let lockedTarget = expectedTarget;
  try {
    // Removal shares this lock. Its completed deletion must not stop writers
    // merely because our preflight ran before we waited for the lock.
    let lockedProfileExists: boolean;
    try {
      lockedProfileExists = registry.hasProfile(name);
    } catch {
      throw new CodexActivationError(
        'invalid_profile',
        'Codex profile registry could not be read safely.'
      );
    }
    if (!lockedProfileExists) {
      throw new CodexActivationError('invalid_profile', `Codex profile '${name}' does not exist.`);
    }
    lockedTarget = readAuth(targetAuthPath, 'Target profile', true);
    if (!matchesCodexActivationIdentity(expectedIdentity, lockedTarget.binding)) {
      throw new CodexActivationError(
        'verification_failed',
        'The target profile changed account before activation. Try again.'
      );
    }
    expectedIdentity = lockedTarget.binding;
    let approval: CodexActivationStopPlan | undefined;
    if (options.confirmationToken) {
      const live = fs.existsSync(authPath) ? readAuth(authPath, 'Live').content : Buffer.alloc(0);
      approval = consumeCodexActivationConfirmation(
        options.confirmationToken,
        name,
        codexAuthHash(Buffer.concat([live, lockedTarget.content]))
      );
      if (!approval) {
        throw new CodexActivationError(
          'confirmation_stale',
          'The activation confirmation expired or the account changed. Review a new warning.'
        );
      }
    }
    await runtime.stop(approval);
    stopped = true;
    // Snapshot ONLY after writers have exited. Never use an active-slot marker.
    if (fs.existsSync(authPath)) {
      original = readAuth(authPath, 'Live');
      saveLiveProfile(registry, name, original);
    }
    const target = readAuth(targetAuthPath, 'Target profile', true);
    if (!matchesCodexActivationIdentity(expectedIdentity, target.binding)) {
      throw new CodexActivationError(
        'verification_failed',
        'The target profile changed account during activation. Try again.'
      );
    }
    expectedIdentity = target.binding;
    authReplaced = true;
    atomicReplace(authPath, target.content);
    await runtime.start();
    const installed = readAuth(authPath, 'Activated', true);
    if (!matchesCodexActivationIdentity(expectedIdentity, installed.binding)) {
      throw new CodexActivationError(
        'verification_failed',
        'Codex did not keep the requested account after restarting.'
      );
    }
    registry.updateProfile(name, {
      last_used: new Date().toISOString(),
      email: installed.identity.email,
      plan_type: installed.identity.plan_type ?? null,
      account_id: installed.identity.account_id,
    });
    invalidateCodexAuthProfilesCache();
    return {
      name,
      email: installed.identity.email,
      plan: installed.identity.plan_type ?? null,
      codexHome,
      previousEmail: original?.identity.email ?? null,
    };
  } catch (error) {
    const safeError = asSafeActivationError(error);
    if (!stopped && error instanceof CodexActivationRuntimeError && error.code === 'busy') {
      if (error.stopPlan && !options.confirmationToken) {
        const live = fs.existsSync(authPath) ? readAuth(authPath, 'Live').content : Buffer.alloc(0);
        throw new CodexActivationError('busy', safeError.message, {
          reason: 'running_processes',
          confirmation: issueCodexActivationConfirmation(
            name,
            codexAuthHash(Buffer.concat([live, lockedTarget.content])),
            error.stopPlan
          ),
        });
      }
      throw new CodexActivationError('busy', safeError.message, { reason: 'unsupported_process' });
    }
    if (stopped) {
      try {
        // A partial start can leave writers alive. Quiesce them before rollback.
        if (authReplaced) {
          await runtime.stop();
          if (original) atomicReplace(authPath, original.content);
          else fs.rmSync(authPath, { force: true });
        }
        await runtime.start();
      } catch {
        try {
          await runtime.start();
        } catch {
          // Surface the rollback failure; finally disposes any remaining lock.
        }
        throw new CodexActivationError(
          safeError.code,
          `${safeError.message} Rollback could not restore and restart the original account.`
        );
      }
    }
    throw safeError;
  } finally {
    invalidateCodexAuthProfilesCache();
    try {
      await runtime.dispose?.();
    } catch {
      // Runtime cleanup must not mask the outcome of the activation transaction.
    }
    try {
      await release();
    } catch {
      // A failed lock cleanup cannot undo a verified activation or mask its error.
    }
  }
}
