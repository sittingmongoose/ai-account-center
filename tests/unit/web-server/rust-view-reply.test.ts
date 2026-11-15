/** L1 reply floor: Rust's bytes and ETag go out exactly as sendRevalidatedView sends the view. */
import { describe, expect, it } from 'bun:test';
import express from 'express';
import http from 'node:http';
import { createHash } from 'node:crypto';
import { sendRevalidatedView } from '../../../src/web-server/routes/revalidated-view';
import { rustViewBytes, sendRustView } from '../../../src/web-server/rust-view-reply';

// What crates/aac-http view_reply sends for this view (its test pins the same Node hash).
const view = { b: 1, generatedAt: 'x', rows: [{ ageSeconds: 3, v: 'é' }] };
const identity = JSON.stringify({ b: 1, rows: [{ v: 'é' }] });
const rust = {
  status: 200,
  bytes: Buffer.from(JSON.stringify(view)),
  etag: `W/"${createHash('sha256').update(identity).digest('hex')}"`,
  view: 'fixture@1',
};

async function serve(handler: express.RequestHandler, headers: Record<string, string> = {}) {
  const app = express();
  app.get('/view', handler);
  const server = http.createServer(app);
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', r));
  const port = (server.address() as { port: number }).port;
  const response = await fetch(`http://127.0.0.1:${port}/view`, { headers });
  const body = await response.text();
  server.close();
  const kept = ['cache-control', 'etag', 'vary', 'content-type', 'content-length'];
  return {
    status: response.status,
    body,
    headers: Object.fromEntries(kept.map((k) => [k, response.headers.get(k)])),
  };
}

describe('Rust view replies (L1)', () => {
  it('match sendRevalidatedView byte for byte, ETag and 304 included', async () => {
    const accepted = rustViewBytes(rust, 'fixture@1');
    expect(accepted).not.toBeNull();
    const ts = await serve((_req, res) => sendRevalidatedView(res, view));
    const viaRust = await serve((_req, res) => sendRustView(res, accepted!));
    expect(viaRust).toEqual(ts);
    const again = { 'if-none-match': rust.etag };
    expect(await serve((_req, res) => sendRustView(res, accepted!), again)).toEqual(
      await serve((_req, res) => sendRevalidatedView(res, view), again)
    );
  });

  it('take only a 200 view reply of the expected version with a well-formed ETag', () => {
    expect(rustViewBytes({ ...rust, status: 503 }, 'fixture@1')).toBeNull();
    expect(rustViewBytes(rust, 'fixture@2')).toBeNull();
    expect(rustViewBytes({ ...rust, etag: '"strong"' }, 'fixture@1')).toBeNull();
    expect(rustViewBytes({ ...rust, bytes: undefined }, 'fixture@1')).toBeNull();
  });
});
