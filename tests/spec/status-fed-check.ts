/**
 * R2 exit gate helper: one Status input through the production fed path (status-feed.ts `answerStatusFed`, the
 * real RustService and the real `aac-http`), against today's handler (`buildStatusView`, written by
 * `sendRevalidatedView`). A served answer must be byte-equal, ETag included; a decline answers with today's
 * handler (nothing written), so it is equal by construction and only counted.
 */
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import type { Request, Response } from 'express';
import type { RustService } from '../../src/web-server/rust-service';
import { answerStatusFed } from '../../src/web-server/status/status-feed';
import { buildStatusView } from '../../src/web-server/status/status-view';
import { getCcsDir } from '../../src/utils/config-manager';

const REQ = {
  method: 'GET',
  sessionID: 'status-fed-fixture',
  session: { authenticated: true },
  get: () => undefined,
} as unknown as Request;

/** `sendRevalidatedView`'s ETag of a view. */
export function revalidatedEtag(view: unknown): string {
  const identity = JSON.stringify(view, (key, value: unknown) =>
    key === 'generatedAt' || key === 'ageSeconds' || key === 'cacheAgeSeconds' ? undefined : value
  );
  return `W/"${createHash('sha256').update(identity).digest('hex')}"`;
}

export interface FedOutcome {
  served: boolean;
  bytes?: Buffer;
  etag?: string;
}

/** One request through the fed path: what it wrote, if anything. */
export async function statusFed(service: RustService, input: unknown): Promise<FedOutcome> {
  const headers = new Map<string, string>();
  let bytes: Buffer | undefined;
  let status = 200;
  const res = {
    req: REQ,
    setHeader: (key: string, value: string) => headers.set(key.toLowerCase(), value),
    vary: () => undefined,
    status: (code: number) => {
      status = code;
      return res;
    },
    end: () => undefined,
    send: (body: Buffer) => {
      bytes = body;
    },
  } as unknown as Response;
  const served = await answerStatusFed(REQ, res, service, input as never, getCcsDir());
  if (!served) {
    assert.equal(bytes, undefined, 'a declined fed request writes nothing');
    return { served };
  }
  assert.equal(status, 200);
  assert.equal(headers.get('cache-control'), 'private, no-cache');
  assert.equal(headers.get('content-type'), 'application/json; charset=utf-8');
  return { served, bytes, etag: headers.get('etag') };
}

/** Serve `input` through the fed path and require today's bytes and ETag. True when Rust served it. */
export async function checkStatusFed(
  service: RustService,
  input: unknown,
  label: string
): Promise<boolean> {
  const expected = buildStatusView(input as never);
  const outcome = await statusFed(service, input);
  if (!outcome.served) return false;
  assert.equal(outcome.bytes?.toString('utf8'), JSON.stringify(expected), `${label}: fed bytes differ`);
  assert.equal(outcome.etag, revalidatedEtag(expected), `${label}: fed ETag differs`);
  return true;
}
