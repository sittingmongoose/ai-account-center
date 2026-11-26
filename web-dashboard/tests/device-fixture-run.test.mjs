// The device fixture's Update apps run endpoint (TOOL-ISSUES device fixture row:
// pressing the header Update apps button showed "API endpoint was not found").
// fakeServer answers POST /api/app-updates/start with a running job, refuses a
// second start with 409, and answers cancel. Fixture data only, no browser.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fakeServer, runJob } from './browser/accounts-fixture.mjs';

const call = (server, method, pathname, body = {}) => {
  for (const [m, re, answer] of server.routes) {
    const match = pathname.match(re);
    if (m === method && match) return answer(body, match);
  }
  throw new Error(`no fixture route for ${method} ${pathname}`);
};

test('runJob is a running job the frame accepts (id, state, startedAt, results)', () => {
  const job = runJob();
  assert.match(job.id, /^[a-f0-9]{8}(-[a-f0-9]{4}){3}-[a-f0-9]{12}$/i);
  assert.equal(job.state, 'running');
  assert.ok(Number.isFinite(Date.parse(job.startedAt)));
  assert.ok(Array.isArray(job.results));
  assert.ok(job.results.length <= 40);
  assert.equal(job.activePlatform, 'mac');
  assert.equal(job.hosts.mac.state, 'running');
});

test('POST /api/app-updates/start returns a running job and status then shows it', () => {
  const server = fakeServer({ rich: true, update: null });
  const started = call(server, 'POST', '/api/app-updates/start');
  assert.equal(started.status, 202);
  const job = JSON.parse(started.body).job;
  assert.equal(job.state, 'running');
  const status = call(server, 'GET', '/api/app-updates/status');
  assert.equal(status.status, 200);
  assert.equal(JSON.parse(status.body).job.state, 'running');
});

test('a second start while running is refused with the running job (409)', () => {
  const server = fakeServer({ rich: true, update: null });
  call(server, 'POST', '/api/app-updates/start');
  const again = call(server, 'POST', '/api/app-updates/start');
  assert.equal(again.status, 409);
  assert.equal(JSON.parse(again.body).job.state, 'running');
});

test('POST /api/app-updates/cancel acknowledges a running job', () => {
  const server = fakeServer({ rich: true, update: null });
  call(server, 'POST', '/api/app-updates/start');
  const cancelled = call(server, 'POST', '/api/app-updates/cancel');
  assert.equal(cancelled.status, 202);
  const body = JSON.parse(cancelled.body);
  assert.equal(body.cancelling, true);
  assert.equal(body.job.cancelRequested, true);
});
