import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { getClaudeProjectsDirForAnalytics } from '../../../src/utils/claude-config-path';

const ENV = ['CCS_HOME', 'CCS_DIR', 'CLAUDE_CONFIG_DIR'] as const;
let home: string;
let saved: Record<string, string | undefined>;

beforeEach(() => {
  saved = Object.fromEntries(ENV.map((name) => [name, process.env[name]]));
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'claude-projects-'));
  process.env.CCS_HOME = home;
  process.env.CCS_DIR = path.join(home, '.ccs');
  delete process.env.CLAUDE_CONFIG_DIR;
});

afterEach(() => {
  for (const name of ENV) {
    const value = saved[name];
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
  fs.rmSync(home, { recursive: true, force: true });
});

describe('the Claude projects directory usage readers scan', () => {
  it('is the canonical Claude directory when no override is set', () => {
    expect(getClaudeProjectsDirForAnalytics()).toBe(path.join(home, '.claude', 'projects'));
  });

  it('follows CLAUDE_CONFIG_DIR, so Analytics reads the logs Home reads', () => {
    const override = path.join(home, 'elsewhere', 'claude');
    process.env.CLAUDE_CONFIG_DIR = override;
    expect(getClaudeProjectsDirForAnalytics()).toBe(path.join(override, 'projects'));
  });

  it('stays canonical when the override names a CCS account instance', () => {
    // Instance directories are scanned as their own source; scanning one as the default too
    // would count its usage twice.
    process.env.CLAUDE_CONFIG_DIR = path.join(home, '.ccs', 'instances', 'party');
    expect(getClaudeProjectsDirForAnalytics()).toBe(path.join(home, '.claude', 'projects'));
  });
});
