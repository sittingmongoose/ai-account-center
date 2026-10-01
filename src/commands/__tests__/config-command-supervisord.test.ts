import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'fs';

import { handleConfigCommand } from '../config-command';
import { resolveNamedCommand } from '../named-command-router';

describe('account dashboard service lifecycle isolation', () => {
  it('starts using dashboard dependencies without CLIProxy or supervisord lifecycle helpers', async () => {
    const logs: string[] = [];
    const originalLog = console.log;
    let serverStarted = false;
    let shutdownRegistered = false;
    let browserOpened = false;
    console.log = (...messages: unknown[]) => {
      logs.push(messages.map(String).join(' '));
    };

    try {
      await handleConfigCommand([], {
        platform: 'linux',
        getPort: async () => 3000,
        openBrowser: async () => {
          browserOpened = true;
          return undefined;
        },
        startServer: async () => {
          serverStarted = true;
          return {
            server: { address: () => ({ address: '127.0.0.1' }) },
            wss: {},
            cleanup: () => {},
          } as never;
        },
        setupGracefulShutdown: () => {
          shutdownRegistered = true;
        },
        getDashboardAuthConfig: () => ({ enabled: true }),
        initUI: async () => {},
        header: (text) => text,
        ok: (text) => text,
        info: (text) => text,
        warn: (text) => text,
        fail: (text) => text,
        resolveNamedCommand,
        configSubcommandRoutes: [],
      });
    } finally {
      console.log = originalLog;
    }

    expect(serverStarted).toBe(true);
    expect(shutdownRegistered).toBe(true);
    expect(browserOpened).toBe(false);
    expect(logs.join('\n')).toContain('Dashboard: http://127.0.0.1:3000');
    expect(logs.join('\n')).not.toMatch(/CLIProxy|supervisord/i);
  });

  it('has no direct imports of the retired provider lifecycle', () => {
    const source = readFileSync(new URL('../config-command.ts', import.meta.url), 'utf8');

    expect(source).not.toMatch(/(?:from\s*|import\s*\()["'][^"']*(?:cliproxy|supervisord)/);
  });
});
