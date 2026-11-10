/**
 * T3's pinned threads through the aac-status native module (native/aac-status/src/pins.rs):
 * the VM's own rows from its database, the other machines' rows parsed from their
 * `sqlite3 -json` answers, the merged pin list, and the composed updates
 * (native/aac-status/src/pinned.rs). Every failure is `{ unavailable: <fixed sentence> }`;
 * nothing here throws, guesses, or reads a database itself.
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
  /** The pin's order key, or null when T3 wrote no key (the row then sorts after every keyed pin). */
  pinOrderKey: string | number | null;
}

export type PinnedThreadsResult = { pinned: PinnedThread[] } | { unavailable: string };

/** One merged pin: the thread fields plus the machine it was read from. */
export interface MergedPin extends PinnedThread {
  machine: string;
}

/** The sentence for each native unavailability code. Anything else reads as unreadable. */
function pinsUnavailableSentence(code: unknown): string {
  if (code === 'missing') return "T3's state file is missing";
  if (code === 'shape') return "T3's state file changed shape";
  if (code === 'limit') return "T3's state file is past this reader's pin limits";
  return "T3's state file could not be read";
}

function isPinnedThread(value: unknown): value is PinnedThread {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const row = value as Record<string, unknown>;
  const scalar = (field: unknown): boolean =>
    typeof field === 'string' || (typeof field === 'number' && Number.isFinite(field));
  const orderKey = (field: unknown): boolean => field === null || scalar(field);
  return (
    typeof row.id === 'string' &&
    typeof row.title === 'string' &&
    scalar(row.pinnedAt) &&
    orderKey(row.pinOrderKey)
  );
}

function isMergedPin(value: unknown): value is MergedPin {
  return isPinnedThread(value) && typeof (value as MergedPin).machine === 'string';
}

async function call(handle: NativeStatusHandle, method: string, args: unknown): Promise<unknown> {
  return JSON.parse(await handle.call(method, JSON.stringify(args))) as unknown;
}

/**
 * A handle to the native module, or why there is none (the fixed sentence the caller shows).
 * Loading once and reusing the handle is fine: the readers are stateless.
 */
export function openStatusHandle(
  options: {
    env?: NodeJS.ProcessEnv;
    file?: string;
    requireFn?: (file: string) => unknown;
  } = {}
): NativeStatusHandle | { unavailable: string } {
  const loaded = loadNativeStatus(options);
  if (!loaded.module) return { unavailable: statusUnavailableSentence(loaded.reason) };
  try {
    return new loaded.module.NativeStatus();
  } catch {
    return { unavailable: statusUnavailableSentence('load_failed') };
  }
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
    const opened = openStatusHandle(options);
    if ('unavailable' in opened) return { unavailable: opened.unavailable };
    handle = opened;
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

/**
 * One remote machine's pinned rows from the `sqlite3 -json` text it printed: the same shape
 * checks as the VM's own read, in the native module. `shape` is a fixed refusal, never a
 * guess; a machine that did not answer never reaches this function.
 */
export async function parseRemotePins(
  machine: 'mac' | 'windows' | 'nas1',
  json: string,
  options: { handle: NativeStatusHandle }
): Promise<PinnedThreadsResult> {
  let answer: unknown;
  try {
    answer = await call(options.handle, 'parseRemotePins', { machine, json });
  } catch {
    return { unavailable: pinsUnavailableSentence('io') };
  }
  const record = answer as Record<string, unknown>;
  if (record.ok === true) {
    if (!Array.isArray(record.pinned) || !record.pinned.every(isPinnedThread)) {
      return { unavailable: pinsUnavailableSentence('io') };
    }
    return { pinned: record.pinned };
  }
  return { unavailable: 'shape' };
}

/**
 * Every machine's good pins in one list: ordered by pinOrderKey then machine, each row
 * tagged with its machine, de-duplicated by thread id. All lists together; empty lists are
 * allowed and mean that machine has no pinned threads.
 */
export async function mergePins(
  lists: ReadonlyArray<{ machine: string; pinned: PinnedThread[] }>,
  options: { handle: NativeStatusHandle }
): Promise<{ pinned: MergedPin[] } | { unavailable: string }> {
  let answer: unknown;
  try {
    answer = await call(options.handle, 'mergePins', {
      lists: lists.map((list) => ({ machine: list.machine, pinned: list.pinned })),
    });
  } catch {
    return { unavailable: pinsUnavailableSentence('io') };
  }
  const record = answer as Record<string, unknown>;
  if (!Array.isArray(record.pinned) || !record.pinned.every(isMergedPin)) {
    return { unavailable: pinsUnavailableSentence('io') };
  }
  return { pinned: record.pinned };
}

/**
 * The composed pinned view (the update each pinned thread would give, every word decided in
 * the native module). The input is the already-validated views the Status reader builds; the
 * answer is passed through unchecked beyond its top shape, as the module's own output.
 */
export async function composePinned(
  input: unknown,
  options: { handle: NativeStatusHandle }
): Promise<{ view: Record<string, unknown> } | { unavailable: string }> {
  let answer: unknown;
  try {
    answer = await call(options.handle, 'compose', input);
  } catch {
    return { unavailable: pinsUnavailableSentence('io') };
  }
  if (!answer || typeof answer !== 'object' || Array.isArray(answer)) {
    return { unavailable: pinsUnavailableSentence('io') };
  }
  const record = answer as Record<string, unknown>;
  if (typeof record.head !== 'string' || !Array.isArray(record.updates)) {
    return { unavailable: pinsUnavailableSentence('io') };
  }
  return { view: record };
}
