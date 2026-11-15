/** Fault injection against a real fixture socket; called only inside capture.ts's I/O sandbox. */
import assert from 'node:assert/strict';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import type { Request, Response } from 'express';
import { RustService } from '../../src/web-server/rust-service';

export async function checkRustProxyBoundary(home: string) {
  const socketPath = path.join(home, 'proxy.sock');
  const secret = 'e'.repeat(64);
  const bootId = 'd'.repeat(24);
  let behavior = 'good';
  let observations = 0;
  const pending = new Set<http.ServerResponse>();
  const server = http.createServer((req, res) => {
    assert.equal(req.headers['x-aac-boot-secret'], secret);
    // L1: after a connect or boot failure the bridge checks the boot's health (public principal).
    if (req.url === '/api/health') {
      assert.deepEqual(JSON.parse(String(req.headers['x-aac-principal'])), {
        kind: 'session',
        id: 'public-health',
        role: 'public',
      });
      res.setHeader('x-aac-service-boot', behavior === 'wrong-boot' ? 'bad' : bootId);
      res.end('{"status":"ok"}');
      return;
    }
    observations++;
    assert.equal(req.headers.cookie, undefined);
    assert.equal(req.headers.authorization, undefined);
    assert.equal(req.headers.origin, undefined);
    const principal = JSON.parse(String(req.headers['x-aac-principal']));
    assert.equal(principal.role, 'admin');
    assert.equal(principal.kind, 'session');
    assert.notEqual(principal.id, 'forged');
    assert.notEqual(principal.id, 'fixture-session');
    res.setHeader('x-aac-service-boot', behavior === 'wrong-boot' ? 'bad' : bootId);
    if (behavior === 'timeout') {
      pending.add(res);
      res.on('close', () => pending.delete(res));
      return;
    }
    if (behavior === 'invalid-json') {
      res.end('{');
      return;
    }
    if (behavior === 'oversize') {
      res.end(' '.repeat(65537));
      return;
    }
    if (behavior === '503') res.statusCode = 503;
    res.end(JSON.stringify({ refreshIntervalSeconds: behavior === 'different' ? 120 : 90 }));
  });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(socketPath, resolve);
  });
  fs.chmodSync(socketPath, 0o600);
  function service(mode: 'on' | 'shadow') {
    const s = new RustService(mode);
    // Inject only the private transport lease, so the production response path gets real socket faults.
    Object.assign(s, { lease: { connection: { socketPath, secret, bootId }, close: () => {} } });
    return s;
  }
  const req = {
    method: 'GET',
    originalUrl: '/api/accounts/settings',
    sessionID: 'fixture-session',
    session: { authenticated: true },
    headers: {
      authorization: 'fixture-must-not-forward',
      cookie: 'fixture-must-not-forward',
      origin: 'https://fixture.invalid',
      'x-aac-principal': 'forged',
      'x-aac-boot-secret': 'forged',
    },
  } as unknown as Request;
  const services: RustService[] = [];
  try {
    let body: unknown;
    const res = {
      json: (value: unknown) => {
        body = value;
      },
    } as Response;
    // A fresh bridge per fault, so an earlier fault's breaker or lost lease cannot hide this one.
    for (const fault of ['good', 'wrong-boot', 'invalid-json', 'oversize', '503', 'timeout']) {
      behavior = fault;
      const on = service('on');
      services.push(on);
      const seen = observations;
      await on.respond(req, res, '/api/accounts/settings', () => ({ refreshIntervalSeconds: 60 }));
      assert.deepEqual(body, { refreshIntervalSeconds: fault === 'good' ? 90 : 60 }, fault);
      assert.equal(observations, seen + 1, `${fault}: the fault reached the socket`);
      if (fault !== 'good') assert.equal(on.stats.fallbacks, 1, fault);
    }
    // L1: three transport failures in a row open the route's breaker; the fourth stays in TS.
    behavior = 'invalid-json';
    const on = service('on');
    services.push(on);
    for (let i = 0; i < 3; i++)
      await on.respond(req, res, '/api/accounts/settings', () => ({ refreshIntervalSeconds: 60 }));
    const opened = observations;
    await on.respond(req, res, '/api/accounts/settings', () => ({ refreshIntervalSeconds: 60 }));
    assert.equal(observations, opened, 'an open breaker never calls the socket');
    assert.deepEqual(body, { refreshIntervalSeconds: 60 });
    const route = (on.diagnostics().routes as Record<string, Record<string, unknown>>)[
      '/api/accounts/settings'
    ];
    assert.deepEqual(route.failed, { reply: 3 });
    assert.deepEqual(route.unavailable, { breaker: 1 });
    behavior = 'different';
    const shadow = service('shadow');
    services.push(shadow);
    await shadow.respond(req, res, '/api/accounts/settings', () => ({
      refreshIntervalSeconds: 60,
    }));
    assert.deepEqual(body, { refreshIntervalSeconds: 60 }, 'shadow always serves TS');
    for (let i = 0; i < 100 && shadow.stats.shadowed === 0; i++)
      await new Promise((r) => setTimeout(r, 10));
    assert.equal(shadow.stats.differences, 1, 'real divergent Rust reply is logged');
    assert.equal(shadow.stats.served, 0);
    const count = observations;
    await on.respond({ ...req, method: 'PUT' } as Request, res, '/api/accounts/settings', () => ({
      ts: true,
    }));
    assert.equal(observations, count, 'write routes never use the socket');
    assert.deepEqual(body, { ts: true });
  } finally {
    for (const s of services) s.stop();
    for (const response of pending) response.destroy();
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}
