import { describe, expect, test } from 'bun:test';

import { ROOT_COMMAND_CATALOG, getPublicRootCommands } from '../../../src/commands/command-catalog';
import { ROOT_COMMAND_ROUTES } from '../../../src/commands/root-command-router';

const RETAINED_COMMANDS = [
  'antigravity',
  'bar',
  'codex-auth',
  'config',
  'dashboard',
  'help',
  'version',
];
const RETIRED_COMMANDS = [
  'api',
  'auth',
  'browser',
  'cleanup',
  'cliproxy',
  'copilot',
  'cursor',
  'doctor',
  'docker',
  'env',
  'migrate',
  'persist',
  'proxy',
  'setup',
  'sync',
  'tokens',
  'update',
];

function commandTokens(entries = ROOT_COMMAND_CATALOG): string[] {
  return [...new Set(entries.flatMap((entry) => [entry.name, ...(entry.aliases || [])]))];
}

describe('retained command catalog', () => {
  test('publishes exactly the retained root commands, including the config alias', () => {
    const publicCommands = commandTokens(getPublicRootCommands()).filter(
      (token) => !token.startsWith('-')
    );

    expect(publicCommands.sort()).toEqual(RETAINED_COMMANDS);
    for (const entry of getPublicRootCommands()) {
      expect(RETAINED_COMMANDS).toContain(entry.name);
    }
  });

  test('covers every retained root route and alias', () => {
    const catalogTokens = new Set(commandTokens());

    for (const route of ROOT_COMMAND_ROUTES) {
      // The explicit update route only reports retirement and is never advertised.
      if (route.name === 'update') {
        expect(catalogTokens.has(route.name)).toBe(false);
        continue;
      }
      expect(catalogTokens.has(route.name)).toBe(true);
      for (const alias of route.aliases || []) {
        expect(catalogTokens.has(alias)).toBe(true);
      }
    }
  });

  test('does not retain retired profile or provider commands in the catalog', () => {
    const catalogTokens = new Set(commandTokens());

    for (const command of RETIRED_COMMANDS) {
      expect(catalogTokens.has(command)).toBe(false);
      expect(catalogTokens.has(`--${command}`)).toBe(false);
    }
    for (const provider of ['claude', 'codex', 'gemini', 'grok', 'kiro', 'qwen', 'gitlab']) {
      expect(catalogTokens.has(provider)).toBe(false);
    }
  });

  test('keeps operational hooks outside the public command list', () => {
    const publicTokens = commandTokens(getPublicRootCommands());

    for (const token of ['--install', '--uninstall', '__complete', '--shell-completion', '-sc']) {
      expect(publicTokens).not.toContain(token);
    }
  });
});
