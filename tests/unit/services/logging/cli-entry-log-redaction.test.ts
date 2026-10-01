import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { createEmptyUnifiedConfig } from '../../../../src/config/unified-config-types';
import { saveUnifiedConfig } from '../../../../src/config/unified-config-loader';
import {
  clearRecentLogEntries,
  getRecentLogEntries,
} from '../../../../src/services/logging/log-buffer';
import { invalidateLoggingConfigCache } from '../../../../src/services/logging/log-config';
import * as cliBootstrap from '../../../../src/commands/cli-bootstrap';
import * as errors from '../../../../src/errors';

let originalArgv: string[] = [];
let originalCcsHome: string | undefined;
let originalExitCode: typeof process.exitCode;
let tempHome = '';
let bootstrapCalls: string[][] = [];
let bootstrapSawLifecycleLog = false;
const restoreSpies: Array<() => void> = [];
let baselineSigintListeners: Array<(...args: unknown[]) => void> = [];
let baselineSigtermListeners: Array<(...args: unknown[]) => void> = [];
let baselineUncaughtExceptionListeners: Array<(...args: unknown[]) => void> = [];
let baselineUnhandledRejectionListeners: Array<(...args: unknown[]) => void> = [];

function removeNewListeners(
  event: 'SIGINT' | 'SIGTERM' | 'uncaughtException' | 'unhandledRejection',
  baseline: Array<(...args: unknown[]) => void>
): void {
  for (const listener of process.listeners(event)) {
    if (!baseline.includes(listener as (...args: unknown[]) => void)) {
      process.removeListener(event, listener as (...args: unknown[]) => void);
    }
  }
}

beforeEach(() => {
  originalArgv = process.argv.slice();
  originalCcsHome = process.env.CCS_HOME;
  originalExitCode = process.exitCode;
  process.exitCode = 0;
  bootstrapCalls = [];
  bootstrapSawLifecycleLog = false;
  tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-cli-entry-log-'));
  process.env.CCS_HOME = tempHome;
  process.argv = [
    'bun',
    'src/ccs.ts',
    'launch',
    '--api-key',
    'secret-key',
    '--mode',
    'prod',
    '--secret',
    'top-secret',
  ];

  clearRecentLogEntries();
  invalidateLoggingConfigCache();
  const config = createEmptyUnifiedConfig();
  config.logging = {
    ...config.logging,
    enabled: true,
    level: 'debug',
    redact: false,
  };
  saveUnifiedConfig(config);
  invalidateLoggingConfigCache();

  baselineSigintListeners = process.listeners('SIGINT');
  baselineSigtermListeners = process.listeners('SIGTERM');
  baselineUncaughtExceptionListeners = process.listeners('uncaughtException');
  baselineUnhandledRejectionListeners = process.listeners('unhandledRejection');

  const errorSpy = spyOn(errors, 'handleError').mockImplementation(() => {});
  const cleanupSpy = spyOn(errors, 'runCleanup').mockImplementation(() => {});
  const bootstrapSpy = spyOn(cliBootstrap, 'prepareCliArguments').mockImplementation(
    async (args: string[]) => {
      bootstrapCalls.push([...args]);
      bootstrapSawLifecycleLog = getRecentLogEntries().some(
        (entry) => entry.source === 'cli:entry'
      );
      return { exitNow: false, args: ['codex-auth', 'show'] };
    }
  );
  restoreSpies.push(...[errorSpy, cleanupSpy, bootstrapSpy].map((spy) => () => spy.mockRestore()));
});

afterEach(() => {
  for (const restore of restoreSpies.splice(0).reverse()) restore();
  process.argv = originalArgv;
  process.exitCode = originalExitCode ?? 0;
  if (originalCcsHome === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = originalCcsHome;

  removeNewListeners('SIGINT', baselineSigintListeners);
  removeNewListeners('SIGTERM', baselineSigtermListeners);
  removeNewListeners('uncaughtException', baselineUncaughtExceptionListeners);
  removeNewListeners('unhandledRejection', baselineUnhandledRejectionListeners);

  fs.rmSync(tempHome, { recursive: true, force: true });
  clearRecentLogEntries();
  invalidateLoggingConfigCache();
});

async function loadCliEntryModule(): Promise<void> {
  await import(`../../../../src/ccs?test=${Date.now()}-${Math.random()}`);
  const deadline = Date.now() + 1000;
  while (
    !getRecentLogEntries().some(
      (entry) => entry.source === 'cli:entry' && entry.event === 'cli.command.complete'
    ) &&
    Date.now() < deadline
  ) {
    await new Promise<void>((resolve) => setImmediate(resolve));
  }
}

describe('CLI entry log redaction', () => {
  it('redacts sensitive argv values before emitting the lifecycle start log', async () => {
    await loadCliEntryModule();

    const startEntry = getRecentLogEntries().find(
      (entry) => entry.source === 'cli:entry' && entry.event === 'cli.command.start'
    );
    const completeEntry = getRecentLogEntries().find(
      (entry) => entry.source === 'cli:entry' && entry.event === 'cli.command.complete'
    );

    expect(startEntry).toBeDefined();
    expect(startEntry?.context).toEqual({
      argv: ['launch', '--api-key', '[redacted]', '--mode', 'prod', '--secret', '[redacted]'],
    });
    expect(startEntry?.requestId).toBeTruthy();
    expect(completeEntry).toBeDefined();
    expect(completeEntry?.requestId).toBe(startEntry?.requestId);
    expect(bootstrapCalls).toEqual([process.argv.slice(2)]);
    expect(bootstrapSawLifecycleLog).toBe(false);

    const serializedStartEntry = JSON.stringify(startEntry);
    expect(serializedStartEntry).not.toContain('secret-key');
    expect(serializedStartEntry).not.toContain('top-secret');
  });
});
