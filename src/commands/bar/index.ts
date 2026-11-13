/** Native app opening, installation and version reporting. */

import { hasAnyFlag } from '../arg-extractor';

export async function handleBarCommand(args: string[]): Promise<void> {
  const subcommand = args[0];

  // --help / -h anywhere in args (e.g. `ccs bar install --help`) → show help.
  // Also dispatch bare `help` subcommand for symmetry.
  if (hasAnyFlag(args, ['--help', '-h']) || subcommand === 'help') {
    const { showHelp } = await import('./help-subcommand');
    await showHelp();
    return;
  }

  // --version / version are aliases for the version subcommand
  if (subcommand === '--version' || subcommand === 'version') {
    const { handleBarVersion } = await import('./version-subcommand');
    await handleBarVersion();
    return;
  }

  const commandHandlers: Record<string, (subArgs: string[]) => Promise<void>> = {
    install: async (subArgs) => {
      const { handleBarInstall } = await import('./install-subcommand');
      await handleBarInstall(subArgs);
    },
    uninstall: async (subArgs) => {
      const { handleBarUninstall } = await import('./uninstall-subcommand');
      await handleBarUninstall(subArgs);
    },
  };

  if (!subcommand) {
    const { handleBarLaunch } = await import('./launch-subcommand');
    await handleBarLaunch([]);
    return;
  }
  if (['launch', 'serve', 'stop', 'status'].includes(subcommand)) {
    console.error(
      '[X] Local bar server mode was retired. Run ai-account-center dashboard, then open the paired native app with ai-account-center bar.'
    );
    process.exitCode = 1;
    return;
  }
  const handler = commandHandlers[subcommand];
  if (!handler) {
    console.error(`[X] Unknown bar subcommand: ${subcommand}`);
    console.error('[i] Usage: ai-account-center bar [install|uninstall|version|--help]');
    process.exitCode = 1;
    return;
  }

  await handler(args.slice(1));
}
