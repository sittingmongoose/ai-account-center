/** Publish changed generations to the private service. */
import type { RustService } from '../rust-service';

interface Held {
  key: string;
  /** The generation the service holds for `key`; null when the input could not be pushed exactly. */
  generation: string | null;
  /** Why `generation` is null when it is not `input` (`cap`: too large to push). */
  refused?: string;
}

/** `requestRust`'s request cap: a feed body above it is never sent. */
const RUST_FEED_CAP = 4 * 1024 * 1024;

/** The generation a source holds, or why the request declines. */
export type Generation = { generation: string } | { declined: string };

/** What the front door has pushed to one boot. */
export class BootFeeds {
  readonly held = new Map<string, Held>();
  readonly pending = new Map<string, { key: string; done: Promise<Generation> }>();
  constructor(readonly boot: string) {}
}

/**
 * The last generation number this process issued. Numbers start from the clock (ms x 1000), so a front door that
 * attaches to a service an earlier process fed still pushes newer generations, and a push that finds a newer one
 * held moves past it ([`seen`]); both stay safe integers for centuries.
 */
let last = 0;
export function nextGeneration(): string {
  last = Math.max(last + 1, Date.now() * 1000);
  return String(last);
}
function seen(held: string): void {
  const n = Number(held);
  if (Number.isSafeInteger(n) && n > last) last = n;
}
const boots = new WeakMap<RustService, BootFeeds>();

export function feedsOf(service: RustService, boot: string): BootFeeds {
  let feeds = boots.get(service);
  if (!feeds || feeds.boot !== boot) {
    feeds = new BootFeeds(boot);
    boots.set(service, feeds);
  }
  return feeds;
}

/**
 * The generation the service holds for `source` with content `key`, pushing `build()` first when it does not.
 * Concurrent requests with the same new content share one push. A push that lost a race (the service names a
 * newer generation) or did not land is a decline for this request only.
 */
export function ensure(
  service: RustService,
  feeds: BootFeeds,
  source: string,
  key: string,
  build: () => { data: string; facts: string } | null
): Promise<Generation> {
  const held = feeds.held.get(source);
  if (held && held.key === key)
    return Promise.resolve(
      held.generation === null
        ? { declined: held.refused ?? 'input' }
        : { generation: held.generation }
    );
  const pending = feeds.pending.get(source);
  if (pending && pending.key === key) return pending.done;
  let built: { data: string; facts: string } | null;
  try {
    built = build();
  } catch {
    built = null;
  }
  if (!built) {
    feeds.held.set(source, { key, generation: null });
    return Promise.resolve({ declined: 'input' });
  }
  const generation = nextGeneration();
  // Review L4: a push the transport's cap would refuse is not tried again for the same content.
  if (
    Buffer.byteLength(built.data) + Buffer.byteLength(built.facts) + generation.length + 64 >
    RUST_FEED_CAP
  ) {
    feeds.held.set(source, { key, generation: null, refused: 'cap' });
    return Promise.resolve({ declined: 'cap' });
  }
  const done = service
    .pushFeed(feeds.boot, source, generation, built.data, built.facts)
    .then((now): Generation => {
      if (now === null) return { declined: 'feed' };
      if (now !== generation) {
        // Another push holds a newer generation: this request is today's; the next one pushes past it.
        seen(now);
        return { declined: 'stale' };
      }
      // A late reply of an older push never replaces what a newer one recorded.
      const current = feeds.held.get(source)?.generation;
      if (current === undefined || current === null || Number(current) < Number(generation))
        feeds.held.set(source, { key, generation });
      return { generation };
    })
    .finally(() => {
      if (feeds.pending.get(source)?.done === done) feeds.pending.delete(source);
    });
  feeds.pending.set(source, { key, done });
  return done;
}
