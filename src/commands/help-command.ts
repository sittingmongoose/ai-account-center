import { initUI } from '../utils/ui';
import {
  ROOT_COMMAND_CATALOG,
  getPublicRootCommands,
  type RootCommandEntry,
} from './command-catalog';
import { showRetiredCommandMessage } from './retired-command';

type HelpWriter = (line: string) => void;

export async function handleHelpCommand(writeLine: HelpWriter = console.log): Promise<void> {
  await initUI();
  writeLine('AI Account Center');
  writeLine('');
  writeLine('Usage: ai-account-center <command> [options]');
  writeLine('');
  writeLine('Commands:');
  for (const entry of getPublicRootCommands()) {
    writeLine(`  ${entry.name.padEnd(14)} ${entry.summary}`);
  }
  writeLine('');
  writeLine('Compatibility: ccs uses the same commands; config is an alias for dashboard.');
  writeLine(
    'Global option: --config-dir PATH selects the existing account configuration directory.'
  );
  writeLine('');
  writeLine('Examples:');
  writeLine('  ai-account-center dashboard --host localhost --port 3000 --no-open');
  writeLine('  ai-account-center dashboard auth setup');
  writeLine('  ai-account-center codex-auth show');
  writeLine('  ai-account-center codex-auth activate work');
  writeLine('  ai-account-center antigravity signin party');
  writeLine('  ai-account-center bar status');
  writeLine('');
}

export async function handleHelpRoute(
  args: string[],
  writeLine: HelpWriter = console.log
): Promise<void> {
  if (!args.length) return handleHelpCommand(writeLine);
  if (args.length > 1) {
    writeLine(`Unexpected help arguments: ${args.slice(1).join(' ')}`);
    process.exitCode = 1;
    return;
  }
  switch (args[0]) {
    case 'dashboard':
    case 'config': {
      const { showConfigCommandHelp } = await import('./config-command-options');
      showConfigCommandHelp(writeLine);
      return;
    }
    case 'codex-auth': {
      const { printCodexAuthHelp } = await import('../codex-auth/codex-auth-help');
      printCodexAuthHelp();
      return;
    }
    case 'antigravity': {
      const { printAntigravityHelp } = await import('./antigravity-command');
      printAntigravityHelp(writeLine);
      return;
    }
    case 'bar': {
      const { showHelp } = await import('./bar/help-subcommand');
      await showHelp();
      return;
    }
    default:
      showRetiredCommandMessage(args[0], writeLine);
  }
}

export function getRootHelpVisibleCommands(): string[] {
  return getPublicRootCommands().map((entry) => entry.name);
}

export function getRootHelpCatalogEntries(): readonly RootCommandEntry[] {
  return ROOT_COMMAND_CATALOG;
}
