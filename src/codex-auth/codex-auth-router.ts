/**
 * Codex auth command router.
 *
 * Exports runCodexAuth(argv) which routes argv[0] (the subcommand) to
 * the appropriate handler. Returns an exit code (0 = success, non-zero = error).
 *
 * Invoked by the AI Account Center codex-auth command.
 */

import { CodexProfileRegistry } from './codex-profile-registry';
import { printCodexAuthHelp, printRetiredCodexAuthCommand } from './codex-auth-help';
import {
  handleCreateCodex,
  handleLoginCodex,
  handleShowCodex,
  handleRemoveCodex,
  handleImportDefaultCodex,
  handleActivateCodex,
} from './commands/index';
import type { CodexCommandContext } from './commands/types';
import type { CodexActivationOptions } from './activate-codex-profile';
import { CodexActivationError } from './activate-codex-profile';

const packageJson = require('../../package.json') as { version: string };

/**
 * Route a `ai-account-center codex-auth <subcommand> [...args]` invocation.
 *
 * @param argv - Arguments after `auth`, e.g. ['create', 'work'] or ['--help']
 * @returns Exit code (0 success, 1 user error, 2+ system error)
 */
export async function runCodexAuth(
  argv: string[],
  activationOptions: CodexActivationOptions = {}
): Promise<number> {
  const [subcommand, ...rest] = argv;

  // Help / no-arg
  if (!subcommand || subcommand === '--help' || subcommand === '-h' || subcommand === 'help') {
    printCodexAuthHelp();
    return 0;
  }

  // Version passthrough
  if (subcommand === '--version' || subcommand === '-v') {
    process.stdout.write(`ai-account-center codex-auth ${packageJson.version}\n`);
    return 0;
  }

  if (subcommand === 'use' || subcommand === 'switch') {
    printRetiredCodexAuthCommand(subcommand);
    return 1;
  }

  const registry = new CodexProfileRegistry();
  const ctx: CodexCommandContext = {
    registry,
    version: packageJson.version,
  };

  try {
    switch (subcommand) {
      case 'activate':
        await handleActivateCodex(ctx, rest, activationOptions);
        return 0;
      case 'create':
        await handleCreateCodex(ctx, rest);
        return 0;
      case 'login':
        await handleLoginCodex(ctx, rest);
        return 0;
      case 'list':
      case 'status':
      case 'show':
        await handleShowCodex(ctx, rest);
        return 0;
      case 'remove':
        await handleRemoveCodex(ctx, rest);
        return 0;
      case 'import-default':
        await handleImportDefaultCodex(ctx, rest);
        return 0;
      default:
        process.stderr.write(`[X] Unknown command: ${subcommand}\n`);
        process.stderr.write(`    ai-account-center codex-auth --help\n`);
        return 1;
    }
  } catch (err) {
    // Unhandled errors from handlers (e.g. process.exit called inside)
    // These should be rare — handlers use exitWithError() which calls process.exit
    const msg = err instanceof Error ? err.message : String(err);
    process.stderr.write(
      err instanceof CodexActivationError
        ? `[X] ${msg}\n`
        : `[X] Unexpected error in ai-account-center codex-auth ${subcommand}: ${msg}\n`
    );
    return 1;
  }
}
