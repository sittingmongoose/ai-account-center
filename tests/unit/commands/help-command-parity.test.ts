import { afterEach, describe, expect, spyOn, test } from 'bun:test';

import {
  handleHelpCommand,
  handleHelpRoute,
  getRootHelpVisibleCommands,
} from '../../../src/commands/help-command';
import * as configHelp from '../../../src/commands/config-command-options';
import * as codexAuthHelp from '../../../src/codex-auth/codex-auth-help';
import * as barHelp from '../../../src/commands/bar/help-subcommand';
import * as antigravityCommand from '../../../src/commands/antigravity-command';

const RETAINED_COMMANDS = [
  'dashboard',
  'config',
  'codex-auth',
  'antigravity',
  'bar',
  'help',
  'version',
];
const RETIRED_TARGETS = [
  'profiles',
  'providers',
  'kiro',
  'browser',
  'completion',
  'targets',
  'api',
  'auth',
  'cliproxy',
  'proxy',
  'cursor',
  'copilot',
  'docker',
  'setup',
  'doctor',
  'env',
  'persist',
  'tokens',
  'migrate',
  'update',
  'sync',
  'cleanup',
];

function stripAnsi(input: string): string {
  return input.replace(/\u001b\[[0-9;]*m/g, '');
}

async function renderLines(
  render: (writeLine: (line: string) => void) => Promise<void>
): Promise<string> {
  const lines: string[] = [];
  await render((line) => lines.push(line));
  return stripAnsi(lines.join('\n'));
}

const originalExitCode = process.exitCode ?? 0;
afterEach(() => {
  process.exitCode = originalExitCode;
});

describe('retained help surface', () => {
  test('root help is compact and advertises the retained commands', async () => {
    const rendered = await renderLines((writeLine) => handleHelpCommand(writeLine));
    const visibleLines = rendered.split('\n').filter((line) => line.trim().length > 0);

    expect(visibleLines.length).toBeLessThanOrEqual(90);
    for (const command of RETAINED_COMMANDS) {
      expect(rendered).toContain(command);
    }
    for (const command of getRootHelpVisibleCommands()) {
      expect(RETAINED_COMMANDS).toContain(command);
      expect(rendered).toContain(command);
    }
    expect(getRootHelpVisibleCommands()).toEqual(
      expect.arrayContaining(['dashboard', 'codex-auth', 'bar', 'help', 'version'])
    );
  });

  test('root help does not advertise retired commands, topics, or runtime flags', async () => {
    const rendered = await renderLines((writeLine) => handleHelpCommand(writeLine));

    for (const target of RETIRED_TARGETS) {
      expect(rendered).not.toMatch(
        new RegExp(`\\b(?:ccs|ai-account-center)(?: help)? ${target}(?:\\s|$)`)
      );
      expect(rendered).not.toMatch(new RegExp(`^\\s*${target}(?:\\s|$)`, 'm'));
    }
    for (const provider of ['claude', 'codex', 'gemini', 'grok', 'qwen', 'gitlab']) {
      expect(rendered).not.toMatch(new RegExp(`\\b(?:ccs|ai-account-center) ${provider}(?:\\s|$)`));
    }
    for (const retiredFlag of [
      '--target',
      '--effort',
      '--browser',
      '--shell-completion',
      '--dev',
    ]) {
      expect(rendered).not.toContain(retiredFlag);
    }
    expect(rendered).not.toContain('ccs <profile>');
  });

  test('empty help target renders the same root help', async () => {
    const rootHelp = await renderLines((writeLine) => handleHelpCommand(writeLine));
    const routeHelp = await renderLines((writeLine) => handleHelpRoute([], writeLine));

    expect(routeHelp).toBe(rootHelp);
  });

  for (const target of ['dashboard', 'config']) {
    test(`${target} help delegates to dashboard configuration help`, async () => {
      const showHelp = spyOn(configHelp, 'showConfigCommandHelp').mockImplementation(() => {});
      try {
        await handleHelpRoute([target], () => {});
        expect(showHelp).toHaveBeenCalledTimes(1);
      } finally {
        showHelp.mockRestore();
      }
    });
  }

  test('codex-auth help delegates to native Codex account help', async () => {
    const showHelp = spyOn(codexAuthHelp, 'printCodexAuthHelp').mockImplementation(() => {});
    try {
      await handleHelpRoute(['codex-auth'], () => {});
      expect(showHelp).toHaveBeenCalledTimes(1);
    } finally {
      showHelp.mockRestore();
    }
  });

  test('antigravity help prints the sign-in command without starting anything', async () => {
    const lines: string[] = [];
    await handleHelpRoute(['antigravity'], (line) => lines.push(line));
    expect(lines.join('\n')).toContain('ai-account-center antigravity signin <profile>');
    expect(await antigravityCommand.handleAntigravityCommand(['help'], () => {})).toBe(0);
    const errors: string[] = [];
    expect(
      await antigravityCommand.handleAntigravityCommand(['login'], (l) => errors.push(l))
    ).toBe(1);
    expect(
      await antigravityCommand.handleAntigravityCommand(['signin'], (l) => errors.push(l))
    ).toBe(1);
    expect(
      await antigravityCommand.handleAntigravityCommand(['signin', '--force'], (l) =>
        errors.push(l)
      )
    ).toBe(1);
    expect(
      await antigravityCommand.handleAntigravityCommand(['status', 'extra'], (l) => errors.push(l))
    ).toBe(1);
    expect(errors.length).toBe(4);
    expect(lines.join('\n')).toContain('ai-account-center antigravity status');
  });

  test('bar help delegates to menu bar help without launching it', async () => {
    const showHelp = spyOn(barHelp, 'showHelp').mockImplementation(async () => {});
    try {
      await handleHelpRoute(['bar'], () => {});
      expect(showHelp).toHaveBeenCalledTimes(1);
    } finally {
      showHelp.mockRestore();
    }
  });

  for (const target of RETIRED_TARGETS) {
    test(`${target} help gives explicit retirement and migration guidance`, async () => {
      const rendered = await renderLines((writeLine) => handleHelpRoute([target], writeLine));

      expect(rendered).toContain(target);
      expect(rendered).toMatch(/retired|removed|no longer supported/i);
      expect(rendered).toMatch(/(?:ccs|ai-account-center) (?:dashboard|config)/);
    });
  }

  test('unknown help target names the problem and retained alternatives', async () => {
    const rendered = await renderLines((writeLine) =>
      handleHelpRoute(['unknown-topic'], writeLine)
    );

    expect(rendered).toContain('unknown-topic');
    expect(rendered).toMatch(/unsupported|unknown/i);
    expect(rendered).toContain('dashboard');
    expect(rendered).toContain('codex-auth');
    expect(rendered).toContain('bar');
    expect(process.exitCode).toBe(1);
  });
});
