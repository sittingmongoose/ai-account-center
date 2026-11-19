/** The private reset source seam, shared by the reset claim route and its source adapter. Types only: the route
 * re-exports them, so the adapter needs no import of an HTTP route module (src/web-server stays acyclic). */
import type { NativeStatusHandle } from '../../status-native/native-loader';

export interface ResetSource {
  /** Fresh identity-bound private Snapshot JSON; never include credentials or email.
   * The read allocates its ticket in the claims file, so it goes through `claimsFile`, the
   * route's one writer of that file (the native module or the Rust service), when given. */
  read(account: string, claimsFile?: NativeStatusHandle): Promise<unknown>;
  /** Must target exactly request.identity and exact grant in request.body; no fallback.
   * Claude's `not-sent` is host proof that execution never reached the provider opener.
   * A lost host response must throw or remain ambiguous, never return that proof word.
   */
  send(request: ResetWrite): Promise<{ outcome: string; retryAfterMs?: number }>;
}
export interface ResetWrite {
  method: string;
  body: Record<string, unknown>;
  identity: Record<string, string>;
  attemptKey: string;
  sourceBinding: string;
  routeAccount: string;
}
