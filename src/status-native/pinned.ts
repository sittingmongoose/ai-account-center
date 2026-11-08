/**
 * T3's pinned threads through the aac-status native module (native/aac-status/src/pins.rs).
 *
 * Minimal wiring on purpose: one function, no route and no view yet. The pinned-update
 * composer and the Status page come after the design is approved. Every failure is
 * `{ unavailable: <fixed sentence> }`; nothing here throws, guesses, or reads the database
 * itself.
 */
import {
  loadNativeStatus,
  statusUnavailableSentence,
  type NativeStatusHandle,
} from './native-loader';

export interface PinnedThread {
  id: string;
  title: string;
  /** As T3 wrote it (an ISO string in practice); passed through, never reinterpreted. */
  pinnedAt: string | number;
  pinOrderKey: string | number;
}

export type PinnedThreadsResult = { pinned: PinnedThread[] } | { unavailable: string };

/** The sentence for each native unavailability code. Anything else reads as unreadable. */
function pinsUnavailableSentence(code: unknown): string {
  if (code === 'missing') return "T3's state file is missing";
  if (code === 'shape') return "T3's state file changed shape";
  return "T3's state file could not be read";
}

function isPinnedThread(value: unknown): value is PinnedThread {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const row = value as Record<string, unknown>;
  const scalar = (field: unknown): boolean =>
    typeof field === 'string' || (typeof field === 'number' && Number.isFinite(field));
  return (
    typeof row.id === 'string' &&
    typeof row.title === 'string' &&
    scalar(row.pinnedAt) &&
    scalar(row.pinOrderKey)
  );
}

/**
 * The pinned top-level threads from T3's state database (`AAC_T3_STATE_DB`, else
 * `~/.t3/userdata/statev2.sqlite`; the native reader resolves the default), in pin order.
 */
export async function readPinnedThreads(
  options: {
    /** A state database to read instead of the default (tests point this at a fixture). */
    dbPath?: string | null;
    env?: NodeJS.ProcessEnv;
    file?: string;
    requireFn?: (file: string) => unknown;
    /** A handle to call instead of loading the module (tests). */
    handle?: NativeStatusHandle;
  } = {}
): Promise<PinnedThreadsResult> {
  let handle = options.handle;
  if (!handle) {
    const loaded = loadNativeStatus(options);
    if (!loaded.module) return { unavailable: statusUnavailableSentence(loaded.reason) };
    try {
      handle = new loaded.module.NativeStatus();
    } catch {
      return { unavailable: statusUnavailableSentence('load_failed') };
    }
  }
  let answer: unknown;
  try {
    const raw = await handle.call('pins', JSON.stringify({ dbPath: options.dbPath ?? null }));
    answer = JSON.parse(raw) as unknown;
  } catch {
    return { unavailable: pinsUnavailableSentence('io') };
  }
  if (!answer || typeof answer !== 'object' || Array.isArray(answer)) {
    return { unavailable: pinsUnavailableSentence('io') };
  }
  const record = answer as Record<string, unknown>;
  if ('unavailable' in record) return { unavailable: pinsUnavailableSentence(record.unavailable) };
  if (!Array.isArray(record.pinned) || !record.pinned.every(isPinnedThread)) {
    return { unavailable: pinsUnavailableSentence('io') };
  }
  return { pinned: record.pinned };
}
