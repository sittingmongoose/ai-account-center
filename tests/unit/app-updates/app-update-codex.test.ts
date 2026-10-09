import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'fs';
import os from 'os';
import path from 'path';
const bridge = require('../../../scripts/app-updates/app_update_codex.cjs');
const directories: string[] = [];
afterEach(() => {
  for (const value of directories.splice(0)) fs.rmSync(value, { recursive: true, force: true });
});
beforeEach(() =>
  Object.assign(bridge.output, {
    appId: 'codex-cli',
    platform: 'ubuntu',
    status: 'failed',
    previousVersion: null,
    version: null,
    manager: 'native',
    messageCode: 'update_failed',
    updateAttempted: false,
    restartedProcesses: 0,
  })
);
function fixture() {
  let clock = 0;
  let version = '1.0.0';
  let pending = false;
  const events: string[] = [];
  let busy = 2;
  return {
    events,
    deps: {
      now: () => clock,
      sleep: async (ms: number) => {
        clock += ms;
        events.push('wait');
      },
      readVersion: async () => version,
      update: async () => {
        version = '2.0.0';
        events.push('update');
      },
      pending: () => pending,
      mark: () => {
        pending = true;
        events.push('mark');
      },
      clear: () => {
        pending = false;
        events.push('clear');
      },
      restartProxies: async () => {
        events.push('proxy-reconnect');
      },
      hasOldProxies: async () => false,
      runtime: () => ({
        stop: async () => {
          events.push('stop');
          if (busy-- > 0) throw Object.assign(new Error('private'), { code: 'busy' });
        },
        start: async () => {
          events.push('start');
        },
        dispose: async () => {
          events.push('dispose');
        },
      }),
    },
    pending: () => pending,
  };
}
describe('Codex update idle and restart coordinator', () => {
  it('queues busy work, then reconnects proxies under stop/start protection', async () => {
    const value = fixture();
    const result = await bridge.execute(value.deps);
    expect(result.status).toBe('updated');
    expect(value.events).toEqual([
      'update',
      'mark',
      'stop',
      'wait',
      'stop',
      'wait',
      'stop',
      'proxy-reconnect',
      'start',
      'clear',
      'dispose',
    ]);
    expect(value.pending()).toBe(false);
  });
  it('retains pending restart after a failure so a later explicit click retries', async () => {
    const value = fixture();
    value.deps.runtime = () => ({
      stop: async () => {},
      start: async () => {
        throw new Error('PRIVATE_SENTINEL');
      },
      dispose: async () => {},
    });
    const result = await bridge.execute(value.deps);
    expect(result.status).toBe('restart_failed');
    expect(value.pending()).toBe(true);
    expect(JSON.stringify(result)).not.toContain('PRIVATE_SENTINEL');
  });
  it('does not stop a current package without pending restart', async () => {
    const value = fixture();
    value.deps.update = async () => {
      value.events.push('update');
    };
    const result = await bridge.execute(value.deps);
    expect(result.status).toBe('current');
    expect(value.events).toEqual(['update']);
  });
  it('gives up on a Codex that stays busy after about a minute, never fifteen', async () => {
    // The live 2026-10-06 run waited 14.8 minutes here while Mac and Windows queued.
    const value = fixture();
    let stops = 0;
    value.deps.runtime = () => ({
      stop: async () => {
        stops++;
        throw Object.assign(new Error('PRIVATE_SENTINEL'), { code: 'busy' });
      },
      start: async () => {
        value.events.push('start');
      },
      dispose: async () => {},
    });
    const before = value.deps.now();
    const result = await bridge.execute(value.deps);
    const waited = value.deps.now() - before;
    expect(waited).toBeLessThanOrEqual(60_000);
    expect(waited).toBeGreaterThanOrEqual(55_000);
    expect(stops).toBeLessThanOrEqual(14);
    expect(result.status).toBe('action_required');
    expect(result.messageCode).toBe('codex_busy');
    expect(result.version).toBe('2.0.0');
    // Nothing was stopped, so nothing restarts; the marker stays for the next click.
    expect(value.events).not.toContain('start');
    expect(value.pending()).toBe(true);
    expect(JSON.stringify(result)).not.toContain('PRIVATE_SENTINEL');
  });
  it('honours a shorter idle budget', async () => {
    const value = fixture();
    value.deps.runtime = () => ({
      stop: async () => {
        throw Object.assign(new Error('busy'), { code: 'busy' });
      },
      start: async () => {},
      dispose: async () => {},
    });
    const result = await bridge.execute({ ...value.deps, idleSeconds: 10 });
    expect(value.deps.now()).toBeLessThanOrEqual(10_000);
    expect(result.messageCode).toBe('codex_busy');
  });
  it('reports an unreplaced proxy as a restart failure', async () => {
    const value = fixture();
    value.deps.hasOldProxies = async () => true;
    const result = await bridge.execute(value.deps);
    expect(result.status).toBe('restart_failed');
    expect(value.pending()).toBe(true);
  });
});
describe('Codex update runtime resolution', () => {
  function write(file: string, text: string): string {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
    return file;
  }
  /** A flat remote helper folder holding the bundle, and an installed AAC package. */
  function layout() {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-codex-runtime-'));
    directories.push(root);
    const bundle = path.join(root, 'app-updates', 'app_update_codex_runtime.cjs');
    const installed = path.join(root, 'pkg', 'dist', 'codex-auth', 'codex-activation-runtime.js');
    write(installed, "exports.createCodexActivationRuntime = () => 'installed';\n");
    write(
      path.join(root, 'pkg', 'node_modules', 'proper-lockfile', 'index.js'),
      "exports.lock = 'installed-lock';\n"
    );
    return { bundle, installed };
  }

  it('tries the bundle beside the helper first, then the installed packages', () => {
    const helpers = path.dirname(
      require.resolve('../../../scripts/app-updates/app_update_codex.cjs')
    );
    const packages = path.join(os.homedir(), '.local', 'lib', 'node_modules');
    expect(bridge.modules[0]).toBe(path.join(helpers, 'app_update_codex_runtime.cjs'));
    expect(bridge.modules[1]).toBe(
      path.resolve(helpers, '..', '..', 'dist', 'codex-auth', 'codex-activation-runtime.js')
    );
    expect(bridge.modules.slice(2)).toEqual([
      path.join(
        packages,
        '@sittingmongoose',
        'ai-account-center',
        'dist',
        'codex-auth',
        'codex-activation-runtime.js'
      ),
      path.join(
        packages,
        '@kaitranntt',
        'ccs',
        'dist',
        'codex-auth',
        'codex-activation-runtime.js'
      ),
    ]);
  });

  it('loads the bundle and its own lock library when the bundle is present', () => {
    const { bundle, installed } = layout();
    write(
      bundle,
      "exports.createCodexActivationRuntime = () => 'bundle';\nexports.lockfile = { lock: 'bundle-lock' };\n"
    );
    const loaded = bridge.loadRuntime([bundle, installed]);
    expect(loaded.selected).toBe(bundle);
    expect(loaded.createCodexActivationRuntime()).toBe('bundle');
    expect(loaded.lockfile.lock).toBe('bundle-lock');
  });

  it('falls back to the installed package and its node_modules lock library', () => {
    const { bundle, installed } = layout();
    const loaded = bridge.loadRuntime([bundle, installed]);
    expect(loaded.selected).toBe(installed);
    expect(loaded.createCodexActivationRuntime()).toBe('installed');
    expect(loaded.lockfile.lock).toBe('installed-lock');
  });

  it('finds nothing when no runtime exists', () => {
    const { bundle } = layout();
    expect(bridge.loadRuntime([bundle])).toBeNull();
  });
});

describe('Codex CLI update command', () => {
  /** npm's layout under a prefix: bin/codex links into lib/node_modules/@openai/codex/bin/codex.js. */
  function npmPrefix() {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-codex-npm-'));
    directories.push(root);
    const prefix = path.join(root, 'prefix');
    const script = path.join(prefix, 'lib', 'node_modules', '@openai', 'codex', 'bin', 'codex.js');
    fs.mkdirSync(path.dirname(script), { recursive: true });
    fs.writeFileSync(script, '#!/usr/bin/env node\n');
    const link = path.join(prefix, 'bin', 'codex');
    fs.mkdirSync(path.dirname(link), { recursive: true });
    fs.symlinkSync('../lib/node_modules/@openai/codex/bin/codex.js', link);
    return { root, prefix, link };
  }

  it('keeps `codex update` for a native install', () => {
    const { link } = npmPrefix();
    expect(bridge.updateCommand(link, null)).toEqual({ file: link, args: ['update'] });
  });

  it('updates an npm-global install with npm itself, under the prefix that owns it', () => {
    const { prefix, link } = npmPrefix();
    expect(bridge.updateCommand(link, { node: '/fixture/node', cli: '/fixture/npm-cli.js', prefix })).toEqual({
      file: '/fixture/node',
      args: ['/fixture/npm-cli.js', 'install', '--global', '--prefix', prefix, '@openai/codex@latest'],
    });
  });

  it('refuses a prefix that does not own the executable, so nothing else is updated', () => {
    const { link } = npmPrefix();
    const other = npmPrefix();
    expect(() =>
      bridge.updateCommand(link, { node: '/fixture/node', cli: '/fixture/npm-cli.js', prefix: other.prefix })
    ).toThrow('npm install does not own this Codex.');
  });

  it('refuses relative npm paths and a prefix without its Codex package', () => {
    const { prefix, link } = npmPrefix();
    expect(() => bridge.updateCommand(link, { node: 'node', cli: '/fixture/npm-cli.js', prefix })).toThrow(
      'npm install is unknown.'
    );
    const empty = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-codex-npm-empty-')), 'prefix');
    directories.push(path.dirname(empty));
    fs.mkdirSync(empty, { recursive: true });
    expect(() => bridge.updateCommand(link, { node: '/fixture/node', cli: '/fixture/npm-cli.js', prefix: empty })).toThrow();
  });
});
