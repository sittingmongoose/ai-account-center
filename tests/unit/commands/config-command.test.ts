import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import * as os from 'os';
import { handleConfigCommand } from '../../../src/commands/config-command';
import { resolveNamedCommand } from '../../../src/commands/named-command-router';

type ConfigCommandDeps = NonNullable<Parameters<typeof handleConfigCommand>[1]>;

const startServerCalls: Array<Record<string, unknown>> = [];
const browserCalls: string[] = [];
const configAuthCalls: string[][] = [];
let shutdownCalls = 0;
let portLookupCalls = 0;
let logLines: string[] = [];
let errorLines: string[] = [];
let dashboardAuthEnabled = false;
let startServerError: Error | undefined;
let mockServerBindHost = '::1';
let browserError: Error | undefined;
let originalConsoleLog: typeof console.log;
let originalConsoleError: typeof console.error;
let originalExitCode: typeof process.exitCode;

function createTestDeps(platform: NodeJS.Platform = 'linux'): ConfigCommandDeps {
  return {
    platform,
    getPort: async () => {
      portLookupCalls++;
      return 3000;
    },
    openBrowser: async (url) => {
      browserCalls.push(String(url));
      if (browserError) throw browserError;
      return undefined;
    },
    startServer: async (options) => {
      startServerCalls.push({ ...options });
      if (startServerError) throw startServerError;
      return {
        server: { address: () => ({ address: mockServerBindHost }) } as never,
        wss: {} as never,
        cleanup: () => {},
      };
    },
    setupGracefulShutdown: () => {
      shutdownCalls++;
    },
    getDashboardAuthConfig: () => ({
      enabled: dashboardAuthEnabled,
      username: '',
      password_hash: '',
      session_timeout_hours: 24,
    }),
    initUI: async () => {},
    header: (message) => message,
    ok: (message) => message,
    info: (message) => message,
    warn: (message) => message,
    fail: (message) => message,
    resolveNamedCommand,
    configSubcommandRoutes: [
      {
        name: 'auth',
        handle: async (args) => {
          configAuthCalls.push([...args]);
        },
      },
    ],
  };
}

beforeEach(() => {
  startServerCalls.length = 0;
  browserCalls.length = 0;
  configAuthCalls.length = 0;
  shutdownCalls = 0;
  portLookupCalls = 0;
  logLines = [];
  errorLines = [];
  dashboardAuthEnabled = false;
  startServerError = undefined;
  browserError = undefined;
  mockServerBindHost = '::1';
  originalConsoleLog = console.log;
  originalConsoleError = console.error;
  originalExitCode = process.exitCode;
  process.exitCode = 0;
  console.log = (...args: unknown[]) => {
    logLines.push(args.map(String).join(' '));
  };
  console.error = (...args: unknown[]) => {
    errorLines.push(args.map(String).join(' '));
  };
});

afterEach(() => {
  console.log = originalConsoleLog;
  console.error = originalConsoleError;
  process.exitCode = originalExitCode ?? 0;
});

describe('account dashboard startup', () => {
  it.each(['help', '--help', '-h'])('shows help without starting a server: %s', async (token) => {
    await handleConfigCommand([token], createTestDeps());

    expect(startServerCalls).toHaveLength(0);
    expect(browserCalls).toHaveLength(0);
    expect(portLookupCalls).toBe(0);
    expect(logLines.join('\n')).toContain('Usage: ai-account-center dashboard');
    expect(process.exitCode ?? 0).toBe(0);
  });

  it('delegates dashboard authentication arguments unchanged before startup', async () => {
    await handleConfigCommand(['auth', 'setup', '--help'], createTestDeps());

    expect(configAuthCalls).toEqual([['setup', '--help']]);
    expect(startServerCalls).toHaveLength(0);
    expect(portLookupCalls).toBe(0);
  });

  it.each(['channels', 'image-analysis', 'thinking'])(
    'rejects a retired config subcommand before startup: %s',
    async (command) => {
      await handleConfigCommand([command, '--enable'], createTestDeps());

      expect(startServerCalls).toHaveLength(0);
      expect(portLookupCalls).toBe(0);
      expect(configAuthCalls).toHaveLength(0);
      expect(process.exitCode).toBe(1);
      const rendered = [...logLines, ...errorLines].join('\n');
      expect(rendered).toContain(command);
      expect(rendered).toMatch(/retired|no longer/i);
      expect(rendered).toMatch(/dashboard|account center/i);
    }
  );

  it('rejects the retired Vite development flag before startup', async () => {
    await handleConfigCommand(['--dev'], createTestDeps());

    expect(startServerCalls).toHaveLength(0);
    expect(portLookupCalls).toBe(0);
    expect(process.exitCode).toBe(1);
    expect([...logLines, ...errorLines].join('\n')).toMatch(/retired|no longer/i);
  });

  it('rejects unknown config subcommands before startup', async () => {
    await handleConfigCommand(['bogus'], createTestDeps());

    expect(startServerCalls).toHaveLength(0);
    expect(portLookupCalls).toBe(0);
    expect(process.exitCode).toBe(1);
    expect(errorLines.join('\n')).toContain('Unexpected arguments: bogus');
  });

  it('binds the default startup path to localhost and registers shutdown', async () => {
    await handleConfigCommand([], createTestDeps());

    expect(startServerCalls).toEqual([{ port: 3000, host: 'localhost' }]);
    expect(portLookupCalls).toBe(1);
    expect(shutdownCalls).toBe(1);
    expect(logLines.join('\n')).toContain('Dashboard: http://[::1]:3000');
    expect(logLines.join('\n')).not.toContain('Dashboard may be reachable from other devices');
    expect(errorLines).toHaveLength(0);
  });

  it('does not open a browser by default on Linux', async () => {
    await handleConfigCommand([], createTestDeps('linux'));

    expect(startServerCalls).toHaveLength(1);
    expect(browserCalls).toHaveLength(0);
    expect(logLines.join('\n')).toContain('http://[::1]:3000');
  });

  it.each(['linux', 'darwin', 'win32'] as const)(
    'honors --no-open while starting the dashboard on %s',
    async (platform) => {
      await handleConfigCommand(['--no-open'], createTestDeps(platform));

      expect(startServerCalls).toHaveLength(1);
      expect(shutdownCalls).toBe(1);
      expect(browserCalls).toHaveLength(0);
    }
  );

  it.each(['darwin', 'win32'] as const)('opens the browser by default on %s', async (platform) => {
    await handleConfigCommand([], createTestDeps(platform));

    expect(browserCalls).toEqual(['http://[::1]:3000']);
    expect(logLines.join('\n')).toContain('Browser opened automatically');
  });

  it('prints a usable URL if a supported platform cannot open a browser', async () => {
    browserError = new Error('No browser installed');
    await handleConfigCommand([], createTestDeps('darwin'));

    expect(startServerCalls).toHaveLength(1);
    expect(shutdownCalls).toBe(1);
    expect(logLines.join('\n')).toContain('Open manually: http://[::1]:3000');
    expect(errorLines).toHaveLength(0);
  });

  it('preserves explicit wildcard host and port with exposure guidance', async () => {
    mockServerBindHost = '0.0.0.0';
    await handleConfigCommand(['--host=0.0.0.0', '--port=4100'], createTestDeps());

    expect(startServerCalls).toEqual([{ port: 4100, host: '0.0.0.0' }]);
    expect(portLookupCalls).toBe(0);
    const rendered = logLines.join('\n');
    expect(rendered).toContain('Dashboard: http://localhost:4100');
    expect(rendered).toContain('Bind host: 0.0.0.0');
    expect(rendered).toContain(
      'Dashboard may be reachable from other devices that can connect to this machine.'
    );
    expect(rendered).toContain('Protect it before sharing: ai-account-center dashboard auth setup');
  });

  it('does not suggest auth setup when exposed dashboard authentication is enabled', async () => {
    mockServerBindHost = '0.0.0.0';
    dashboardAuthEnabled = true;
    await handleConfigCommand(['--host', '0.0.0.0'], createTestDeps());

    expect(logLines.join('\n')).toContain('Dashboard may be reachable from other devices');
    expect(logLines.join('\n')).not.toContain('Protect it before sharing');
  });

  it('starts a wildcard dashboard when network interface discovery is unavailable', async () => {
    mockServerBindHost = '0.0.0.0';
    const networkInterfacesSpy = spyOn(os, 'networkInterfaces').mockImplementation(() => {
      throw new Error('network interfaces unavailable');
    });

    try {
      await handleConfigCommand(['--host', '0.0.0.0'], createTestDeps());

      expect(startServerCalls).toHaveLength(1);
      expect(logLines.join('\n')).toContain('Dashboard: http://localhost:3000');
      expect(errorLines).toHaveLength(0);
    } finally {
      networkInterfacesSpy.mockRestore();
    }
  });

  it('fails cleanly before browser launch or shutdown registration if binding fails', async () => {
    startServerError = new Error('Unable to bind 192.0.2.123:4100');
    await handleConfigCommand(['--host', '192.0.2.123', '--port', '4100'], createTestDeps());

    expect(process.exitCode).toBe(1);
    expect(shutdownCalls).toBe(0);
    expect(browserCalls).toHaveLength(0);
    expect(errorLines.join('\n')).toContain(
      'Failed to start server: Unable to bind 192.0.2.123:4100'
    );
  });
});
