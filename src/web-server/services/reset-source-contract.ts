/** The private reset source seam, shared by the reset claim route and its source adapter. Types only: the route
 * re-exports them, so the adapter needs no import of an HTTP route module (src/web-server stays acyclic). */
export interface ResetSource {
  /** Fresh identity-bound private Snapshot JSON; never include credentials or email. */
  read(account: string): Promise<unknown>;
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
