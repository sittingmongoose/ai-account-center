import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { handleUsageHubCommand } from '../../../src/commands/usage-hub-command';
import {
  isUsageHubKeyShape,
  readUsageHubKeyState,
  usageHubKeyMatches,
  usageHubKeyPath,
} from '../../../src/web-server/usage-hub/usage-hub-key-store';

let tempHome = '';
let previous: Record<'CCS_HOME' | 'CCS_DIR', string | undefined> = {
  CCS_HOME: undefined,
  CCS_DIR: undefined,
};

beforeEach(() => {
  tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-usage-hub-cli-'));
  previous = { CCS_HOME: process.env.CCS_HOME, CCS_DIR: process.env.CCS_DIR };
  delete process.env.CCS_DIR;
  process.env.CCS_HOME = tempHome;
});

afterEach(() => {
  for (const [name, value] of Object.entries(previous)) {
    if (value === undefined) delete process.env[name];
    else process.env[name] = value;
  }
  fs.rmSync(tempHome, { recursive: true, force: true });
});

async function run(...args: string[]) {
  let out = '';
  let err = '';
  const code = await handleUsageHubCommand(args, {
    out: (text) => {
      out += text;
    },
    err: (text) => {
      err += text;
    },
  });
  return { code, out, err };
}

describe('ai-account-center dashboard usage-hub', () => {
  it('refuses to generate a key nobody would see', async () => {
    const result = await run('generate');
    expect(result.code).toBe(1);
    expect(result.out).toBe('');
    expect(result.err).toContain('--stdout');
    expect(fs.existsSync(usageHubKeyPath())).toBe(false);
  });

  it('writes only the key to stdout, once, and stores only its hash', async () => {
    const result = await run('generate', '--stdout');
    expect(result.code).toBe(0);
    const key = result.out.trim();
    expect(result.out).toBe(`${key}\n`);
    expect(isUsageHubKeyShape(key)).toBe(true);
    expect(result.err).not.toContain(key);
    expect(fs.readFileSync(usageHubKeyPath(), 'utf8')).not.toContain(key);
    const state = await readUsageHubKeyState();
    if (state.state !== 'on') throw new Error('expected on');
    expect(usageHubKeyMatches(key, state.record)).toBe(true);

    const status = await run('status');
    expect(status.code).toBe(0);
    expect(status.out).toContain('Usage hub: on');
    expect(status.out).not.toContain(key);
    expect(status.out).toContain('http://127.0.0.1:');

    const again = await run('generate', '--print-once');
    expect(again.code).toBe(1);
    expect(again.out).toBe('');
    expect(again.err).toContain('rotate');
  });

  it('rotates and turns off', async () => {
    const first = (await run('generate', '--stdout')).out.trim();
    const rotated = await run('rotate', '--stdout');
    expect(rotated.code).toBe(0);
    const second = rotated.out.trim();
    expect(second).not.toBe(first);
    const state = await readUsageHubKeyState();
    if (state.state !== 'on') throw new Error('expected on');
    expect(usageHubKeyMatches(first, state.record)).toBe(false);
    expect(usageHubKeyMatches(second, state.record)).toBe(true);

    expect((await run('off')).code).toBe(0);
    expect(await readUsageHubKeyState()).toEqual({ state: 'off' });
    expect((await run('status')).out).toContain('Usage hub: off');
  });

  it('rejects unknown commands and arguments', async () => {
    expect((await run('show-key')).code).toBe(1);
    expect((await run('status', '--stdout')).code).toBe(1);
    expect((await run('off', 'now')).code).toBe(1);
    const help = await run('--help');
    expect(help.code).toBe(0);
    expect(help.out).toContain('Add hub');
  });
});
