import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import { readFileSync } from 'fs';

import * as configCommand from '../../../src/commands/config-command';
import * as codexAuthCommand from '../../../src/codex-auth/codex-auth-router';
import * as barCommand from '../../../src/commands/bar';
import * as versionCommand from '../../../src/commands/version-command';
import * as helpCommand from '../../../src/commands/help-command';
import {
  requiresRuntimeServices,
  tryHandleRootCommand,
} from '../../../src/commands/root-command-router';

let calls: Array<{ command: string; args: string[] }> = [];
let logLines: string[] = [];
let errorLines: string[] = [];
let originalExitCode: typeof process.exitCode;
const restoreSpies: Array<() => void> = [];

beforeEach(() => {
  calls = [];
  logLines = [];
  errorLines = [];
  originalExitCode = process.exitCode;
  process.exitCode = 0;
  const logSpy = spyOn(console, 'log').mockImplementation((...args: unknown[]) => {
    logLines.push(args.map(String).join(' '));
  });
  const errorSpy = spyOn(console, 'error').mockImplementation((...args: unknown[]) => {
    errorLines.push(args.map(String).join(' '));
  });
  const configSpy = spyOn(configCommand, 'handleConfigCommand').mockImplementation(async (args) => {
    calls.push({ command: 'dashboard', args: [...args] });
  });
  const authSpy = spyOn(codexAuthCommand, 'runCodexAuth').mockImplementation(async (args) => {
    calls.push({ command: 'codex-auth', args: [...args] });
    return 37;
  });
  const barSpy = spyOn(barCommand, 'handleBarCommand').mockImplementation(async (args) => {
    calls.push({ command: 'bar', args: [...args] });
  });
  const versionSpy = spyOn(versionCommand, 'handleVersionCommand').mockImplementation(async () => {
    calls.push({ command: 'version', args: [] });
  });
  const helpSpy = spyOn(helpCommand, 'handleHelpRoute').mockImplementation(async (args) => {
    calls.push({ command: 'help', args: [...args] });
  });
  const rootHelpSpy = spyOn(helpCommand, 'handleHelpCommand').mockImplementation(async () => {
    calls.push({ command: 'help', args: [] });
  });
  const exitSpy = spyOn(process, 'exit').mockImplementation(() => {
    throw new Error('Root commands must return without terminating the process');
  });
  restoreSpies.push(
    ...[
      logSpy,
      errorSpy,
      configSpy,
      authSpy,
      barSpy,
      versionSpy,
      helpSpy,
      rootHelpSpy,
      exitSpy,
    ].map((spy) => () => spy.mockRestore())
  );
});

afterEach(() => {
  for (const restore of restoreSpies.splice(0).reverse()) restore();
  process.exitCode = originalExitCode ?? 0;
});

describe('retained root command routing', () => {
  it.each(['dashboard', 'config'])(
    'passes dashboard arguments unchanged through %s',
    async (command) => {
      const args = ['--host=127.0.0.1', '--port', '4100', '--no-open'];

      await expect(tryHandleRootCommand([command, ...args])).resolves.toBe(true);
      expect(calls).toEqual([{ command: 'dashboard', args }]);
    }
  );

  it('delegates native Codex account arguments and preserves the returned status', async () => {
    await expect(tryHandleRootCommand(['codex-auth', 'activate', 'work'])).resolves.toBe(true);

    expect(calls).toEqual([{ command: 'codex-auth', args: ['activate', 'work'] }]);
    expect(process.exitCode).toBe(37);
  });

  it('passes menu bar arguments unchanged without launching another command', async () => {
    await expect(tryHandleRootCommand(['bar', 'status', '--port', '3999'])).resolves.toBe(true);

    expect(calls).toEqual([{ command: 'bar', args: ['status', '--port', '3999'] }]);
  });

  it.each(['version', '--version', '-v'])('routes a version alias: %s', async (command) => {
    await expect(tryHandleRootCommand([command])).resolves.toBe(true);

    expect(calls).toEqual([{ command: 'version', args: [] }]);
  });

  it('rejects extra version arguments before invoking the version handler', async () => {
    await expect(tryHandleRootCommand(['version', '--force'])).resolves.toBe(true);

    expect(calls).toHaveLength(0);
    expect(process.exitCode).toBe(1);
    expect(errorLines.join('\n')).toContain('Unexpected version arguments: --force');
  });

  it.each(['help', '--help', '-h'])(
    'routes retained help aliases and topics: %s',
    async (command) => {
      await expect(tryHandleRootCommand([command, 'dashboard'])).resolves.toBe(true);

      expect(calls).toEqual([{ command: 'help', args: ['dashboard'] }]);
    }
  );

  it('shows root help for an empty invocation', async () => {
    await expect(tryHandleRootCommand([])).resolves.toBe(true);

    expect(calls).toEqual([{ command: 'help', args: [] }]);
  });

  it.each([['update'], ['--update'], ['update', '--help'], ['--update', '--force', '--beta']])(
    'retires the upstream updater instead of calling any handler: %j',
    async (...args) => {
      await expect(tryHandleRootCommand(args)).resolves.toBe(true);

      expect(calls).toHaveLength(0);
      expect(process.exitCode).toBe(1);
      const rendered = [...logLines, ...errorLines].join('\n');
      expect(rendered).toMatch(/self-update is retired/i);
      expect(rendered).toContain('AI Account Center');
      expect(rendered).toMatch(/own release|own.*checkout/i);
    }
  );

  it.each([
    'auth',
    'api',
    'browser',
    'cliproxy',
    'docker',
    'env',
    'persist',
    'proxy',
    'tokens',
    'migrate',
    'setup',
    'doctor',
    'sync',
    'cleanup',
    '__complete',
    '--shell-completion',
    '--install',
    '--uninstall',
    'glm',
    'cursor',
    'my-private-profile',
  ])('consumes retired commands and profile names with migration guidance: %s', async (command) => {
    await expect(tryHandleRootCommand([command, 'status'])).resolves.toBe(true);

    expect(calls).toHaveLength(0);
    expect(process.exitCode).toBe(1);
    const rendered = [...logLines, ...errorLines].join('\n');
    expect(rendered).toContain(command);
    expect(rendered).toMatch(/retired|no longer/i);
    expect(rendered).toContain('ai-account-center dashboard');
  });

  it('keeps updater imports and child process launches out of the root and entry modules', () => {
    for (const sourcePath of [
      '../../../src/commands/root-command-router.ts',
      '../../../src/ccs.ts',
    ]) {
      const source = readFileSync(new URL(sourcePath, import.meta.url), 'utf8');
      expect(source).not.toMatch(/(?:from\s*|import\s*\()["'][^"']*update-(?:command|checker)/);
      expect(source).not.toMatch(/(?:from\s*|import\s*\()["'](?:node:)?child_process["']/);
      expect(source).not.toMatch(/\b(?:spawn|spawnSync|execFile|execFileSync)\s*\(/);
    }
  });
});

describe('configuration-backed runtime service gate', () => {
  it.each([
    [],
    ['--help'],
    ['--version'],
    ['update', '--force'],
    ['old-profile'],
    ['config', '--help=true'],
    ['config', 'channels'],
    ['config', '--dev'],
    ['config', '--port', 'invalid'],
    ['config', 'auth', '--help'],
    ['codex-auth', '--help'],
    ['codex-auth', 'use', 'saved'],
    ['codex-auth', 'switch', 'saved'],
    ['bar', '--help=true'],
    ['bar', 'version'],
  ])(
    'keeps metadata and retired invocations outside configuration logging: %j',
    async (...args) => {
      expect(await requiresRuntimeServices(args)).toBe(false);
    }
  );

  it.each([
    ['dashboard', '--no-open'],
    ['config', '--port', '4100'],
    ['config', 'auth', 'setup'],
    ['codex-auth', 'activate', 'saved'],
    ['bar', 'status'],
  ])('preserves runtime service setup for account operations: %j', async (...args) => {
    expect(await requiresRuntimeServices(args)).toBe(true);
  });
});
