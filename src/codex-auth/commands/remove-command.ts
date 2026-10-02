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
import { acquireCodexActivationLock, getActivationCodexHome } from '../codex-activation-lock';
import { CodexProfileRemovalError, removeCodexProfileUnderLock } from '../codex-profile-removal';
import { parseArgs, rejectUnsupportedOptions, getProfileNameError } from './types';
import type { CodexCommandContext } from './types';

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
    await removeCodexProfileUnderLock(ctx.registry, profileName, codexHome, {
      force: Boolean(force),
    });
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
    if (failure instanceof RemovalFailure || failure instanceof CodexProfileRemovalError)
      exitWithError(failure.message, failure.exitCode);
    else exitWithError('Could not safely remove the Codex profile.', ExitCode.GENERAL_ERROR);
    return;
  }
  console.log(ok(`Profile removed: ${profileName}`));
}
