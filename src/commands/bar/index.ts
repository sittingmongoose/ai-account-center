/**
 * `ccs bar` command dispatcher
 *
 * Mirrors the pattern in src/commands/docker/index.ts.
 * Subcommands: launch (default), serve, stop, status, install, uninstall,
 *              version / --version, help / --help / -h.
 *
 * `serve` is the long-lived server host (spawned detached by `launch` and by
 * the Swift app). `stop` and `status` manage the detached server lifecycle.
 */

import { hasAnyFlag } from '../arg-extractor';
import { validatePortArgs } from './port-arg';

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
    launch: async (subArgs) => {
      const { handleBarLaunch } = await import('./launch-subcommand');
      await handleBarLaunch(subArgs);
    },
    serve: async (subArgs) => {
      const { handleBarServe } = await import('./serve-subcommand');
      await handleBarServe(subArgs);
    },
    stop: async (subArgs) => {
      const { handleBarStop } = await import('./stop-subcommand');
      await handleBarStop(subArgs);
    },
    status: async (subArgs) => {
      const { handleBarStatus } = await import('./status-subcommand');
      await handleBarStatus(subArgs);
    },
    install: async (subArgs) => {
      const { handleBarInstall } = await import('./install-subcommand');
      await handleBarInstall(subArgs);
    },
    uninstall: async (subArgs) => {
      const { handleBarUninstall } = await import('./uninstall-subcommand');
      await handleBarUninstall(subArgs);
    },
  };

  // Bare `ccs bar` → launch. Bare flags (e.g. `ccs bar --port 3999`) also go to
  // launch with the full arg list preserved (--help/--version were handled above).
  if (!subcommand || subcommand === 'launch' || subcommand.startsWith('-')) {
    const launchArgs = subcommand === 'launch' ? args.slice(1) : args;
    const argError = validatePortArgs(launchArgs);
    if (argError !== null) {
      console.error(`[X] ${argError}`);
      console.error('[i] Usage: ai-account-center bar [--port N]');
      process.exitCode = 1;
      return;
    }
    await commandHandlers.launch(launchArgs);
    return;
  }

  const handler = commandHandlers[subcommand];
  if (!handler) {
    console.error(`[X] Unknown bar subcommand: ${subcommand}`);
    console.error(
      '[i] Usage: ai-account-center bar [launch|serve|stop|status|install|uninstall|version|--help]'
    );
    process.exitCode = 1;
    return;
  }

  await handler(args.slice(1));
}
