/**
 * codex-auth remove command.
 * Deletes profile dir + registry entry.
 * Guards: refuses to remove the default when others exist (unless --force).
 * Protects the current shared native login, including with --yes/--force.
 * --yes skips confirmation prompt.
 */

import * as fs from 'fs';
import * as path from 'path';
import { initUI, info, ok } from '../../utils/ui';
import { InteractivePrompt } from '../../utils/prompt';
import { exitWithError } from '../../errors';
import { ExitCode } from '../../errors/exit-codes';
import { resolveCodexProfileDir } from '../codex-profile-paths';
import { decodeAccountIdentity } from '../codex-account-identity';
import { decodeIdToken, hasStructurallyValidIdToken } from '../decode-id-token';
import { acquireCodexActivationLock, getActivationCodexHome } from '../codex-activation-lock';
import {
  decodeCodexActivationIdentity,
  matchesCodexActivationIdentity,
  type CodexActivationIdentity,
} from '../codex-activation-identity';
import { parseArgs, rejectUnsupportedOptions, getProfileNameError } from './types';
import type { CodexCommandContext } from './types';
import type { CodexProfileMetadata } from '../types';

export interface CodexRemoveOptions {
  /** Explicit native-home injection for tests; never inferred from CODEX_HOME. */
  codexHome?: string;
}

class RemovalFailure extends Error {
  constructor(
    message: string,
    readonly exitCode: ExitCode = ExitCode.GENERAL_ERROR
  ) {
    super(message);
  }
}

export async function handleRemoveCodex(
  ctx: CodexCommandContext,
  args: string[],
  options: CodexRemoveOptions = {}
): Promise<void> {
  await initUI();
  const parsed = parseArgs(args);
  rejectUnsupportedOptions(
    parsed,
    'ai-account-center codex-auth remove <name> [--yes|-y] [--force]',
    {
      yes: true,
      force: true,
    }
  );

  const { profileName, yes, force } = parsed;

  if (!profileName) {
    console.log('Usage: ai-account-center codex-auth remove <name> [--yes|-y] [--force]');
    exitWithError('Profile name required', ExitCode.PROFILE_ERROR);
    return;
  }

  const nameError = getProfileNameError(profileName);
  if (nameError) {
    exitWithError(nameError, ExitCode.PROFILE_ERROR);
    return;
  }

  const { registry } = ctx;

  if (!registry.hasProfile(profileName)) {
    exitWithError(`Profile not found: ${profileName}`, ExitCode.PROFILE_ERROR);
    return;
  }

  const allProfiles = registry.listProfiles();
  const isDefault = registry.getDefault() === profileName;

  // Default guard: refuse if others exist and no --force
  if (isDefault && allProfiles.length > 1 && !force) {
    console.log(`    Saved default protection is retained from the existing registry.`);
    console.log(`    Or override : ai-account-center codex-auth remove ${profileName} --force`);
    exitWithError('Cannot remove default profile', ExitCode.PROFILE_ERROR);
    return;
  }

  // Active-env warning (best-effort — can only see current shell)
  if (process.env.CCS_CODEX_PROFILE === profileName) {
    process.stderr.write(`[!] CCS_CODEX_PROFILE in this shell points to "${profileName}".\n`);
    process.stderr.write(`    After removal, codex sessions in this shell will fail until you\n`);
    const others = allProfiles.filter((n) => n !== profileName);
    if (others.length > 0) {
      process.stderr.write(
        `    Activate another saved login and unset legacy CCS_CODEX_PROFILE before retrying.\n`
      );
    } else {
      process.stderr.write(`    run: unset CCS_CODEX_PROFILE\n`);
    }
  }

  const profileDir = resolveCodexProfileDir(profileName);
  const authJsonPath = path.join(profileDir, 'auth.json');
  const authExists = fs.existsSync(authJsonPath);
  const dirExists = fs.existsSync(profileDir);

  // Load cached email for impact summary
  const meta = registry.getProfile(profileName);
  let emailStr = meta.email ?? null;
  if (!emailStr && authExists) {
    const identity = decodeAccountIdentity(authJsonPath);
    emailStr = identity.email ?? null;
  }

  // Confirmation remains outside the activation lock. Ghost cleanup retains
  // its existing non-interactive behavior, but gets the same locked guards.
  if (dirExists) {
    console.log(`Profile "${profileName}" will be removed.`);
    console.log(`  Profile dir   : ${profileDir}`);
    console.log(`  auth.json     : ${authExists ? 'present (will be deleted)' : 'not found'}`);
    console.log(`  Email         : ${emailStr ?? '<unknown>'}`);
    console.log('');
    if (!yes) {
      const confirmed = await InteractivePrompt.confirm('Delete this profile?', { default: false });
      if (!confirmed) {
        console.log(info('Cancelled.'));
        return;
      }
    }
  }

  const codexHome = path.resolve(options.codexHome ?? getActivationCodexHome());
  let release: (() => Promise<void>) | undefined;
  let failure: unknown;
  try {
    fs.mkdirSync(codexHome, { recursive: true, mode: 0o700 });
    try {
      release = await acquireCodexActivationLock(codexHome);
    } catch {
      throw new RemovalFailure('Another Codex account activation or removal is already running.');
    }
    await _removeUnderLock(ctx, profileName, Boolean(force), codexHome);
  } catch (error) {
    failure = error;
  } finally {
    // process.exit in exitWithError would bypass finally: report only below.
    try {
      await release?.();
    } catch {
      // A lock cleanup error must not mask completed restoration or its error.
    }
  }
  if (failure) {
    if (failure instanceof RemovalFailure) exitWithError(failure.message, failure.exitCode);
    else exitWithError('Could not safely remove the Codex profile.', ExitCode.GENERAL_ERROR);
    return;
  }
  console.log(ok(`Profile removed: ${profileName}`));
}

async function _removeUnderLock(
  ctx: CodexCommandContext,
  profileName: string,
  force: boolean,
  codexHome: string
): Promise<void> {
  const { registry } = ctx;
  // Fresh membership/default/metadata reads happen after acquiring activation
  // lock and before taking registry's shorter write lock.
  if (!registry.hasProfile(profileName)) {
    throw new RemovalFailure(`Profile not found: ${profileName}`, ExitCode.PROFILE_ERROR);
  }
  const meta = registry.getProfile(profileName);
  const originalDefault = registry.getDefault();
  if (originalDefault === profileName && registry.listProfiles().length > 1 && !force) {
    throw new RemovalFailure('Cannot remove default profile', ExitCode.PROFILE_ERROR);
  }
  const profileDir = resolveCodexProfileDir(profileName);
  const authJsonPath = path.join(profileDir, 'auth.json');
  const nativeAuthPath = path.join(codexHome, 'auth.json');
  _assertInactiveSavedProfile(nativeAuthPath, authJsonPath);
  if (!fs.existsSync(profileDir)) {
    process.stderr.write(`[!] Profile dir was already missing; removing registry entry only.\n`);
    try {
      registry.removeProfile(profileName, { forceDefault: force });
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      throw new RemovalFailure(
        `Profile registry update failed; profile dir was already missing.\n  ${msg}`
      );
    }
    return;
  }

  const stagedDeleteDir = `${profileDir}.deleting.${process.pid}.${Math.random()
    .toString(36)
    .slice(2)}`;
  const preservationDir = `${profileDir}.preserved.${process.pid}.${Math.random()
    .toString(36)
    .slice(2)}`;

  try {
    fs.renameSync(profileDir, stagedDeleteDir);
  } catch (err) {
    const e = err as NodeJS.ErrnoException;
    if (e.code === 'EACCES') {
      throw new RemovalFailure('Permission denied');
    }
    throw err;
  }

  try {
    await fs.promises.cp(stagedDeleteDir, preservationDir, { recursive: true, errorOnExist: true });
  } catch (err) {
    const restored = _restoreProfileDir(stagedDeleteDir, profileDir);
    await _removePathBestEffort(preservationDir);
    const preservedPath = restored ? profileDir : stagedDeleteDir;
    const msg = err instanceof Error ? err.message : String(err);
    throw new RemovalFailure(
      `Profile data delete preparation failed; profile data was preserved at ${preservedPath}.\n  ${msg}`
    );
  }

  try {
    // Native login/logout tools do not share our lock. Recheck after copying
    // and restore the staged data if a fresh native identity now matches it.
    _assertInactiveSavedProfile(nativeAuthPath, path.join(stagedDeleteDir, 'auth.json'));
    registry.removeProfile(profileName, { forceDefault: force });
  } catch (err) {
    const restored = _restoreProfileDir(stagedDeleteDir, profileDir);
    await _removePathBestEffort(preservationDir);
    const preservedPath = restored ? profileDir : stagedDeleteDir;
    const msg = err instanceof Error ? err.message : String(err);
    throw new RemovalFailure(
      `Profile registry update failed; profile data was preserved at ${preservedPath}.\n  ${msg}`
    );
  }

  try {
    await fs.promises.rm(stagedDeleteDir, { recursive: true, force: true });
    await fs.promises.rm(preservationDir, { recursive: true, force: true });
  } catch (err) {
    const restoreSource = fs.existsSync(preservationDir) ? preservationDir : stagedDeleteDir;
    const restoredDir = _restoreProfileDir(restoreSource, profileDir);
    const restoredRegistry = _restoreRegistryEntry(registry, profileName, meta, originalDefault);
    if (restoredDir && restoreSource === preservationDir) {
      await _removePathBestEffort(stagedDeleteDir);
    }
    const preservedPath = restoredDir ? profileDir : stagedDeleteDir;
    const msg = err instanceof Error ? err.message : String(err);
    const registryNote = restoredRegistry
      ? 'Profile registry entry was restored.'
      : 'Profile registry entry could not be restored automatically.';
    throw new RemovalFailure(
      `Profile data delete failed; profile data was preserved at ${preservedPath}. ${registryNote}\n  ${msg}`
    );
  }
}

async function _removePathBestEffort(targetPath: string): Promise<void> {
  try {
    await fs.promises.rm(targetPath, { recursive: true, force: true });
  } catch {
    // best-effort cleanup after data has already been preserved elsewhere
  }
}

function _restoreRegistryEntry(
  registry: CodexCommandContext['registry'],
  profileName: string,
  meta: CodexProfileMetadata,
  originalDefault: string | null
): boolean {
  try {
    if (registry.hasProfile(profileName)) {
      registry.updateProfile(profileName, meta);
    } else {
      registry.createProfile(profileName, meta);
    }
    if (originalDefault === profileName) {
      registry.setDefault(profileName);
    }
    return true;
  } catch {
    process.stderr.write(
      `[!] Profile data delete failed and automatic registry restore failed for "${profileName}".\n`
    );
    return false;
  }
}

function _restoreProfileDir(stagedDeleteDir: string, profileDir: string): boolean {
  try {
    if (fs.existsSync(stagedDeleteDir) && !fs.existsSync(profileDir)) {
      fs.renameSync(stagedDeleteDir, profileDir);
      return true;
    }
    return fs.existsSync(profileDir);
  } catch {
    process.stderr.write(
      `[!] Registry update failed and automatic restore failed. Profile data remains at ${stagedDeleteDir}.\n`
    );
    return false;
  }
}

/** A fresh read of one auth.json: its email, and its workspace and principal binding. */
interface FreshLogin {
  email: string;
  /** Null when the token carries no readable workspace binding; never printed. */
  binding: CodexActivationIdentity | null;
}

/** No cache or registry metadata can establish the current native identity. */
function _freshLogin(authPath: string, label: string): FreshLogin | null {
  let raw: string;
  try {
    raw = fs.readFileSync(authPath, 'utf8');
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw new RemovalFailure(`Could not read ${label} Codex login; profile retained.`);
  }
  try {
    const parsed = JSON.parse(raw) as { tokens?: { id_token?: unknown; account_id?: unknown } };
    const token = parsed?.tokens?.id_token;
    const email = typeof token === 'string' ? decodeIdToken(token).email : null;
    if (typeof token !== 'string' || !hasStructurallyValidIdToken(token) || !email) {
      throw new RemovalFailure(`Could not verify ${label} Codex login; profile retained.`);
    }
    return { email, binding: decodeCodexActivationIdentity(token, parsed.tokens?.account_id) };
  } catch {
    // JSON errors may contain credential fragments. Report only our own text.
    throw new RemovalFailure(`Could not verify ${label} Codex login; profile retained.`);
  }
}

/**
 * Whether a saved login is the live one. Like activation, this matches the workspace
 * and principal, so a personal and a workspace login under one email stay apart. A
 * match in either direction refuses, so an older saved token of the live workspace and
 * email that carries no principal stays protected. Only when either side has no
 * readable binding does the email alone decide.
 */
function _isLiveLogin(live: FreshLogin, saved: FreshLogin): boolean {
  if (!live.binding || !saved.binding) return live.email === saved.email;
  return (
    matchesCodexActivationIdentity(live.binding, saved.binding) ||
    matchesCodexActivationIdentity(saved.binding, live.binding)
  );
}

function _assertInactiveSavedProfile(nativeAuthPath: string, profileAuthPath: string): void {
  const live = _freshLogin(nativeAuthPath, 'the current native');
  if (!live) return;
  const target = _freshLogin(profileAuthPath, 'the saved profile');
  if (!target) {
    // A ghost entry's cached email is not fresh identity proof. Its deletion
    // is permitted when there is no native login, not while identity is unknown.
    throw new RemovalFailure('Could not verify the saved Codex login; profile retained.');
  }
  if (_isLiveLogin(live, target)) {
    throw new RemovalFailure(
      'Cannot remove a profile for the current Codex account. Activate another account first.',
      ExitCode.PROFILE_ERROR
    );
  }
}
