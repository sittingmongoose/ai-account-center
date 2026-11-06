import { describe, expect, it } from 'bun:test';
import { handleStoreCommand, storeCommandHelp } from '../../../src/commands/store-command';

function io(env: NodeJS.ProcessEnv = {}) {
  const out: string[] = [];
  const err: string[] = [];
  return {
    io: { out: (t: string) => out.push(t), err: (t: string) => err.push(t), env },
    out,
    err,
  };
}

describe('dashboard store command', () => {
  it('prints its help, and no arguments is a usage error', async () => {
    const help = io();
    expect(await handleStoreCommand(['help'], help.io)).toBe(0);
    expect(help.out.join('')).toBe(storeCommandHelp());
    const none = io();
    expect(await handleStoreCommand([], none.io)).toBe(1);
  });

  it('refuses unknown commands and a restore without exactly one folder', async () => {
    const unknown = io();
    expect(await handleStoreCommand(['wipe'], unknown.io)).toBe(1);
    expect(unknown.err.join('')).toContain('Unknown store command');
    const restore = io();
    expect(await handleStoreCommand(['restore'], restore.io)).toBe(1);
    const extra = io();
    expect(await handleStoreCommand(['status', 'x'], extra.io)).toBe(1);
  });

  it('says plainly when the store is switched off or not built here', async () => {
    const off = io({ AAC_STORE: 'off' });
    expect(await handleStoreCommand(['status'], off.io)).toBe(1);
    expect(off.err.join('')).toContain('not available on this computer (disabled)');
    const missing = io({ AAC_STORE_MODULE: '/nonexistent/aac-store.node' });
    expect(await handleStoreCommand(['status'], missing.io)).toBe(1);
    expect(missing.err.join('')).toContain('(missing)');
  });
});
