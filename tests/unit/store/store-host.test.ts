/**
 * R3: the store host's TS half against the fake service (real socket, real bridge and
 * lifecycle code; the store's semantics run against Nas1 binaries in the spec). The hosted
 * open, the per-boot re-open after a restart, the attached refusal and the NAPI flip each
 * have no pre-R3 behavior to pass against: the switch, the registry and the socket handle
 * are new.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { RustService } from '../../../src/web-server/rust-service';
import { rustStoreSwitchOn } from '../../../src/web-server/rust-service-groups';
import {
  notifyRustStoreGaveUp,
  onRustStoreGaveUp,
  registerRustStoreConnection,
  rustStoreHosted,
  setRustStoreHosted,
} from '../../../src/web-server/rust-service-transport';
import {
  closeAacStore,
  openAacStore,
  resetAacStoreForTests,
  storeErrorCode,
  shareBackupEnv,
} from '../../../src/store/store-service';
import { SocketStoreHandle } from '../../../src/store/store-host-client';
import type { NativeLoadResult } from '../../../src/store/native-loader';

const identity = () =>
  JSON.stringify({ commit: 'f'.repeat(40), storeSchema: 2, bindingVersion: 2 });
const hostLoad = (): NativeLoadResult => ({
  module: {
    NativeStore: class {
      async call(): Promise<string> {
        throw new Error('NAPI open in hosted mode');
      }
    },
    bindingVersion: () => 2,
    buildIdentity: identity,
  },
  file: 'fixture.node',
});

const fake = path.join(import.meta.dir, '../web-server/fixtures/fake-aac-http.mjs');
let scratch = '';
let root = '';
const services: RustService[] = [];
const saved = {
  home: process.env.CCS_HOME,
  dir: process.env.CCS_DIR,
  store: process.env.AAC_RUST_STORE,
  off: process.env.AAC_STORE,
};

type Lease = { owned: boolean; pid: number | null; connection: { bootId: string } };
const lease = (s: RustService) => (s as unknown as { lease: Lease | null }).lease;

function binaryCopy(): string {
  const binary = path.join(scratch, 'aac-http');
  fs.copyFileSync(fake, binary);
  fs.chmodSync(binary, 0o755);
  return binary;
}

async function until<T>(read: () => T | null | undefined, what: string): Promise<T> {
  const deadline = Date.now() + 5_000;
  for (;;) {
    const value = read();
    if (value !== null && value !== undefined) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 10));
  }
}

beforeEach(() => {
  scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-r3-'));
  root = path.join(scratch, '.ccs');
  fs.mkdirSync(root, { recursive: true, mode: 0o700 });
  root = fs.realpathSync(root);
  process.env.CCS_HOME = scratch;
  process.env.CCS_DIR = root;
  process.env.AAC_RUST_STORE = 'on';
  delete process.env.AAC_STORE;
  resetAacStoreForTests();
});

afterEach(async () => {
  await closeAacStore().catch(() => undefined);
  for (const service of services.splice(0)) service.stop();
  // Let stopped children exit before the folder goes (their sockets live under it).
  await new Promise((r) => setTimeout(r, 50));
  setRustStoreHosted(root, false);
  resetAacStoreForTests();
  for (const [key, value] of [
    ['CCS_HOME', saved.home],
    ['CCS_DIR', saved.dir],
    ['AAC_RUST_STORE', saved.store],
    ['AAC_STORE', saved.off],
  ] as const) {
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
  }
  fs.rmSync(scratch, { recursive: true, force: true });
});

describe('the store switch', () => {
  it('resolves a relative share backup folder as the in-process writer did', () => {
    expect(shareBackupEnv({ AAC_STORE_SHARE_BACKUP_DIR: 'fixture-backups' }).shareBackupDir).toBe(
      path.resolve('fixture-backups')
    );
    expect(shareBackupEnv({ AAC_STORE_SHARE_BACKUP_DIR: ' off ' }).shareBackupOff).toBe(true);
  });
  it('reads the env first, then config.yaml, else off', () => {
    expect(rustStoreSwitchOn({ AAC_RUST_STORE: 'on' })).toBe(true);
    expect(rustStoreSwitchOn({ AAC_RUST_STORE: 'off' })).toBe(false);
    expect(rustStoreSwitchOn({ AAC_RUST_STORE: 'yes' })).toBe(false);
    delete process.env.AAC_RUST_STORE;
    fs.writeFileSync(path.join(root, 'config.yaml'), 'version: 1\nrustService:\n  store: on\n');
    expect(rustStoreSwitchOn({ CCS_DIR: root })).toBe(true);
    fs.writeFileSync(path.join(root, 'config.yaml'), 'version: 1\n');
    expect(rustStoreSwitchOn({ CCS_DIR: root })).toBe(false);
  });
});

describe('the hosted open', () => {
  it('opens through the service, never NAPI, and reopens on the restarted boot', async () => {
    const service = new RustService('on', { restartDelaysMs: [250, 10, 10, 10, 10] });
    services.push(service);
    await service.start(binaryCopy());
    expect(rustStoreHosted(root)).toBe(true);
    // The boot descriptor carries the store config the service was started with.
    const descriptor = JSON.parse(
      fs.readFileSync(path.join(root, 'run', 'aac-http.json'), 'utf8')
    ) as { store: { dir: string } | null };
    expect(descriptor.store?.dir).toBe(path.join(root, 'store'));

    const opened = await openAacStore({ hostLoad });
    expect(opened.hosted).toBe(true);
    const store = opened.store!;
    const firstBoot = lease(service)!.connection.bootId;
    await store.record([]);
    expect((await store.counts()).opens).toBe(1);

    // Kill the service: calls read service_down until the restart, then reopen per boot.
    process.kill(lease(service)!.pid!, 'SIGKILL');
    await expect(store.counts()).rejects.toThrow(/service_down/);
    const next = await until(() => {
      const current = lease(service);
      return current && current.connection.bootId !== firstBoot ? current : null;
    }, 'the restarted boot');
    expect(next.owned).toBe(true);
    expect((await store.counts()).opens).toBe(1);
  });

  it('an attached boot reads lock_held, and AAC_STORE=off wins', async () => {
    const service = new RustService('on', { restartDelaysMs: [10] });
    services.push(service);
    await service.start(binaryCopy());
    // A second front door on the same root attaches; it must not drive the store.
    const second = new RustService('on', { restartDelaysMs: [10] });
    services.push(second);
    await second.start(binaryCopy());
    expect(lease(second)?.owned).toBe(false);
    // The attached registration marks this root's store as another front door's: the open
    // reads lock_held, as a second in-process writer would.
    const opened = await openAacStore({ hostLoad });
    expect(opened).toEqual({ store: null, reason: 'lock_held' });

    await closeAacStore();
    resetAacStoreForTests();
    process.env.AAC_STORE = 'off';
    const off = await openAacStore({ hostLoad });
    expect(off).toEqual({ store: null, reason: 'disabled' });
  });
});

describe('the socket handle', () => {
  it('reads service_down with no boot, and flips to NAPI once the budget is used up', async () => {
    setRustStoreHosted(root, true);
    const nativeCalls: string[] = [];
    const load = (): NativeLoadResult => ({
      module: {
        NativeStore: class {
          async call(method: string): Promise<string> {
            nativeCalls.push(method);
            return method === 'open' ? '{"state":"ready"}' : '{"native":true}';
          }
        },
        bindingVersion: () => 2,
        buildIdentity: identity,
      },
      file: 'fake.node',
    });
    let fellBack = 0;
    const handle = new SocketStoreHandle(root, { load, onFallback: () => fellBack++ });
    await expect(handle.call('open', '{"dir":"x"}')).rejects.toThrow(/service_down/);
    expect(storeErrorCode(await handle.call('status', '{}').catch((e: unknown) => e))).toBe(
      'service_down'
    );
    onRustStoreGaveUp(root, () => handle.hostGaveUp());
    notifyRustStoreGaveUp(root);
    expect(await handle.call('status', '{}')).toBe('{"native":true}');
    expect(nativeCalls).toEqual(['open', 'status']);
    expect(fellBack).toBe(1);
    handle.hostGaveUp();
    expect(fellBack).toBe(1);
  });

  it('an attached registration alone never answers a call', async () => {
    setRustStoreHosted(root, true);
    const remove = registerRustStoreConnection(
      root,
      { socketPath: path.join(root, 'none.sock'), secret: 's', bootId: 'b' },
      new Set<http.ClientRequest>(),
      {},
      false
    );
    const handle = new SocketStoreHandle(root, {});
    await expect(handle.call('open', '{"dir":"x"}')).rejects.toThrow(/service_down/);
    remove();
  });

  it('giving up before any boot flips on the first call, through the registry', async () => {
    setRustStoreHosted(root, true);
    notifyRustStoreGaveUp(root);
    const handle = new SocketStoreHandle(root, {
      load: (): NativeLoadResult => ({
        module: {
          NativeStore: class {
            async call(method: string): Promise<string> {
              return method === 'open' ? '{"state":"ready"}' : '{"native":true}';
            }
          },
          bindingVersion: () => 2,
          buildIdentity: identity,
        },
        file: 'fake.node',
      }),
    });
    // `open` after the budget is used up opens in process right away.
    expect(await handle.call('open', '{"dir":"x"}')).toBe('{"state":"ready"}');
    expect(await handle.call('counts', '{}')).toBe('{"native":true}');
  });
});

describe('the R3 handoff guards', () => {
  it('refuses a module from another commit, schema or binding before a hosted open', async () => {
    const service = new RustService('on');
    services.push(service);
    await service.start(binaryCopy());
    for (const delta of [{ commit: 'e'.repeat(40) }, { storeSchema: 99 }, { bindingVersion: 99 }]) {
      resetAacStoreForTests();
      const badLoad = (): NativeLoadResult => {
        const loaded = hostLoad();
        if (!loaded.module) throw new Error('fixture module');
        return {
          ...loaded,
          module: {
            ...loaded.module,
            buildIdentity: () => JSON.stringify({ ...JSON.parse(identity()), ...delta }),
          },
        };
      };
      expect((await openAacStore({ hostLoad: badLoad })).reason).toBe('incompatible');
    }
  });

  it('shares the pending close so shutdown waits for the writer', async () => {
    let finish!: () => void;
    const pending = new Promise<void>((resolve) => {
      finish = resolve;
    });
    let closes = 0;
    await openAacStore({
      load: () => ({
        file: 'fixture.node',
        module: {
          NativeStore: class {
            async call(method: string): Promise<string> {
              if (method === 'close') {
                closes++;
                await pending;
              }
              return '{}';
            }
          },
          bindingVersion: () => 2,
        },
      }),
    });
    const first = closeAacStore();
    const second = closeAacStore();
    expect(second).toBe(first);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(closes).toBe(1);
    let done = false;
    void second.then(() => {
      done = true;
    });
    expect(done).toBe(false);
    finish();
    await second;
    expect(done).toBe(true);
  });
});
