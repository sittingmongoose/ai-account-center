/**
 * The staged delete of one saved Codex profile, shared by `codex-auth remove`
 * and the dashboard's Remove (CONTRACT-registry-lifecycle section 6.7).
 *
 * Call it under the Codex activation lock. It re-reads membership and the
 * saved default, refuses the live native login by fresh email, renames the
 * profile folder aside, keeps a preservation copy until the registry entry is
 * gone, and restores both if any step fails.
 */

import * as fs from 'fs';
import * as path from 'path';
import { ExitCode } from '../errors/exit-codes';
import { resolveCodexProfileDir } from './codex-profile-paths';
import { decodeIdToken, hasStructurallyValidIdToken } from './decode-id-token';
import type { CodexProfileRegistry } from './codex-profile-registry';
import type { CodexProfileMetadata } from './types';

/** Why a removal stopped; the message is for the CLI and may name local paths. */
export type CodexProfileRemovalReason =
  | 'not_found'
  | 'account_default'
  | 'account_active'
  | 'unverified'
  | 'failed';

export class CodexProfileRemovalError extends Error {
  constructor(
    message: string,
    readonly exitCode: ExitCode = ExitCode.GENERAL_ERROR,
    readonly reason: CodexProfileRemovalReason = 'failed'
  ) {
    super(message);
    this.name = 'CodexProfileRemovalError';
  }
}

export interface CodexProfileRemovalOptions {
  force?: boolean;
  /** Where the CLI prints its warnings; the dashboard passes a no-op. */
  warn?: (line: string) => void;
}

const defaultWarn = (line: string) => {
  process.stderr.write(line);
};

export async function removeCodexProfileUnderLock(
  registry: CodexProfileRegistry,
  profileName: string,
  codexHome: string,
  options: CodexProfileRemovalOptions = {}
): Promise<void> {
  const force = options.force === true;
  const warn = options.warn ?? defaultWarn;
  // Fresh membership/default/metadata reads happen after acquiring activation
  // lock and before taking registry's shorter write lock.
  if (!registry.hasProfile(profileName)) {
    throw new CodexProfileRemovalError(
      `Profile not found: ${profileName}`,
      ExitCode.PROFILE_ERROR,
      'not_found'
    );
  }
  const meta = registry.getProfile(profileName);
  const originalDefault = registry.getDefault();
  if (originalDefault === profileName && registry.listProfiles().length > 1 && !force) {
    throw new CodexProfileRemovalError(
      'Cannot remove default profile',
      ExitCode.PROFILE_ERROR,
      'account_default'
    );
  }
  const profileDir = resolveCodexProfileDir(profileName);
  const authJsonPath = path.join(profileDir, 'auth.json');
  const nativeAuthPath = path.join(codexHome, 'auth.json');
  assertInactiveSavedProfile(nativeAuthPath, authJsonPath);
  if (!fs.existsSync(profileDir)) {
    warn(`[!] Profile dir was already missing; removing registry entry only.\n`);
    try {
      registry.removeProfile(profileName, { forceDefault: force });
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      throw new CodexProfileRemovalError(
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
      throw new CodexProfileRemovalError('Permission denied');
    }
    throw err;
  }

  try {
    await fs.promises.cp(stagedDeleteDir, preservationDir, { recursive: true, errorOnExist: true });
  } catch (err) {
    const restored = restoreProfileDir(stagedDeleteDir, profileDir, warn);
    await removePathBestEffort(preservationDir);
    const preservedPath = restored ? profileDir : stagedDeleteDir;
    const msg = err instanceof Error ? err.message : String(err);
    throw new CodexProfileRemovalError(
      `Profile data delete preparation failed; profile data was preserved at ${preservedPath}.\n  ${msg}`
    );
  }

  try {
    // Native login/logout tools do not share our lock. Recheck after copying
    // and restore the staged data if a fresh native identity now matches it.
    assertInactiveSavedProfile(nativeAuthPath, path.join(stagedDeleteDir, 'auth.json'));
    registry.removeProfile(profileName, { forceDefault: force });
  } catch (err) {
    const restored = restoreProfileDir(stagedDeleteDir, profileDir, warn);
    await removePathBestEffort(preservationDir);
    const preservedPath = restored ? profileDir : stagedDeleteDir;
    const msg = err instanceof Error ? err.message : String(err);
    // The CLI text is unchanged; the reason tells the dashboard what refused it.
    throw new CodexProfileRemovalError(
      `Profile registry update failed; profile data was preserved at ${preservedPath}.\n  ${msg}`,
      ExitCode.GENERAL_ERROR,
      err instanceof CodexProfileRemovalError ? err.reason : 'failed'
    );
  }

  try {
    await fs.promises.rm(stagedDeleteDir, { recursive: true, force: true });
    await fs.promises.rm(preservationDir, { recursive: true, force: true });
  } catch (err) {
    const restoreSource = fs.existsSync(preservationDir) ? preservationDir : stagedDeleteDir;
    const restoredDir = restoreProfileDir(restoreSource, profileDir, warn);
    const restoredRegistry = restoreRegistryEntry(
      registry,
      profileName,
      meta,
      originalDefault,
      warn
    );
    if (restoredDir && restoreSource === preservationDir) {
      await removePathBestEffort(stagedDeleteDir);
    }
    const preservedPath = restoredDir ? profileDir : stagedDeleteDir;
    const msg = err instanceof Error ? err.message : String(err);
    const registryNote = restoredRegistry
      ? 'Profile registry entry was restored.'
      : 'Profile registry entry could not be restored automatically.';
    throw new CodexProfileRemovalError(
      `Profile data delete failed; profile data was preserved at ${preservedPath}. ${registryNote}\n  ${msg}`
    );
  }
}

async function removePathBestEffort(targetPath: string): Promise<void> {
  try {
    await fs.promises.rm(targetPath, { recursive: true, force: true });
  } catch {
    // best-effort cleanup after data has already been preserved elsewhere
  }
}

function restoreRegistryEntry(
  registry: CodexProfileRegistry,
  profileName: string,
  meta: CodexProfileMetadata,
  originalDefault: string | null,
  warn: (line: string) => void
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
    warn(
      `[!] Profile data delete failed and automatic registry restore failed for "${profileName}".\n`
    );
    return false;
  }
}

function restoreProfileDir(
  stagedDeleteDir: string,
  profileDir: string,
  warn: (line: string) => void
): boolean {
  try {
    if (fs.existsSync(stagedDeleteDir) && !fs.existsSync(profileDir)) {
      fs.renameSync(stagedDeleteDir, profileDir);
      return true;
    }
    return fs.existsSync(profileDir);
  } catch {
    warn(
      `[!] Registry update failed and automatic restore failed. Profile data remains at ${stagedDeleteDir}.\n`
    );
    return false;
  }
}

/** No cache or registry metadata can establish the current native identity. */
function freshEmail(authPath: string, label: string): string | null {
  let raw: string;
  try {
    raw = fs.readFileSync(authPath, 'utf8');
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw new CodexProfileRemovalError(
      `Could not read ${label} Codex login; profile retained.`,
      ExitCode.GENERAL_ERROR,
      'unverified'
    );
  }
  try {
    const parsed = JSON.parse(raw) as { tokens?: { id_token?: unknown } };
    const token = parsed?.tokens?.id_token;
    const email = typeof token === 'string' ? decodeIdToken(token).email : null;
    if (typeof token !== 'string' || !hasStructurallyValidIdToken(token) || !email) {
      throw new CodexProfileRemovalError(
        `Could not verify ${label} Codex login; profile retained.`,
        ExitCode.GENERAL_ERROR,
        'unverified'
      );
    }
    return email;
  } catch {
    // JSON errors may contain credential fragments. Report only our own text.
    throw new CodexProfileRemovalError(
      `Could not verify ${label} Codex login; profile retained.`,
      ExitCode.GENERAL_ERROR,
      'unverified'
    );
  }
}

function assertInactiveSavedProfile(nativeAuthPath: string, profileAuthPath: string): void {
  const liveEmail = freshEmail(nativeAuthPath, 'the current native');
  if (!liveEmail) return;
  const targetEmail = freshEmail(profileAuthPath, 'the saved profile');
  if (!targetEmail) {
    // A ghost entry's cached email is not fresh identity proof. Its deletion
    // is permitted when there is no native login, not while identity is unknown.
    throw new CodexProfileRemovalError(
      'Could not verify the saved Codex login; profile retained.',
      ExitCode.GENERAL_ERROR,
      'unverified'
    );
  }
  if (targetEmail === liveEmail) {
    throw new CodexProfileRemovalError(
      'Cannot remove a profile for the current Codex account. Activate another account first.',
      ExitCode.PROFILE_ERROR,
      'account_active'
    );
  }
}
