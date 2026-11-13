/**
 * Loads the store's native module (docs/store.md 9.2): `dist/native/aac-store.<platform>-<arch>.node`.
 * Any failure returns no module and a fixed reason; the dashboard then runs without history.
 */
import * as fs from 'fs';
import * as path from 'path';

/** Must equal the crate's BINDING_VERSION (native/aac-store/src/node.rs). */
export const STORE_BINDING_VERSION = 2;

export interface NativeStoreHandle {
  call(method: string, args: string): Promise<string>;
}

export interface NativeStoreModule {
  NativeStore: new () => NativeStoreHandle;
  bindingVersion(): number;
}

export type StoreUnavailableReason = 'disabled' | 'missing' | 'incompatible' | 'load_failed';

export type NativeLoadResult =
  | { module: NativeStoreModule; file: string }
  | { module: null; reason: StoreUnavailableReason; file: string | null };

/** The module file name for this computer, e.g. `aac-store.linux-x64.node`. */
export function nativeModuleName(
  platform: string = process.platform,
  arch: string = process.arch
): string {
  return `aac-store.${platform}-${arch}.node`;
}

/** `dist/native/...` next to the compiled server (this file compiles to `dist/store/`). */
export function defaultNativeModulePath(): string {
  return path.resolve(__dirname, '..', 'native', nativeModuleName());
}

export function loadNativeStore(
  options: {
    env?: NodeJS.ProcessEnv;
    file?: string;
    requireFn?: (file: string) => unknown;
  } = {}
): NativeLoadResult {
  const env = options.env ?? process.env;
  if ((env.AAC_STORE ?? '').trim().toLowerCase() === 'off') {
    return { module: null, reason: 'disabled', file: null };
  }
  const file = options.file ?? env.AAC_STORE_MODULE ?? defaultNativeModulePath();
  try {
    if (!fs.statSync(file).isFile()) return { module: null, reason: 'missing', file };
  } catch {
    return { module: null, reason: 'missing', file };
  }
  try {
    const loaded = (options.requireFn ?? require)(file) as Partial<NativeStoreModule>;
    if (
      typeof loaded?.NativeStore !== 'function' ||
      typeof loaded.bindingVersion !== 'function' ||
      loaded.bindingVersion() !== STORE_BINDING_VERSION
    ) {
      return { module: null, reason: 'incompatible', file };
    }
    return { module: loaded as NativeStoreModule, file };
  } catch {
    return { module: null, reason: 'load_failed', file };
  }
}
