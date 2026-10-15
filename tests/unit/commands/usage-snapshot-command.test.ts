/** `dashboard usage-snapshot set`: folder checks. Temporary CCS_HOME and folders only. */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { handleUsageSnapshotCommand } from '../../../src/commands/usage-snapshot-command';
import { readSnapshotSettings } from '../../../src/web-server/agent-usage/agent-usage-snapshot';

let home = '';
let saved: Record<'CCS_HOME' | 'CCS_DIR', string | undefined> = {
  CCS_HOME: undefined,
  CCS_DIR: undefined,
};

beforeEach(() => {
  saved = { CCS_HOME: process.env.CCS_HOME, CCS_DIR: process.env.CCS_DIR };
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-usage-snapshot-cmd-'));
  delete process.env.CCS_DIR;
  process.env.CCS_HOME = home;
  fs.mkdirSync(path.join(home, '.ccs'));
});

afterEach(() => {
  for (const [name, value] of Object.entries(saved)) {
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
  fs.rmSync(home, { recursive: true, force: true });
});

function io() {
  const out: string[] = [];
  const err: string[] = [];
  return { out, err, io: { out: (t: string) => out.push(t), err: (t: string) => err.push(t) } };
}

describe('usage-snapshot set', () => {
  it('takes a folder written with a trailing separator as that folder', async () => {
    fs.mkdirSync(path.join(home, 'share'));
    const folder = path.join(home, 'share', 'aac-usage');
    const run = io();
    expect(await handleUsageSnapshotCommand(['set', folder + path.sep], run.io)).toBe(0);
    expect(run.err.join('')).not.toContain('[X]');
    expect((await readSnapshotSettings())?.directory).toBe(folder);
  });

  it('still refuses a relative folder or one with ".."', async () => {
    const parent = [home, 'share', '..', 'aac-usage'].join(path.sep);
    for (const folder of ['share/aac-usage', parent]) {
      const run = io();
      expect(await handleUsageSnapshotCommand(['set', folder], run.io)).toBe(1);
      expect(run.err.join('')).toContain('without ".."');
    }
  });
});
