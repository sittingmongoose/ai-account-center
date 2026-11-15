/**
 * L1 reply floor, TS half: for a route group Rust owns (R1 onward), the service sends the final
 * bytes, the `ETag` (the identity rule of routes/revalidated-view.ts, crates/aac-http view_reply)
 * and `x-aac-view: <name>@<version>`; the front door writes them without parsing or serializing
 * again. Headers, validators and the 304 are exactly sendRevalidatedView's. R1's Home is the first
 * route that writes them (GET /api/views/home, fed).
 */
import type { Response } from 'express';

export interface RustViewBytes {
  bytes: Buffer;
  etag: string;
  view: string;
}

/** A Rust view reply, or null when the reply is not one (the caller's TS path answers). */
export function rustViewBytes(
  reply: { status: number; bytes?: Buffer; etag?: string | null; view?: string | null },
  expected: string
): RustViewBytes | null {
  if (reply.status !== 200 || !reply.bytes || reply.view !== expected) return null;
  if (!reply.etag || !/^W\/"[a-f0-9]{64}"$/.test(reply.etag)) return null;
  return { bytes: reply.bytes, etag: reply.etag, view: reply.view };
}

/**
 * `cacheControl` is the route's own policy when it is not the revalidated views' (Home keeps its `no-store`: R1
 * changes how the answer is built, not how a browser may keep it).
 */
export function sendRustView(
  res: Response,
  reply: RustViewBytes,
  cacheControl = 'private, no-cache'
): void {
  res.setHeader('Cache-Control', cacheControl);
  res.setHeader('ETag', reply.etag);
  res.vary('Cookie');
  const validators = res.req.get('If-None-Match');
  if (
    validators?.split(',').some((tag) => {
      const value = tag.trim();
      return value === '*' || value.replace(/^W\//, '') === reply.etag.replace(/^W\//, '');
    })
  ) {
    res.status(304).end();
    return;
  }
  res.setHeader('Content-Type', 'application/json; charset=utf-8');
  res.send(reply.bytes);
}
