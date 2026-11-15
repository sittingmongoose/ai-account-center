/**
 * L1 lifecycle through the real bridge and lifecycle code, against a stand-in service on a real
 * Unix socket (fixtures/fake-aac-http.mjs; the VM builds no Rust). The same behaviours run against
 * the Nas1-built binary in tests/spec/rust-lifecycle.mjs. Each failed against 7bc46ca90.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import type { Request, Response } from 'express';
import { RustService } from '../../../src/web-server/rust-service';
import { shortName } from '../../../src/web-server/rust-service-lifecycle';

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
  process.env.CCS_HOME = saved.home;
  process.env.CCS_DIR = saved.dir;
  fs.rmSync(scratch, { recursive: true, force: true });
});

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

  it('opens a route breaker after three timeouts and probes again after 30 s', async () => {
    let now = 1_000_000;
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
