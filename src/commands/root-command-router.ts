import { resolveNamedCommand, type NamedCommandRoute } from './named-command-router';
import { showRetiredCommandMessage } from './retired-command';
import { hasAnyFlag } from './arg-extractor';

export const ROOT_COMMAND_ROUTES: readonly NamedCommandRoute[] = [
  {
    name: 'dashboard',
    aliases: ['config'],
    handle: async (args) => {
      if (
        args[0] === 'auth' &&
        (!args[1] || args.slice(1).some((arg) => ['help', '--help', '-h'].includes(arg)))
      ) {
        const { showConfigAuthHelp } = await import('./config-command-options');
        showConfigAuthHelp();
        return;
      }
      if (
        args[0] !== 'auth' &&
        args[0] !== 'usage-hub' &&
        (hasAnyFlag(args, ['--help', '-h']) || args[0] === 'help')
      ) {
        const { showConfigCommandHelp } = await import('./config-command-options');
        showConfigCommandHelp();
        return;
      }
      const { handleConfigCommand } = await import('./config-command');
      await handleConfigCommand(args);
    },
  },
  {
    name: 'codex-auth',
    handle: async (args) => {
      if (!args.length || ['help', '--help', '-h'].includes(args[0])) {
        const { printCodexAuthHelp } = await import('../codex-auth/codex-auth-help');
        printCodexAuthHelp();
        return;
      }
      if (['use', 'switch'].includes(args[0])) {
        const { printRetiredCodexAuthCommand } = await import('../codex-auth/codex-auth-help');
        printRetiredCodexAuthCommand(args[0]);
        process.exitCode = 1;
        return;
      }
      if (['--version', '-v'].includes(args[0])) {
        const { getVersion } = await import('../utils/version');
        console.log(`AI Account Center codex-auth ${getVersion()}`);
        return;
      }
      if (
        ![
          'create',
          'login',
          'activate',
          'show',
          'list',
          'status',
          'remove',
          'import-default',
        ].includes(args[0])
      ) {
        showRetiredCommandMessage(`codex-auth ${args[0]}`);
        return;
      }
      const { runCodexAuth } = await import('../codex-auth/codex-auth-router');
      process.exitCode = await runCodexAuth(args);
    },
  },
  {
    name: 'antigravity',
    handle: async (args) => {
      const { handleAntigravityCommand } = await import('./antigravity-command');
      process.exitCode = await handleAntigravityCommand(args);
    },
  },
  {
    name: 'bar',
    handle: async (args) => {
      const { handleBarCommand } = await import('./bar');
      await handleBarCommand(args);
    },
  },
  {
    name: 'version',
    aliases: ['--version', '-v'],
    handle: async (args) => {
      if (args.length) {
        console.error(`[X] Unexpected version arguments: ${args.join(' ')}`);
        process.exitCode = 1;
        return;
      }
      const { handleVersionCommand } = await import('./version-command');
      await handleVersionCommand();
    },
  },
  {
    name: 'help',
    aliases: ['--help', '-h'],
    handle: async (args) => {
      const { handleHelpRoute } = await import('./help-command');
      await handleHelpRoute(args);
    },
  },
  {
    name: 'update',
    aliases: ['--update'],
    handle: () => showRetiredCommandMessage('update'),
  },
];

/** Only an actual account/dashboard/bar operation needs configuration-backed logging. */
export async function requiresRuntimeServices(args: string[]): Promise<boolean> {
  const [command, subcommand, ...rest] = args;
  if (command === 'dashboard' || command === 'config') {
    if (subcommand === 'auth') {
      return (
        ['setup', 'show', 'status', 'disable'].includes(rest[0]) &&
        !rest.some((arg) => ['help', '--help', '-h'].includes(arg))
      );
    }
    if (subcommand === 'usage-hub') {
      return (
        ['status', 'generate', 'rotate', 'off'].includes(rest[0]) &&
        !rest.some((arg) => ['help', '--help', '-h'].includes(arg))
      );
    }
    const { parseConfigCommandArgs } = await import('./config-command-options');
    const parsed = parseConfigCommandArgs(args.slice(1));
    return !parsed.help && !parsed.error;
  }
  if (command === 'codex-auth') {
    return (
      [
        'create',
        'login',
        'activate',
        'show',
        'list',
        'status',
        'remove',
        'import-default',
      ].includes(subcommand) && !rest.some((arg) => ['--help', '-h'].includes(arg))
    );
  }
  if (command === 'antigravity') {
    return (
      ['signin', 'status'].includes(subcommand) &&
      !rest.some((arg) => ['--help', '-h'].includes(arg))
    );
  }
  if (command === 'bar') {
    return (
      !hasAnyFlag(args, ['--help', '-h']) &&
      !args.some((arg) => ['help', 'version', '--version'].includes(arg)) &&
      (!subcommand ||
        subcommand.startsWith('-') ||
        ['launch', 'serve', 'stop', 'status', 'install', 'uninstall'].includes(subcommand))
    );
  }
  return false;
}

/** Every invocation is consumed here; unknown tokens never become profile launches. */
export async function tryHandleRootCommand(args: string[]): Promise<boolean> {
  if (!args.length) {
    const { handleHelpCommand } = await import('./help-command');
    await handleHelpCommand();
    return true;
  }
  const route = resolveNamedCommand(args[0], ROOT_COMMAND_ROUTES);
  if (route) await route.handle(args.slice(1));
  else showRetiredCommandMessage(args[0]);
  return true;
}
