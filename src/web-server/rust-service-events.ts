/**
 * R4: the service's event stream consumers. The supervisor opens one NDJSON stream per boot
 * (`GET /internal/events`) and dispatches every event here; the Status owner's refresh and the
 * Scoreboard's facts cache subscribe. A refused or broken stream is not fatal: the front door
 * keeps answering, and the next boot opens a new one.
 */

type Listener = (event: Record<string, unknown>) => void;

const listeners = new Set<Listener>();

/** Subscribe to the service's events; returns the unsubscribe call. */
export function onRustServiceEvent(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** One event from the service's stream (the supervisor's stream callback). */
export function dispatchRustServiceEvent(event: Record<string, unknown>): void {
  for (const listener of [...listeners]) {
    try {
      listener(event);
    } catch {
      /* A consumer never breaks the stream. */
    }
  }
}
