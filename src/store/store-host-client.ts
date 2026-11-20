/**
 * R3: the store's socket handle. With the store hosted, `AacStore` keeps its API and talks
 * to the Rust service's Engine instead of the in-process NAPI module, through the same
 * `call(method, json)` contract (`NativeStoreHandle`). The connection is resolved per call
 * from the supervisor's registry, so a restarted service is picked up (and re-opened)
 * without producers noticing. A service that is down gives `service_down`, as `lock_held`
 * reads today; once the supervisor's restart budget is used up, the dead process has
 * released the kernel lock and this handle opens the same files through NAPI for the rest
 * of the process (design 1.3's automatic fallback).
 */
import {
  requestRustStore,
  rustStoreConnection,
  rustStoreGaveUp,
  RustStoreError,
} from '../web-server/rust-service-transport';
import { loadNativeStore, type NativeLoadResult, type NativeStoreHandle } from './native-loader';
import { requestNativeFeed } from '../web-server/native-collection-feed';

/** Shutdown must reach the supervisor even when a store call never settles. */
export const STORE_CLOSE_DEADLINE_MS = 3000;

function serviceDown(): Error {
  return new RustStoreError('service_down: the store service is not answering');
}

/** Windows worker clients attach to the dashboard's in-process store. The dashboard owns close. */
export class DashboardStoreHandle implements NativeStoreHandle {
  async call(method: string, args: string): Promise<string> {
    if (method === 'close') return '{"closed":true}';
    if (!/^[A-Za-z0-9]{1,32}$/.test(method))
      throw new RustStoreError('invalid: unknown store method');
    const route = method === 'open' ? 'status' : method;
    const response = await requestNativeFeed(`/store/${route}`, {
      method: 'POST',
      body: method === 'open' ? '{}' : args,
    }).catch(() => {
      throw serviceDown();
    });
    if (response.ok) return response.text();
    const failure = (await response.json().catch(() => null)) as { error?: unknown } | null;
    if (typeof failure?.error === 'string') throw new RustStoreError(failure.error);
    throw serviceDown();
  }
}

export class SocketStoreHandle implements NativeStoreHandle {
  private openArgs = '';
  private openReply = '';
  private openedBoot: string | null = null;
  private opening: Promise<string> | null = null;
  private native: NativeStoreHandle | null = null;
  private nativeOpening: Promise<void> | null = null;
  private fellBack = false;
  private identity: string | null = null;
  private closing: Promise<string> | null = null;
  private readonly pending = new Set<Promise<string>>();

  constructor(
    private readonly root: string,
    private readonly options: {
      env?: NodeJS.ProcessEnv;
      load?: () => NativeLoadResult;
      onFallback?: () => void;
      fallback?: boolean;
    } = {}
  ) {}

  call(method: string, args: string): Promise<string> {
    if (method === 'close') {
      if (!this.closing) {
        const finish = (async () => {
          await Promise.allSettled([...this.pending]);
          await this.nativeOpening?.catch(() => undefined);
          return this.native ? this.native.call('close', args) : this.closeOverSocket();
        })();
        this.closing = new Promise<string>((resolve, reject) => {
          const timer = setTimeout(() => reject(serviceDown()), STORE_CLOSE_DEADLINE_MS);
          void finish.then(resolve, reject).finally(() => clearTimeout(timer));
        });
      }
      return this.closing;
    }
    if (this.closing) return Promise.reject(new RustStoreError('closed: the store is stopping'));
    const call = this.callInner(method, args);
    this.pending.add(call);
    void call.finally(() => this.pending.delete(call)).catch(() => undefined);
    return call;
  }

  private async callInner(method: string, args: string): Promise<string> {
    if (this.native) return this.native.call(method, args);
    if (method === 'open') {
      this.openArgs = args;
      return this.ensureOpen();
    }
    if (rustStoreGaveUp(this.root)) {
      await this.fallbackToNative();
      const native = this.native as NativeStoreHandle | null;
      if (!native) throw serviceDown();
      return native.call(method, args);
    }
    await this.ensureOpen();
    const opened = this.native as NativeStoreHandle | null;
    if (opened) return opened.call(method, args);
    try {
      return await this.socket(method, args);
    } catch (error) {
      // A new boot does not hold this handle's open yet: open it once, then retry once.
      // The retry is safe: `closed` proves the first attempt never reached an engine.
      if (error instanceof RustStoreError && error.message.startsWith('closed:')) {
        this.openedBoot = null;
        await this.ensureOpen();
        const reopened = this.native as NativeStoreHandle | null;
        if (reopened) return reopened.call(method, args);
        return this.socket(method, args);
      }
      throw error;
    }
  }

  /** The supervisor gave up: fall back now, not on some later sample. Never rejects. */
  hostGaveUp(): void {
    if (this.closing || this.options.fallback === false) return;
    void this.fallbackToNative().catch(() => {
      /* The next call retries; the failure names itself there. */
    });
  }

  private async ensureOpen(): Promise<string> {
    if (this.native) return this.openReply;
    if (rustStoreGaveUp(this.root)) {
      await this.fallbackToNative();
      return this.openReply;
    }
    const entry = rustStoreConnection(this.root);
    if (!entry) throw serviceDown();
    if (this.openedBoot === entry.connection.bootId) return this.openReply;
    this.opening ??= (async () => {
      const bootId = entry.connection.bootId;
      try {
        const hosted = await requestRustStore(entry, 'hostIdentity', '{}');
        const native = this.nativeIdentity();
        const identity = JSON.parse(hosted) as { commit: string; storeSchema: number };
        if (
          this.identityKey(identity) !== native ||
          identity.commit !== entry.connection.build?.commit ||
          identity.storeSchema !== entry.connection.build?.storeSchema
        )
          throw new RustStoreError('incompatible: the store host and binding builds differ');
        this.identity = native;
      } catch (error) {
        if (error instanceof RustStoreError) throw error;
        throw serviceDown();
      }
      const reply = await requestRustStore(entry, 'open', this.openArgs).catch((error: unknown) => {
        if (error instanceof RustStoreError) throw error;
        throw serviceDown();
      });
      this.openedBoot = bootId;
      this.openReply = reply;
      return reply;
    })().finally(() => {
      this.opening = null;
    });
    return this.opening;
  }

  private async socket(method: string, args: string): Promise<string> {
    const entry = rustStoreConnection(this.root);
    if (!entry) throw serviceDown();
    try {
      return await requestRustStore(entry, method, args);
    } catch (error) {
      if (error instanceof RustStoreError) throw error;
      throw serviceDown();
    }
  }

  private async closeOverSocket(): Promise<string> {
    const entry = rustStoreConnection(this.root);
    this.openedBoot = null;
    if (!entry) return '{"closed":true}';
    try {
      return await requestRustStore(entry, 'close', '{}');
    } catch {
      // The supervisor still stops the child after this error; do not claim a clean seal.
      throw serviceDown();
    }
  }

  private identityKey(value: unknown): string {
    const id = value as { commit?: unknown; storeSchema?: unknown; bindingVersion?: unknown };
    if (
      !id ||
      typeof id.commit !== 'string' ||
      !/^[a-f0-9]{7,40}$/.test(id.commit) ||
      id.storeSchema !== 2 ||
      id.bindingVersion !== 2
    )
      throw new RustStoreError('incompatible: unproved store build identity');
    return JSON.stringify([id.commit, id.storeSchema, id.bindingVersion]);
  }

  private nativeIdentity(): string {
    const loaded = (this.options.load ?? (() => loadNativeStore({ env: this.options.env })))();
    if (!loaded.module?.buildIdentity)
      throw new RustStoreError('incompatible: the store binding has no compiled identity');
    try {
      return this.identityKey(JSON.parse(loaded.module.buildIdentity()));
    } catch {
      throw new RustStoreError('incompatible: invalid store binding identity');
    }
  }

  /**
   * Open the same store through the NAPI module, once, for the rest of this process.
   * The supervisor calls this only after its budget is used up, when no service process
   * holds the lock; a `lock_held` here names a holder this dashboard does not own.
   */
  private async fallbackToNative(): Promise<void> {
    if (this.options.fallback === false) throw serviceDown();
    if (this.native) return;
    this.nativeOpening ??= (async () => {
      const loaded = (this.options.load ?? (() => loadNativeStore({ env: this.options.env })))();
      if (!loaded.module)
        throw new RustStoreError(`${loaded.reason}: the store module is unavailable for fallback`);
      const identity = this.nativeIdentity();
      if (this.identity !== null && this.identity !== identity)
        throw new RustStoreError('incompatible: the fallback binding build changed');
      const handle = new loaded.module.NativeStore();
      const reply = await handle.call('open', this.openArgs);
      this.native = handle;
      this.openReply = reply;
      if (!this.fellBack) {
        this.fellBack = true;
        this.options.onFallback?.();
      }
    })().finally(() => {
      this.nativeOpening = null;
    });
    return this.nativeOpening;
  }
}
