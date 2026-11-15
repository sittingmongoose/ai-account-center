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

function serviceDown(): Error {
  return new RustStoreError('service_down: the store service is not answering');
}

export class SocketStoreHandle implements NativeStoreHandle {
  private openArgs = '';
  private openReply = '';
  private openedBoot: string | null = null;
  private opening: Promise<string> | null = null;
  private native: NativeStoreHandle | null = null;
  private nativeOpening: Promise<void> | null = null;
  private fellBack = false;

  constructor(
    private readonly root: string,
    private readonly options: {
      env?: NodeJS.ProcessEnv;
      load?: () => NativeLoadResult;
      onFallback?: () => void;
    } = {}
  ) {}

  async call(method: string, args: string): Promise<string> {
    if (this.native) return this.native.call(method, args);
    if (method === 'open') {
      this.openArgs = args;
      return this.ensureOpen();
    }
    if (method === 'close') return this.closeOverSocket();
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
      const reply = await requestRustStore(entry, 'open', this.openArgs);
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
      // The service closes its engine itself when it ends; a lost close is not an error.
      return '{"closed":true}';
    }
  }

  /**
   * Open the same store through the NAPI module, once, for the rest of this process.
   * The supervisor calls this only after its budget is used up, when no service process
   * holds the lock; a `lock_held` here names a holder this dashboard does not own.
   */
  private async fallbackToNative(): Promise<void> {
    if (this.native) return;
    this.nativeOpening ??= (async () => {
      const loaded = (this.options.load ?? (() => loadNativeStore({ env: this.options.env })))();
      if (!loaded.module)
        throw new RustStoreError(`${loaded.reason}: the store module is unavailable for fallback`);
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
