/** Run the existing store suites through either real handle. Fixture directories only. */
import { afterAll } from 'bun:test';
import fs from 'node:fs';
import path from 'node:path';
import type http from 'node:http';
import {
  loadNativeStore,
  type NativeLoadResult,
  type NativeStoreHandle,
} from '../../src/store/native-loader';
import { SocketStoreHandle } from '../../src/store/store-host-client';
import {
  acquireRustServiceDetailed,
  type RustLease,
} from '../../src/web-server/rust-service-lifecycle';
import {
  registerRustStoreConnection,
  setRustStoreHosted,
} from '../../src/web-server/rust-service-transport';

const cleanups = new Set<() => Promise<void>>();
afterAll(async () => {
  for (const cleanup of [...cleanups]) await cleanup();
});

export function loadStoreTestModule(
  options: Parameters<typeof loadNativeStore>[0]
): NativeLoadResult {
  const loaded = loadNativeStore(options);
  if (!loaded.module || process.env.AAC_TEST_STORE_HANDLE !== 'socket') return loaded;
  const module = loaded.module;
  class FixtureHandle implements NativeStoreHandle {
    private handle: SocketStoreHandle | null = null;
    private lease: RustLease | null = null;
    private remove: (() => void) | null = null;
    private root = '';
    private cleanup = async (): Promise<void> => {
      await this.handle?.call('close', '{}').catch(() => undefined);
      this.remove?.();
      this.lease?.close();
      const socket = this.lease?.connection.socketPath;
      const end = Date.now() + 3000;
      while (socket && fs.existsSync(socket) && Date.now() < end)
        await new Promise((r) => setTimeout(r, 10));
      setRustStoreHosted(this.root, false);
      this.handle = null;
      this.lease = null;
      cleanups.delete(this.cleanup);
    };
    async call(method: string, args: string): Promise<string> {
      if (method === 'open') {
        const input = JSON.parse(args) as {
          dir: string;
          retentionDays?: number;
          cacheBytes?: number;
          activityRetentionDays?: number;
        };
        if (path.basename(input.dir) !== 'store')
          throw new Error('fixture store must end in /store');
        this.root = path.dirname(input.dir);
        fs.mkdirSync(this.root, { recursive: true, mode: 0o700 });
        const active = new Set<http.ClientRequest>();
        const result = await acquireRustServiceDetailed(
          this.root,
          process.env.AAC_RUST_SERVICE_BINARY ?? path.resolve('dist/native/aac-http'),
          active,
          undefined,
          {
            bindingIdentity: JSON.parse(module.buildIdentity!()),
            retentionDays: input.retentionDays ?? 90,
            cacheBytes: input.cacheBytes,
            activityRetentionDays: input.activityRetentionDays,
            snapshotDirectory: null,
            shareBackupDir: null,
            shareBackupOff: true,
          }
        );
        if (!result.lease?.owned)
          throw new Error(`fixture service did not start: ${result.failure}`);
        this.lease = result.lease;
        setRustStoreHosted(this.root, true);
        this.remove = registerRustStoreConnection(
          this.root,
          this.lease.connection,
          active,
          {},
          true
        );
        this.handle = new SocketStoreHandle(this.root, { load: () => loaded });
        cleanups.add(this.cleanup);
        try {
          return await this.handle.call(method, args);
        } catch (error) {
          await this.cleanup();
          throw error;
        }
      }
      if (!this.handle) throw new Error('closed: fixture store is closed');
      if (method === 'close') {
        await this.cleanup();
        return '{"closed":true}';
      }
      return this.handle.call(method, args);
    }
  }
  return { ...loaded, module: { ...module, NativeStore: FixtureHandle } };
}
