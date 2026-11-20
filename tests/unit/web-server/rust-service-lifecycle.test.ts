/**
 * L1 lifecycle through the real bridge and lifecycle code, against a stand-in service on a real
 * Unix socket (fixtures/fake-aac-http.mjs; the VM builds no Rust). The same behaviours run against
 * the Nas1-built binary in tests/spec/rust-lifecycle.mjs. The original regressions failed
 * against 7bc46ca90; the shared-parent cleanup race failed against d3b27df54.
 */
import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import type { Request, Response } from 'express';
import { RustService } from '../../../src/web-server/rust-service';
import { shortName, sweepStaleBoots } from '../../../src/web-server/rust-service-lifecycle';
import {
  getRecentLogEntries,
  clearRecentLogEntries,
  invalidateLoggingConfigCache,
} from '../../../src/services/logging';
import { rustServiceEnvironment } from '../../../src/web-server/rust-service-env';
import { requestRustAgent } from '../../../src/web-server/rust-service-transport';

const fake = path.join(import.meta.dir, 'fixtures/fake-aac-http.mjs');
let scratch = '';
let ccs = '';
const services: RustService[] = [];
const saved = { home: process.env.CCS_HOME, dir: process.env.CCS_DIR };

function makeRoot(name = '.ccs'): string {
  const root = path.join(scratch, name);
  fs.mkdirSync(root, { recursive: true, mode: 0o700 });
  return fs.realpathSync(root);
}

function useRoot(root: string): void {
  process.env.CCS_HOME = path.dirname(root);
  process.env.CCS_DIR = root;
  ccs = root;
}

function binaryCopy(name = 'aac-http'): string {
  const binary = path.join(scratch, name);
  fs.copyFileSync(fake, binary);
  fs.chmodSync(binary, 0o755);
  return binary;
}

async function started(mode: 'on' | 'shadow', binary: string, options = {}) {
  const service = new RustService(mode, options);
  services.push(service);
  await service.start(binary);
  return service;
}

type Lease = {
  owned: boolean;
  pid: number | null;
  connection: { bootId: string; socketPath: string };
};
const lease = (service: RustService) => (service as unknown as { lease: Lease | null }).lease;

const request = {
  method: 'GET',
  originalUrl: '/api/accounts/settings',
  sessionID: 'fixture-session',
  session: { authenticated: true },
  headers: {},
} as unknown as Request;

async function settings(service: RustService): Promise<unknown> {
  let body: unknown;
  const res = { json: (value: unknown) => (body = value) } as unknown as Response;
  await service.respond(request, res, '/api/accounts/settings', () => ({ ts: true }));
  return body;
}

function writeSettings(text: string): void {
  fs.writeFileSync(path.join(ccs, 'account-refresh-settings.json'), text);
}

async function until(check: () => boolean, ms = 8000): Promise<void> {
  const end = Date.now() + ms;
  while (!check()) {
    if (Date.now() > end) throw new Error('timed out waiting');
    await new Promise((r) => setTimeout(r, 20));
  }
}

beforeEach(() => {
  scratch = fs.mkdtempSync(path.join(process.env.TMPDIR ?? os.tmpdir(), 'aac-l1-'));
  useRoot(makeRoot());
});

afterEach(async () => {
  for (const service of services.splice(0)) service.stop();
  // Let the stopped children exit, then remove the short socket folders a long root made.
  await new Promise((r) => setTimeout(r, 100));
  for (const base of [process.env.XDG_RUNTIME_DIR, `/tmp/aac-${process.getuid?.()}`])
    if (base && fs.existsSync(base))
      for (const name of fs.readdirSync(base, { withFileTypes: true }))
        if (name.name.includes(shortName(ccs)))
          fs.rmSync(path.join(base, name.name), { recursive: true, force: true });
  // The shared /tmp/aac-<uid> folder goes too once empty (the lifecycle makes it again).
  try {
    fs.rmdirSync(`/tmp/aac-${process.getuid?.()}`);
  } catch {
    /* Absent, or another root's folder is in it. */
  }
  restoreEnv('CCS_HOME', saved.home);
  restoreEnv('CCS_DIR', saved.dir);
  invalidateLoggingConfigCache();
  fs.rmSync(scratch, { recursive: true, force: true });
});

/** Assigning `undefined` stores the string "undefined" and later tests write into the checkout. */
function restoreEnv(name: 'CCS_HOME' | 'CCS_DIR', value: string | undefined): void {
  if (value === undefined) delete process.env[name];
  else process.env[name] = value;
}

describe('Rust service lifecycle (L1)', () => {
  it('restarts a killed service after its backoff and serves again', async () => {
    writeSettings('{"refreshIntervalSeconds":90}');
    const service = await started('on', binaryCopy(), { restartDelaysMs: [50, 50, 50, 50, 50] });
    expect(await settings(service)).toEqual({ refreshIntervalSeconds: 90 });
    const first = lease(service);
    expect(first?.owned).toBe(true);
    process.kill(first?.pid as number, 'SIGKILL');
    await until(() => lease(service) !== null && lease(service) !== first);
    expect(lease(service)?.connection.bootId).not.toBe(first?.connection.bootId);
    expect(await settings(service)).toEqual({ refreshIntervalSeconds: 90 });
    expect((service.diagnostics().service as Record<string, unknown>).restarts).toBe(1);
  });

  it('gives up after five restarts in an hour, and TS keeps answering', async () => {
    writeSettings('{"refreshIntervalSeconds":90}');
    const service = await started('on', binaryCopy(), { restartDelaysMs: [20, 20, 20, 20, 20] });
    // The first start, then five restarts; the sixth kill uses up the hour's budget.
    for (let i = 0; i < 6; i++) {
      await until(() => lease(service) !== null);
      const current = lease(service);
      process.kill(current?.pid as number, 'SIGKILL');
      await until(() => lease(service) !== current);
    }
    await new Promise((r) => setTimeout(r, 200));
    expect(lease(service)).toBeNull();
    expect((service.diagnostics().service as Record<string, unknown>).state).toBe('gave-up');
    expect(await settings(service)).toEqual({ ts: true });
  });

  it('refuses to attach to a boot of another build and starts its own (in-place upgrade)', async () => {
    writeSettings('{"refreshIntervalSeconds":90}');
    const binary = binaryCopy();
    const owner = await started('on', binary);
    expect(lease(owner)?.owned).toBe(true);
    // The same binary attaches to the healthy boot without replacing it.
    const same = await started('on', binary);
    expect(lease(same)?.owned).toBe(false);
    expect(lease(same)?.connection.bootId).toBe(lease(owner)?.connection.bootId);
    // An upgrade rewrites the file in place: the old process keeps running the old build.
    fs.appendFileSync(binary, '\n// upgraded\n');
    const upgraded = await started('on', binary);
    expect(lease(upgraded)?.owned).toBe(true);
    expect(lease(upgraded)?.connection.bootId).not.toBe(lease(owner)?.connection.bootId);
    // Both boots still answer, each to its own front door.
    expect(await settings(owner)).toEqual({ refreshIntervalSeconds: 90 });
    expect(await settings(upgraded)).toEqual({ refreshIntervalSeconds: 90 });
  });

  it('starts under a root too long for a Unix socket path', async () => {
    const root = makeRoot(path.join('u'.repeat(60), 'v'.repeat(60), '.ccs'));
    useRoot(root);
    writeSettings('{"refreshIntervalSeconds":120}');
    expect(
      Buffer.byteLength(path.join(root, 'run/aac-http-' + 'a'.repeat(24) + '/http.sock'))
    ).toBeGreaterThan(108);
    const service = await started('on', binaryCopy());
    const socket = lease(service)?.connection.socketPath ?? '';
    expect(Buffer.byteLength(socket)).toBeLessThanOrEqual(100);
    expect(socket).toContain(shortName(root));
    expect(await settings(service)).toEqual({ refreshIntervalSeconds: 120 });
    // The descriptor stays in the root's own run folder.
    expect(fs.existsSync(path.join(root, 'run/aac-http.json'))).toBe(true);
  });

  it('sweeps boot folders left by killed owners, and only those', async () => {
    const run = path.join(ccs, 'run');
    fs.mkdirSync(run, { mode: 0o700 });
    const stale = path.join(run, 'aac-http-' + '1'.repeat(24));
    const fresh = path.join(run, 'aac-http-' + '2'.repeat(24));
    const other = path.join(run, 'keep-me');
    for (const folder of [stale, fresh, other]) fs.mkdirSync(folder, { mode: 0o700 });
    const old = new Date(Date.now() - 5 * 60_000);
    fs.utimesSync(stale, old, old);
    fs.utimesSync(other, old, old);
    await started('on', binaryCopy());
    expect(fs.existsSync(stale)).toBe(false);
    expect(fs.existsSync(fresh)).toBe(true);
    expect(fs.existsSync(other)).toBe(true);
  });

  it('keeps a new short socket parent while the old boot cleans up', async () => {
    useRoot(makeRoot(path.join('p'.repeat(110), '.ccs')));
    writeSettings('{"refreshIntervalSeconds":90}');
    const binary = binaryCopy();
    const old = await started('on', binary);
    const previous = lease(old);
    expect(previous).not.toBeNull();
    const shared = path.dirname(path.dirname(previous!.connection.socketPath));
    expect(shared).toContain(shortName(ccs));
    const temporary = path.join(ccs, 'run', `.aac-http-${previous!.connection.bootId}.json`);
    let allowOld!: () => void, oldPaused!: () => void, removed!: () => void;
    let allowStat!: () => void, nextAtStat!: () => void;
    const oldGate = new Promise<void>((resolve) => {
      allowOld = resolve;
    });
    const paused = new Promise<void>((resolve) => {
      oldPaused = resolve;
    });
    const removedGate = new Promise<void>((resolve) => {
      removed = resolve;
    });
    const statGate = new Promise<void>((resolve) => {
      allowStat = resolve;
    });
    const stating = new Promise<void>((resolve) => {
      nextAtStat = resolve;
    });
    const rm = fs.promises.rm.bind(fs.promises);
    const rmdir = fs.promises.rmdir.bind(fs.promises);
    const lstat = fs.promises.lstat.bind(fs.promises);
    let starting = false;
    let removeCalled = false;
    const rmSpy = spyOn(fs.promises, 'rm').mockImplementation(async (...args) => {
      await rm(...args);
      if (args[0] === temporary) {
        oldPaused();
        await oldGate;
      }
    });
    const dirSpy = spyOn(fs.promises, 'rmdir').mockImplementation(async (...args) => {
      if (args[0] === shared) removeCalled = true;
      try {
        await rmdir(...args);
      } finally {
        if (args[0] === shared) removed();
      }
    });
    const statSpy = spyOn(fs.promises, 'lstat').mockImplementation((async (
      ...args: Parameters<typeof lstat>
    ) => {
      const value = await lstat(...args);
      if (starting && args[0] === shared) {
        nextAtStat();
        await statGate;
      }
      return value;
    }) as typeof fs.promises.lstat);
    let next: RustService | undefined;
    try {
      old.stop();
      // The old boot and descriptor are removed; hold cleanup before its empty-parent rmdir.
      await paused;
      starting = true;
      next = new RustService('on');
      services.push(next);
      const start = next.start(binary);
      // Delay a real lstat result while the next start validates its short shared parent.
      await stating;
      allowOld();
      // Drain cleanup's continuation, without a timer race: without the creation lock it
      // enters rmdir immediately. Let that real removal finish before returning the stat.
      await new Promise<void>((resolve) => setImmediate(resolve));
      const removedBeforeCreation = removeCalled;
      if (removedBeforeCreation) await removedGate;
      allowStat();
      await start;
      expect(lease(next)).not.toBeNull();
      expect(next.stats.fallbacks).toBe(0);
      expect(removedBeforeCreation).toBe(false);
      expect(await settings(next)).toEqual({ refreshIntervalSeconds: 90 });
    } finally {
      allowOld();
      allowStat();
      rmSpy.mockRestore();
      dirSpy.mockRestore();
      statSpy.mockRestore();
      old.stop();
      next?.stop();
    }
  }, 15000);

  for (const scenario of [
    'once',
    'stat',
    'twice',
    'exhausted',
    'public',
    'symlink',
    'owner',
  ] as const)
    it(`revalidates a vanished short parent with bounded retries: ${scenario}`, async () => {
      useRoot(makeRoot(path.join('r'.repeat(110), '.ccs')));
      writeSettings('{"refreshIntervalSeconds":90}');
      const { socketLayout } = await import('../../../src/web-server/rust-service-lifecycle');
      const folders = socketLayout(ccs, '0'.repeat(24))!.folders;
      const shared = folders[folders.length - 2];
      const lstat = fs.promises.lstat.bind(fs.promises);
      let validations = 0;
      const statSpy = spyOn(fs.promises, 'lstat').mockImplementation((async (
        ...args: Parameters<typeof lstat>
      ) => {
        const value = await lstat(...args);
        if (args[0] === shared) {
          validations++;
          if (
            validations === 1 ||
            (scenario === 'twice' && validations === 2) ||
            scenario === 'exhausted'
          ) {
            await fs.promises.rmdir(shared);
            if (scenario === 'stat')
              throw Object.assign(new Error('fixture parent vanished before lstat'), {
                code: 'ENOENT',
              });
          } else if (scenario === 'public') {
            await fs.promises.chmod(shared, 0o755);
            return lstat(...args);
          } else if (scenario === 'symlink') {
            await fs.promises.rmdir(shared);
            await fs.promises.symlink(scratch, shared);
            return lstat(...args);
          } else if (scenario === 'owner') {
            Object.defineProperty(value, 'uid', { value: (process.getuid?.() ?? 0) + 1 });
          }
        }
        return value;
      }) as typeof fs.promises.lstat);
      try {
        const service = await started('on', binaryCopy());
        expect(validations).toBe(scenario === 'twice' || scenario === 'exhausted' ? 3 : 2);
        if (scenario === 'once' || scenario === 'stat' || scenario === 'twice') {
          expect(lease(service)?.owned).toBe(true);
          expect(service.stats.fallbacks).toBe(0);
          expect(await settings(service)).toEqual({ refreshIntervalSeconds: 90 });
        } else {
          expect(lease(service)).toBeNull();
          expect(service.stats.fallbacks).toBe(1);
        }
      } finally {
        statSpy.mockRestore();
        if (scenario === 'symlink') await fs.promises.unlink(shared);
      }
    });

  it('never scans abandoned parents through a symlinked runtime ancestor', async () => {
    const target = path.join(scratch, 'target');
    const runtime = path.join(target, 'runtime');
    fs.mkdirSync(runtime, { recursive: true, mode: 0o700 });
    const ancestor = path.join(scratch, 'ancestor');
    fs.symlinkSync(target, ancestor);
    const candidate = path.join(runtime, 'aac-' + 'a'.repeat(12));
    fs.mkdirSync(candidate, { mode: 0o700 });
    const old = new Date(Date.now() - 24 * 60 * 60_000);
    fs.utimesSync(candidate, old, old);
    const savedRun = process.env.AAC_RUST_RUN_DIR;
    process.env.AAC_RUST_RUN_DIR = path.join(ancestor, 'runtime');
    try {
      expect(await sweepStaleBoots(ccs, null)).toBe(0);
      expect(fs.existsSync(candidate)).toBe(true);
    } finally {
      if (savedRun === undefined) delete process.env.AAC_RUST_RUN_DIR;
      else process.env.AAC_RUST_RUN_DIR = savedRun;
    }
  });

  it('opens a route breaker after three timeouts and probes again after 30 s', async () => {
    let now = 1_000_000;
    // A warning would spawn the Node fixture again as the log file's lease helper.
    // Keep the wall-clock assertion on the breaker path itself.
    fs.writeFileSync(path.join(ccs, 'config.yaml'), 'version: 10\nlogging:\n  enabled: false\n', {
      mode: 0o600,
    });
    invalidateLoggingConfigCache();
    writeSettings('"hang"');
    const service = await started('on', binaryCopy(), { now: () => now });
    for (let i = 0; i < 3; i++) expect(await settings(service)).toEqual({ ts: true });
    const began = Date.now();
    expect(await settings(service)).toEqual({ ts: true });
    expect(Date.now() - began).toBeLessThan(100);
    const route = () =>
      (service.diagnostics().routes as Record<string, Record<string, unknown>>)[
        '/api/accounts/settings'
      ];
    expect(route().failed).toEqual({ deadline: 3 });
    expect(route().unavailable).toEqual({ breaker: 1 });
    writeSettings('{"refreshIntervalSeconds":90}');
    now += 30_000;
    expect(await settings(service)).toEqual({ refreshIntervalSeconds: 90 });
    expect(route().state).toBe('serving');
    // One route timing out is not a process failure: health answered, so the boot stays.
    expect((service.diagnostics().service as Record<string, unknown>).restarts).toBe(0);
  });

  // Review L1: a request admitted to the half-open probe that never dispatches must not keep it.
  it('keeps the half-open probe for a request that really goes to Rust', async () => {
    let now = 1_000_000;
    writeSettings('"hang"');
    const service = await started('on', binaryCopy(), { now: () => now });
    for (let i = 0; i < 3; i++) expect(await settings(service)).toEqual({ ts: true });
    writeSettings('{"refreshIntervalSeconds":90}');
    now += 30_000;
    // Admitted, then refused before any dispatch: an input the builder cannot carry.
    expect(
      await service.compose(
        request,
        '/api/accounts/settings',
        null,
        () => ({ ts: true }),
        () => true
      )
    ).toEqual({ ts: true });
    // ... and an owned read of another root.
    expect(
      await service.ownedValue(
        request,
        '/api/accounts/settings',
        '/elsewhere',
        () => null,
        async () => ({
          ts: true,
        })
      )
    ).toEqual({ ts: true });
    expect(await settings(service)).toEqual({ refreshIntervalSeconds: 90 });
  });

  // Review M1, the reviewer's reproduction: the owned child dies while shadow awaits the TS read.
  for (const helper of ['compose', 'ownedValue'] as const)
    it(`keeps the TS answer and frees its slot when the service dies during the shadow reference (${helper})`, async () => {
      const service = await started('shadow', binaryCopy(), { restartDelaysMs: [60_000] });
      const first = lease(service);
      expect(first?.owned).toBe(true);
      let began!: () => void;
      let finish!: (value: unknown) => void;
      const legacyStarted = new Promise<void>((resolve) => (began = resolve));
      const held = new Promise((resolve) => (finish = resolve));
      const legacy = () => {
        began();
        return held;
      };
      const route = helper === 'compose' ? '/api/views/home' : '/api/accounts/visibility';
      const req = { ...request, originalUrl: route } as unknown as Request;
      const answer =
        helper === 'compose'
          ? service.compose(req, route, { data: {}, presentation: {} }, legacy, () => true)
          : service.ownedValue(
              req,
              route,
              (first as unknown as { connection: { ccsDir: string } }).connection.ccsDir,
              (body) => ({ value: body }),
              legacy as () => Promise<unknown>
            );
      await legacyStarted;
      process.kill(first?.pid as number, 'SIGKILL');
      await until(() => lease(service) === null);
      finish({ ts: 'successful' });
      expect(await answer).toEqual({ ts: 'successful' });
      const inner = service as unknown as { inFlight: number; shadowInFlight: Map<string, number> };
      // The comparison ends in the background; its slot must come back.
      await until(() => (inner.shadowInFlight.get(route) ?? 0) === 0, 2000);
      expect(inner.inFlight).toBe(0);
      const counted = (service.diagnostics().routes as Record<string, Record<string, unknown>>)[
        route
      ];
      expect(counted.unavailable).toEqual({ down: 1 });
    });

  // Review L3: a process that accepts but never answers (here: stopped) is retired and restarted.
  it('retires a hung owned service after sustained timeouts and restarts it', async () => {
    writeSettings('{"refreshIntervalSeconds":90}');
    const service = await started('on', binaryCopy(), { restartDelaysMs: [50, 50, 50, 50, 50] });
    const first = lease(service);
    expect(await settings(service)).toEqual({ refreshIntervalSeconds: 90 });
    process.kill(first?.pid as number, 'SIGSTOP');
    try {
      for (let i = 0; i < 3; i++) expect(await settings(service)).toEqual({ ts: true });
      await until(() => lease(service) !== null && lease(service) !== first);
      expect(lease(service)?.connection.bootId).not.toBe(first?.connection.bootId);
      expect(await settings(service)).toEqual({ refreshIntervalSeconds: 90 });
      expect((service.diagnostics().service as Record<string, unknown>).restarts).toBe(1);
    } finally {
      for (const signal of ['SIGCONT', 'SIGKILL'] as const)
        try {
          process.kill(first?.pid as number, signal);
        } catch {
          /* Already gone. */
        }
    }
  });

  it('names the reason of every decline per route', async () => {
    const service = await started('on', binaryCopy());
    writeSettings('{"refreshIntervalSeconds":5}');
    expect(await settings(service)).toEqual({ ts: true });
    writeSettings('{"refreshIntervalSeconds":"x"}');
    expect(await settings(service)).toEqual({ ts: true });
    await settings(service);
    writeSettings('{"refreshIntervalSeconds":60}');
    expect(await settings(service)).toEqual({ refreshIntervalSeconds: 60 });
    const routes = service.diagnostics().routes as Record<string, Record<string, unknown>>;
    expect(routes['/api/accounts/settings']).toMatchObject({
      state: 'serving',
      served: 1,
      declined: { number: 1, shape: 2 },
    });
    // Declines never open the breaker.
    expect(routes['/api/accounts/settings'].unavailable).toEqual({});
  });

  it('keeps at most two shadow comparisons in flight per route and times both paths', async () => {
    writeSettings('"slow"');
    const service = await started('shadow', binaryCopy());
    const bodies = await Promise.all(Array.from({ length: 5 }, () => settings(service)));
    expect(bodies).toEqual(Array(5).fill({ ts: true }));
    const route = () =>
      (service.diagnostics().routes as Record<string, Record<string, unknown>>)[
        '/api/accounts/settings'
      ];
    expect(route().skipped).toBe(3);
    await until(() => service.stats.shadowed === 2);
    // The fake answers 90, TS answered { ts: true }: a counted difference, never served.
    expect(service.stats.differences).toBe(2);
    expect(service.stats.served).toBe(0);
    writeSettings('{"refreshIntervalSeconds":90}');
    let body: unknown;
    const res = { json: (value: unknown) => (body = value) } as unknown as Response;
    await service.respond(request, res, '/api/accounts/settings', () => ({
      refreshIntervalSeconds: 90,
    }));
    expect(body).toEqual({ refreshIntervalSeconds: 90 });
    await until(() => service.stats.shadowed === 3);
    const paired = route().paired as Record<string, { samples: number; p50Ms: number }>;
    expect(paired.ts.samples).toBe(1);
    expect(paired.rust.samples).toBe(1);
    expect(paired.rust.p50Ms).toBeGreaterThan(0);
  });
});

describe('Rust service environment (L5 review M1)', () => {
  it('deletes an unset CCS_DIR instead of storing the string undefined', () => {
    const saved = process.env.CCS_DIR;
    try {
      delete process.env.CCS_DIR;
      restoreEnv('CCS_DIR', undefined);
      expect(process.env.CCS_DIR).toBeUndefined();
      expect(Object.hasOwn(process.env, 'CCS_DIR')).toBe(false);
    } finally {
      restoreEnv('CCS_DIR', saved);
    }
  });

  it('passes only the allowlisted names a local agent verb reads, transport on', () => {
    const env = rustServiceEnvironment(
      {
        PATH: '/bin',
        HOME: '/fixture/home',
        CCS_DIR: '/fixture/home/.ccs',
        XDG_CONFIG_HOME: '/fixture/home/.config',
        KIMI_API_KEY: 'fixture-only',
        https_proxy: 'http://proxy.invalid:3128',
        CCS_SESSION_SECRET: 'fixture-only',
        NODE_OPTIONS: '--require /fixture/x.js',
        LD_PRELOAD: '/fixture/x.so',
        AAC_UNLISTED: 'x',
      },
      false,
      true
    );
    expect(env).toEqual({
      PATH: '/bin',
      HOME: '/fixture/home',
      CCS_DIR: '/fixture/home/.ccs',
      XDG_CONFIG_HOME: '/fixture/home/.config',
      KIMI_API_KEY: 'fixture-only',
      https_proxy: 'http://proxy.invalid:3128',
    });
  });

  it('keeps provider keys and proxies out while the agent transport is off (Wonder L1)', () => {
    const env = rustServiceEnvironment(
      {
        PATH: '/bin',
        HOME: '/fixture/home',
        CCS_DIR: '/fixture/home/.ccs',
        KIMI_API_KEY: 'fixture-only',
        KIMI_CODE_API_KEY: 'fixture-only',
        ZAI_API_KEY: 'fixture-only',
        ZHIPU_API_KEY: 'fixture-only',
        BAILIAN_TOKEN_PLAN_API_KEY: 'fixture-only',
        OPENCODE_GO_API_KEY: 'fixture-only',
        OPENCODE_API_KEY: 'fixture-only',
        HTTPS_PROXY: 'http://proxy.invalid:3128',
        https_proxy: 'http://proxy.invalid:3128',
        HTTP_PROXY: 'http://proxy.invalid:3128',
        http_proxy: 'http://proxy.invalid:3128',
        ALL_PROXY: 'socks5://proxy.invalid:1080',
        all_proxy: 'socks5://proxy.invalid:1080',
        NO_PROXY: 'localhost',
        no_proxy: 'localhost',
      },
      false
    );
    expect(env).toEqual({
      PATH: '/bin',
      HOME: '/fixture/home',
      CCS_DIR: '/fixture/home/.ccs',
    });
  });

  it('matches Windows names case-insensitively and never passes one twice', () => {
    const env = rustServiceEnvironment(
      {
        Path: 'C:\\Windows',
        SystemRoot: 'C:\\Windows',
        SystemDrive: 'C:',
        UserProfile: 'C:\\Users\\fixture',
        HTTPS_PROXY: 'http://a.invalid',
        https_proxy: 'http://b.invalid',
        SSH_AUTH_SOCK: '\\\\.\\pipe\\fixture-agent',
        Node_Options: '--inspect',
      },
      true,
      true
    );
    expect(env).toEqual({
      Path: 'C:\\Windows',
      SystemRoot: 'C:\\Windows',
      SystemDrive: 'C:',
      UserProfile: 'C:\\Users\\fixture',
      HTTPS_PROXY: 'http://a.invalid',
    });
  });
});

describe('Spawn environment through the real lifecycle (Wonder L1)', () => {
  const keys = ['KIMI_API_KEY', 'HTTPS_PROXY', 'AAC_AGENT_TRANSPORT'] as const;
  const saved: Record<string, string | undefined> = {};
  beforeEach(() => {
    for (const name of keys) saved[name] = process.env[name];
    process.env.KIMI_API_KEY = 'fixture-only';
    process.env.HTTPS_PROXY = 'http://proxy.invalid:3128';
    delete process.env.AAC_AGENT_TRANSPORT;
  });
  afterEach(() => {
    for (const name of keys) restoreEnv(name, saved[name]);
  });

  it('spawns the service without provider keys or proxies while the transport is off', async () => {
    await started('shadow', binaryCopy());
    const names = JSON.parse(fs.readFileSync(path.join(ccs, 'fixture-spawn-env.json'), 'utf8'));
    expect(names).toContain('HOME');
    expect(names).not.toContain('KIMI_API_KEY');
    expect(names).not.toContain('HTTPS_PROXY');
  });

  it('spawns the service with provider keys and proxies when the transport is on', async () => {
    process.env.AAC_AGENT_TRANSPORT = 'on';
    await started('shadow', binaryCopy());
    const names = JSON.parse(fs.readFileSync(path.join(ccs, 'fixture-spawn-env.json'), 'utf8'));
    expect(names).toContain('KIMI_API_KEY');
    expect(names).toContain('HTTPS_PROXY');
  });
});

describe('Symlinked root through the real lookups (Wonder L4)', () => {
  function alias(): string {
    const link = path.join(scratch, 'alias');
    fs.symlinkSync(ccs, link);
    return link;
  }

  it('feedBoot names the boot for a symlinked scope, and still declines another root', async () => {
    const service = await started('on', binaryCopy());
    expect(service.feedBoot(alias())).toBe(lease(service)?.connection.bootId);
    const other = path.join(scratch, 'other');
    fs.mkdirSync(other, { recursive: true });
    expect(service.feedBoot(other)).toBeNull();
  });

  it('ownedValue serves Rust through a symlinked root and keeps another root on TS', async () => {
    const service = await started('on', binaryCopy());
    const accept = (body: unknown) => ({ value: body });
    let legacyRan = false;
    const legacy = async () => {
      legacyRan = true;
      return { ts: true };
    };
    const route = '/api/claude/desktop-profiles';
    const value = await service.ownedValue(request, route, alias(), accept, legacy);
    expect(value).toEqual({ profiles: [] });
    expect(legacyRan).toBe(false);
    const other = path.join(scratch, 'other');
    fs.mkdirSync(other, { recursive: true });
    expect(await service.ownedValue(request, route, other, accept, legacy)).toEqual({ ts: true });
    expect(legacyRan).toBe(true);
  });

  it('requestRustAgent finds the connection through a symlinked root', async () => {
    const service = await started('on', binaryCopy());
    const payload = {
      computer: {},
      args: {},
      deadlineMs: 2000,
      timeoutMs: 2000,
      maxBytes: 65536,
      callId: 'fixture-call',
    };
    const result = await requestRustAgent(alias(), 'inventory', payload);
    expect(result).toBe(JSON.stringify({ ok: true, verb: 'inventory' }));
    const other = path.join(scratch, 'other');
    fs.mkdirSync(other, { recursive: true });
    expect(await requestRustAgent(other, 'inventory', payload)).toBeUndefined();
    expect(lease(service)).not.toBeNull();
  });
});

describe('Shadow envelope cost (Wonder L5)', () => {
  it('bills the input build to the served TS latency and to the Rust path', async () => {
    writeSettings('{"refreshIntervalSeconds":90}');
    const service = await started('shadow', binaryCopy());
    // A frozen clock makes both paired timings exact: 12 ms of input build, 7 ms of legacy.
    const perf = globalThis.performance;
    const realNow = perf.now;
    let clock = realNow.call(perf);
    Object.defineProperty(perf, 'now', { value: () => clock, configurable: true });
    try {
      const body = await service.compose(
        request,
        '/api/accounts/settings',
        () => {
          clock += 12;
          return { snapshot: {} };
        },
        () => {
          clock += 7;
          return { refreshIntervalSeconds: 90 };
        },
        (value) =>
          typeof value === 'object' &&
          value !== null &&
          'refreshIntervalSeconds' in value &&
          typeof value.refreshIntervalSeconds === 'number'
      );
      expect(body).toEqual({ refreshIntervalSeconds: 90 });
      await until(() => service.stats.shadowed === 1);
      const route = (service.diagnostics().routes as Record<string, Record<string, unknown>>)[
        '/api/accounts/settings'
      ];
      const paired = route.paired as Record<string, { samples: number; p50Ms: number }>;
      expect(paired.ts.samples).toBe(1);
      expect(paired.ts.p50Ms).toBe(19);
      expect(paired.rust.samples).toBe(1);
      expect(paired.rust.p50Ms).toBe(12);
    } finally {
      Object.defineProperty(perf, 'now', { value: realNow, configurable: true });
    }
  });
});

it('bounds repeated and alternating comparison logs through the real shadow reply path', async () => {
  writeSettings('{"refreshIntervalSeconds":90}');
  const service = await started('shadow', binaryCopy());
  clearRecentLogEntries();
  for (let n = 0; n < 40; n++) {
    let body: unknown;
    await service.respond(
      request,
      {
        json: (v: unknown) => {
          body = v;
        },
      } as Response,
      '/api/accounts/settings',
      () => (n % 2 ? { refreshIntervalSeconds: 90 } : { fixture: true })
    );
    expect(body).toBeDefined();
    await until(() => service.stats.shadowed === n + 1);
  }
  const logs = getRecentLogEntries().filter(
    (e) => e.event === 'rust.service.shadow_difference' || e.event === 'rust.service.route_state'
  );
  expect(service.stats.differences).toBe(20);
  expect(logs.length).toBeLessThanOrEqual(3);
  (service as unknown as { logMinute(): void }).logMinute();
  const summary = getRecentLogEntries().findLast((e) => e.event === 'rust.service.minute');
  expect(Number(summary?.context.suppressedComparisons)).toBeGreaterThan(30);
}, 20000);

it('stop cancels an acquisition and cannot adopt a late healthy child', async () => {
  const binary = binaryCopy();
  let fake = fs.readFileSync(binary, 'utf8');
  fake = fake.replace(
    'res.end(\'{"status":"ok"}\');',
    'setTimeout(() => res.end(\'{"status":"ok"}\'), 300);'
  );
  fs.writeFileSync(binary, fake);
  const service = new RustService('shadow');
  services.push(service);
  const starting = service.start(binary);
  await new Promise((r) => setTimeout(r, 75));
  service.stop();
  await starting;
  await new Promise((r) => setTimeout(r, 400));
  expect(lease(service)).toBeNull();
  expect(fs.existsSync(path.join(ccs, 'run/aac-http.json'))).toBe(false);
});

it('bounds the whole silent acquisition and closes its unadopted owned child', async () => {
  const binary = binaryCopy();
  const fake = fs
    .readFileSync(binary, 'utf8')
    .replace("if (req.url === '/api/health') {", "if (req.url === '/api/health') { return;");
  fs.writeFileSync(binary, fake);
  const service = new RustService('shadow');
  services.push(service);
  const began = performance.now();
  await service.start(binary);
  expect(performance.now() - began).toBeLessThan(5600);
  expect(lease(service)).toBeNull();
  service.stop();
  await new Promise((r) => setTimeout(r, 200));
  expect(fs.existsSync(path.join(ccs, 'run/aac-http.json'))).toBe(false);
  expect(
    fs.readdirSync(path.join(ccs, 'run')).filter((name) => name.startsWith('aac-http-'))
  ).toEqual([]);
}, 7000);

// r2b-review-L2: the route's `served` counts every Rust success on it; the fed path's own serves
// and named declines must be countable apart from the envelope fallback's successes.
it('counts fed serves and declines apart from the envelope successes on the same route', async () => {
  const service = await started('on', binaryCopy());
  const statusRequest = { ...request, originalUrl: '/api/status' };
  const headers = { 'x-aac-feed': '{"now":1}' };
  const fed = await service.fedView(statusRequest, '/api/status', headers, 'status@1');
  expect(fed && typeof fed === 'object' && fed.view).toBe('status@1');
  const envelope = await service.compose(
    statusRequest,
    '/api/status',
    { snapshot: {} },
    () => ({ ts: true }),
    (body) => (body as { source?: string } | null)?.source === 'envelope'
  );
  expect(envelope).toEqual({ version: 1, source: 'envelope' });
  fs.writeFileSync(path.join(ccs, 'status-fed.json'), '"busy"');
  const declined = await service.fedView(statusRequest, '/api/status', headers, 'status@1');
  expect(declined).toBe('busy');
  const route = (service.diagnostics().routes as Record<string, Record<string, unknown>>)[
    '/api/status'
  ];
  expect(route).toMatchObject({
    state: 'declined:busy',
    served: 2,
    fedServed: 1,
    declined: { busy: 1 },
    fedDeclined: { busy: 1 },
  });
  expect(service.stats.served).toBe(2);
});
