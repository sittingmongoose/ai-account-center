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
    observations++;
    assert.equal(req.headers['x-aac-boot-secret'], secret);
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
    const on = service('on');
    services.push(on);
    let body: unknown;
    const res = {
      json: (value: unknown) => {
        body = value;
      },
    } as Response;
    for (const fault of ['good', 'wrong-boot', 'invalid-json', 'oversize', '503', 'timeout']) {
      behavior = fault;
      const before = on.stats.fallbacks;
      await on.respond(req, res, '/api/accounts/settings', () => ({ refreshIntervalSeconds: 60 }));
      assert.deepEqual(body, { refreshIntervalSeconds: fault === 'good' ? 90 : 60 }, fault);
      if (fault !== 'good') assert.ok(on.stats.fallbacks > before, fault);
    }
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
