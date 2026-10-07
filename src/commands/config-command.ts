/**
 * Config Command Handler
 *
 * Launches web-based configuration dashboard.
 * Usage: ai-account-center dashboard [--port PORT] [--host HOST] [--no-open]
 */

import getPort from 'get-port';
import open from 'open';
import { startServer } from '../web-server';
import { setupGracefulShutdown } from '../web-server/shutdown';
import { initUI, header, ok, info, warn, fail } from '../utils/ui';
import { resolveNamedCommand, type NamedCommandRoute } from './named-command-router';
import {
  isLoopbackHost,
  isWildcardHost,
  normalizeDashboardHost,
  resolveDashboardUrls,
} from './config-dashboard-host';
import { parseConfigCommandArgs, showConfigCommandHelp } from './config-command-options';
import { createLogger } from '../services/logging';
import { getDashboardAuthConfig } from '../config/config-loader-facade';
import { showRetiredCommandMessage } from './retired-command';

const logger = createLogger('command:config');

const CONFIG_SUBCOMMAND_ROUTES: readonly NamedCommandRoute[] = [
  {
    name: 'auth',
    handle: async (args) => {
      const { handleConfigAuthCommand } = await import('./config-auth');
      await handleConfigAuthCommand(args);
    },
  },
  {
    name: 'usage-hub',
    handle: async (args) => {
      const { handleUsageHubCommand } = await import('./usage-hub-command');
      process.exitCode = await handleUsageHubCommand(args);
    },
  },
  {
    name: 'proxy',
    handle: async (args) => {
      const { handleProxyCommand } = await import('./dashboard-proxy-command');
      process.exitCode = await handleProxyCommand(args);
    },
  },
];

interface ConfigCommandDependencies {
  getPort: typeof getPort;
  openBrowser: typeof open;
  startServer: typeof startServer;
  setupGracefulShutdown: typeof setupGracefulShutdown;
  platform?: NodeJS.Platform;
  getDashboardAuthConfig: typeof getDashboardAuthConfig;
  initUI: typeof initUI;
  header: typeof header;
  ok: typeof ok;
  info: typeof info;
  warn: typeof warn;
  fail: typeof fail;
  resolveNamedCommand: typeof resolveNamedCommand;
  configSubcommandRoutes: readonly NamedCommandRoute[];
}

const defaultConfigCommandDependencies: ConfigCommandDependencies = {
  getPort,
  openBrowser: open,
  startServer,
  setupGracefulShutdown,
  getDashboardAuthConfig,
  initUI,
  header,
  ok,
  info,
  warn,
  fail,
  resolveNamedCommand,
  configSubcommandRoutes: CONFIG_SUBCOMMAND_ROUTES,
};

/**
 * Handle config command
 */
export async function handleConfigCommand(
  args: string[],
  deps: ConfigCommandDependencies = defaultConfigCommandDependencies
): Promise<void> {
  if (args.length === 1 && args[0] === 'help') {
    await deps.initUI();
    showConfigCommandHelp();
    return;
  }

  if (['channels', 'image-analysis', 'thinking'].includes(args[0])) {
    showRetiredCommandMessage(`config ${args[0]}`);
    return;
  }

  const subcommand = args[0]?.startsWith('-')
    ? undefined
    : deps.resolveNamedCommand(args[0], deps.configSubcommandRoutes);
  if (subcommand) {
    await subcommand.handle(args.slice(1));
    return;
  }

  await deps.initUI();

  const parsed = parseConfigCommandArgs(args);
  if (parsed.help) {
    showConfigCommandHelp();
    return;
  }
  if (parsed.error) {
    console.error(deps.fail(parsed.error));
    process.exitCode = 1;
    return;
  }

  const options = parsed.options;
  logger.info('dashboard.launch_requested', 'Config dashboard launch requested', {
    noOpen: options.noOpen,
    host: options.host || null,
    port: options.port || null,
  });

  console.log(deps.header('AI Account Center'));
  console.log('');

  console.log(deps.info('Starting dashboard server...'));

  // Find available port
  const port =
    options.port ??
    (await deps.getPort({
      port: [3000, 3001, 3002, 8000, 8080],
    }));

  try {
    // Start server
    const serverOptions: Parameters<typeof startServer>[0] = {
      port,
    };
    if (options.host) {
      serverOptions.host = normalizeDashboardHost(options.host);
    }

    const { server, wss, cleanup } = await deps.startServer(serverOptions);

    // Setup graceful shutdown
    deps.setupGracefulShutdown(server, wss, cleanup);

    const urls = resolveDashboardUrls(resolveServerBindHost(server) ?? options.host, port);
    const shouldWarnAboutExposure = urls.bindHost ? !isLoopbackHost(urls.bindHost) : false;

    console.log(deps.ok(`Dashboard: ${urls.browserUrl}`));

    if (shouldWarnAboutExposure && urls.bindHost) {
      console.log(deps.info(`Bind host: ${urls.bindHost}`));
      if (urls.networkUrls?.length === 1) {
        console.log(deps.info(`Network URL: ${urls.networkUrls[0]}`));
      } else if (urls.networkUrls && urls.networkUrls.length > 1) {
        console.log(deps.info('Network URLs:'));
        for (const networkUrl of urls.networkUrls) {
          console.log(deps.info(`  ${networkUrl}`));
        }
      }
    }

    if (shouldWarnAboutExposure && urls.bindHost) {
      const authConfig = deps.getDashboardAuthConfig();
      console.log(
        deps.warn('Dashboard may be reachable from other devices that can connect to this machine.')
      );
      if (!authConfig.enabled) {
        console.log(deps.info('Protect it before sharing: ai-account-center dashboard auth setup'));
      }
      if (isWildcardHost(urls.bindHost) && !urls.networkUrls?.length) {
        console.log(deps.info('Use your machine IP or hostname from the other device.'));
      }
    }
    console.log('');

    const shouldOpen = !options.noOpen && (deps.platform ?? process.platform) !== 'linux';
    if (shouldOpen) {
      try {
        await deps.openBrowser(urls.browserUrl, { wait: false });
        logger.info('dashboard.browser_opened', 'Dashboard browser launch attempted', {
          browserUrl: urls.browserUrl,
        });
        console.log(deps.info('Browser opened automatically'));
      } catch {
        logger.warn('dashboard.browser_open_failed', 'Automatic browser launch failed', {
          browserUrl: urls.browserUrl,
        });
        console.log(deps.info(`Open manually: ${urls.browserUrl}`));
      }
    } else {
      console.log(deps.info(`Open manually: ${urls.browserUrl}`));
    }

    console.log('');
    console.log(deps.info('Press Ctrl+C to stop'));
  } catch (error) {
    logger.error('dashboard.launch_failed', 'Config dashboard failed to launch', {
      message: (error as Error).message,
    });
    console.error(deps.fail(`Failed to start server: ${(error as Error).message}`));
    process.exitCode = 1;
    return;
  }
}

function resolveServerBindHost(server: {
  address(): string | { address: string } | null;
}): string | undefined {
  const address = server.address();
  if (!address || typeof address === 'string') {
    return undefined;
  }

  return address.address;
}
