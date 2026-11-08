/**
 * Loads the status readers' native module: `dist/native/aac-status.<platform>-<arch>.node`.
 * Any failure returns no module and a fixed reason; the server then runs without the readers.
 * Copied from the history store's loader (native/aac-store branch).
 */
import * as fs from 'fs';
import * as path from 'path';

/** Must equal the crate's BINDING_VERSION (native/aac-status/src/node.rs). */
export const STATUS_BINDING_VERSION = 1;

export interface NativeStatusHandle {
  call(method: string, args: string): Promise<string>;
}

export interface NativeStatusModule {
  NativeStatus: new () => NativeStatusHandle;
  bindingVersion(): number;
}

export type StatusUnavailableReason = 'disabled' | 'missing' | 'incompatible' | 'load_failed';

export type NativeStatusLoadResult =
  | { module: NativeStatusModule; file: string }
  | { module: null; reason: StatusUnavailableReason; file: string | null };

/** The module file name for this computer, e.g. `aac-status.linux-x64.node`. */
export function nativeStatusModuleName(
  platform: string = process.platform,
  arch: string = process.arch
): string {
  return `aac-status.${platform}-${arch}.node`;
}

/** `dist/native/...` next to the compiled server (this file compiles to `dist/status-native/`). */
export function defaultNativeStatusModulePath(): string {
  return path.resolve(__dirname, '..', 'native', nativeStatusModuleName());
}

export function loadNativeStatus(
  options: {
    env?: NodeJS.ProcessEnv;
    file?: string;
    requireFn?: (file: string) => unknown;
  } = {}
): NativeStatusLoadResult {
  const env = options.env ?? process.env;
  if ((env.AAC_STATUS_NATIVE ?? '').trim().toLowerCase() === 'off') {
    return { module: null, reason: 'disabled', file: null };
  }
  const file = options.file ?? env.AAC_STATUS_MODULE ?? defaultNativeStatusModulePath();
  try {
    if (!fs.statSync(file).isFile()) return { module: null, reason: 'missing', file };
  } catch {
    return { module: null, reason: 'missing', file };
  }
  try {
    const loaded = (options.requireFn ?? require)(file) as Partial<NativeStatusModule>;
    if (
      typeof loaded?.NativeStatus !== 'function' ||
      typeof loaded.bindingVersion !== 'function' ||
      loaded.bindingVersion() !== STATUS_BINDING_VERSION
    ) {
      return { module: null, reason: 'incompatible', file };
    }
    return { module: loaded as NativeStatusModule, file };
  } catch {
    return { module: null, reason: 'load_failed', file };
  }
}

/** The plain sentence a caller shows for each loader reason. */
export function statusUnavailableSentence(reason: StatusUnavailableReason): string {
  switch (reason) {
    case 'disabled':
      return 'the aac-status native module is disabled';
    case 'missing':
      return 'the aac-status native module is missing';
    case 'incompatible':
      return 'the aac-status native module is incompatible';
    case 'load_failed':
      return 'the aac-status native module could not be loaded';
  }
}
