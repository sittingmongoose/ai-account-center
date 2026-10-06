import { extractOption, hasAnyFlag, scanCommandArgs } from './arg-extractor';
import { DEFAULT_DASHBOARD_HOST } from './config-dashboard-host';

const CONFIG_COMMAND_FLAGS = [
  '--help',
  '-h',
  '--port',
  '-p',
  '--host',
  '-H',
  '--no-open',
  '--dev',
] as const;

export interface ConfigCommandOptions {
  port?: number;
  host?: string;
  hostProvided: boolean;
  noOpen: boolean;
}

export interface ConfigCommandParseResult {
  help: boolean;
  error?: string;
  options: ConfigCommandOptions;
}

function formatUnexpectedArgsError(tokens: string[]): string {
  return `Unexpected arguments: ${tokens.join(' ')}`;
}

export function parseConfigCommandArgs(args: string[]): ConfigCommandParseResult {
  const options: ConfigCommandOptions = {
    host: DEFAULT_DASHBOARD_HOST,
    hostProvided: false,
    noOpen: false,
  };

  if (hasAnyFlag(args, ['--help', '-h'])) {
    return { help: true, options };
  }

  const portOption = extractOption(args, ['--port', '-p'], {
    knownFlags: CONFIG_COMMAND_FLAGS,
  });
  if (portOption.found) {
    if (portOption.missingValue || !portOption.value) {
      return { help: false, error: 'Invalid port number', options };
    }

    const port = Number(portOption.value);
    if (!/^\d+$/.test(portOption.value) || !Number.isInteger(port) || port <= 0 || port >= 65536) {
      return { help: false, error: 'Invalid port number', options };
    }

    options.port = port;
  }

  const hostOption = extractOption(portOption.remainingArgs, ['--host', '-H'], {
    knownFlags: CONFIG_COMMAND_FLAGS,
  });
  if (hostOption.found) {
    const host = hostOption.value?.trim();
    if (hostOption.missingValue || !host) {
      return { help: false, error: 'Invalid host value', options };
    }

    options.host = host;
    options.hostProvided = true;
  }

  if (hasAnyFlag(hostOption.remainingArgs, ['--dev'])) {
    return {
      help: false,
      error: '--dev is retired; use the AI Account Center dashboard.',
      options,
    };
  }
  options.noOpen = hasAnyFlag(hostOption.remainingArgs, ['--no-open']);

  const unexpected = scanCommandArgs(hostOption.remainingArgs, {
    knownFlags: ['--no-open'],
  });
  const unexpectedTokens = [...unexpected.unknownFlags, ...unexpected.positionals];
  if (unexpectedTokens.length > 0) {
    return {
      help: false,
      error: formatUnexpectedArgsError(unexpectedTokens),
      options,
    };
  }

  return { help: false, options };
}

export function showConfigCommandHelp(writeLine: (line: string) => void = console.log): void {
  writeLine('');
  writeLine('Usage: ai-account-center dashboard [command] [options]');
  writeLine('Compatibility: ccs config accepts the same arguments.');
  writeLine('');
  writeLine('Open the account and usage dashboard.');
  writeLine('');
  writeLine('Commands:');
  writeLine('  auth setup         Configure dashboard username and password');
  writeLine('  auth show          Display dashboard authentication status');
  writeLine('  auth disable       Disable dashboard authentication');
  writeLine('  usage-hub status   Show the T3 usage hub state and the URL for T3');
  writeLine('  usage-hub generate --stdout   Create the T3 usage hub key (shown once)');
  writeLine('  usage-hub rotate --stdout     Replace the T3 usage hub key');
  writeLine('  usage-hub off      Turn the T3 usage hub off');
  writeLine('');
  writeLine('Options:');
  writeLine('  --port, -p PORT    Server port (default: auto-detect)');
  writeLine('  --host, -H HOST    Bind host (default: localhost)');
  writeLine('  --no-open         Start the server without opening a browser');
  writeLine('  --help, -h        Show this help message');
  writeLine('');
  writeLine('Linux service startup never opens a browser automatically.');
  writeLine('');
  writeLine('Examples:');
  writeLine('  ai-account-center dashboard --host localhost --port 3000 --no-open');
  writeLine('  ai-account-center dashboard auth setup');
  writeLine('');
}

export function showConfigAuthHelp(writeLine: (line: string) => void = console.log): void {
  writeLine('Dashboard Auth Management');
  writeLine('Usage: ai-account-center dashboard auth <setup|show|disable>');
  writeLine('  setup    Configure dashboard username and password');
  writeLine('  show     Display authentication status (alias: status)');
  writeLine('  disable  Disable dashboard authentication');
  writeLine(
    'Environment overrides: CCS_DASHBOARD_AUTH_ENABLED, CCS_DASHBOARD_USERNAME, CCS_DASHBOARD_PASSWORD_HASH.'
  );
}
