#!/usr/bin/env node
// A stand-in for the aac-http binary in the TS lifecycle tests (no Rust on the VM). It speaks the
// L1 private protocol the bridge relies on: the boot line on stdin, a 0600 Unix socket, the boot
// secret, the boot id header, the build identity on health (sha256 of this very file, as the real
// service hashes its own image), named declines, and exit when stdin closes. The settings file
// steers it: a valid integer answers, `"hang"` never answers, `"slow"` answers after 300 ms, and
// anything else is a named decline.
import fs from 'node:fs';
import http from 'node:http';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';

const self = fileURLToPath(import.meta.url);
const sha256 = createHash('sha256').update(fs.readFileSync(self)).digest('hex');
const build = JSON.stringify({ commit: 'f'.repeat(40), sha256, protocol: 3, storeSchema: 2 });
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => {
  input += chunk;
  const end = input.indexOf('\n');
  if (end < 0 || server.listening) return;
  start(JSON.parse(input.slice(0, end)));
});
process.stdin.on('end', () => process.exit(0));
process.on('SIGTERM', () => process.exit(0));

const server = http.createServer();
// R3: the fake store host's memory (the real one's semantics run on Nas1 binaries).
const store = { opens: 0, records: 0, closed: true };
const storeStatus = () =>
  JSON.stringify({
    state: 'ready',
    instanceId: 'f'.repeat(32),
    storeSchemaVersion: 2,
    openedAt: 1,
    cacheBytes: 1,
    retentionDays: 0,
    redbBytes: 1,
    logs: {},
    checkpoints: {},
    meta: {},
    redbStats: {},
    capture: {},
    storageHealth: {},
    findings: [],
  });
function storeCall(boot, req, res) {
  let principal;
  try {
    principal = JSON.parse(req.headers['x-aac-principal']);
  } catch {
    principal = null;
  }
  if (principal?.role !== 'service') {
    res.statusCode = 401;
    res.end('{"error":"Authentication required"}');
    return;
  }
  if (!boot.store) {
    res.statusCode = 500;
    res.end('{"error":"service_down: the store host is not enabled on this boot"}');
    return;
  }
  const method = req.url.slice('/internal/store/'.length);
  let body = '';
  req.on('data', (chunk) => (body += chunk));
  req.on('end', () => {
    const fail = (error) => {
      res.statusCode = 500;
      res.end(JSON.stringify({ error }));
    };
    if (method === 'hostIdentity') {
      res.end(JSON.stringify({ commit: 'f'.repeat(40), storeSchema: 2, bindingVersion: 2 }));
    } else if (method === 'open') {
      store.opens++;
      store.closed = false;
      res.end(storeStatus());
    } else if (method === 'close') {
      store.closed = true;
      res.end('{"closed":true}');
    } else if (store.closed) fail('closed: the store is not open');
    else if (method === 'status') res.end(storeStatus());
    else if (method === 'record') {
      store.records += JSON.parse(body).observations?.length ?? 0;
      res.end('{"sampled":1,"seen":0,"skipped":0,"rejected":0}');
    } else if (method === 'counts')
      res.end(JSON.stringify({ opens: store.opens, records: store.records }));
    else if (method === 'hostJobs')
      res.end(
        '{"open":true,"bootError":null,"jobs":{"passes":0,"lastPassMs":null,"lastError":null,"lastErrorCode":null,"lastErrorMs":null,"lastShareFailedMs":null}}'
      );
    else fail(`invalid: unknown method ${method}`);
  });
}
function start(boot) {
  // Names only (never values): the spawn-env tests assert which names reached the child.
  fs.writeFileSync(
    `${boot.ccsDir}/fixture-spawn-env.json`,
    JSON.stringify(Object.keys(process.env)),
    { mode: 0o600 }
  );
  server.on('request', (req, res) => {
    res.setHeader('x-aac-service-boot', boot.bootId);
    if (req.headers['x-aac-boot-secret'] !== boot.secret) {
      res.statusCode = 401;
      res.end('{"error":"Authentication required"}');
      return;
    }
    if (req.url === '/api/health') {
      res.setHeader('x-aac-service-build', build);
      res.end('{"status":"ok"}');
      return;
    }
    if (req.url === '/api/accounts/settings') {
      let text = '';
      try {
        text = fs.readFileSync(`${boot.ccsDir}/account-refresh-settings.json`, 'utf8');
      } catch {
        text = '';
      }
      const decline = (reason) => {
        res.statusCode = 503;
        res.setHeader('x-aac-decline', reason);
        res.end('{"error":"TypeScript settings fallback required"}');
      };
      if (text === '"hang"') return;
      if (text === '"slow"') {
        setTimeout(() => res.end('{"refreshIntervalSeconds":90}'), 300);
        return;
      }
      let value;
      try {
        value = JSON.parse(text);
      } catch {
        return decline(text ? 'shape' : 'unsupported');
      }
      const n = value?.refreshIntervalSeconds;
      if (!Number.isInteger(n)) return decline('shape');
      if (n < 30 || n > 3600) return decline('number');
      res.end(JSON.stringify({ refreshIntervalSeconds: n }));
      return;
    }
    if (req.url.startsWith('/internal/store/')) return storeCall(boot, req, res);
    // R3: a body without the writer snapshot is the hosted status read.
    if (req.url === '/api/store/status') {
      let body = '';
      req.on('data', (chunk) => (body += chunk));
      req.on('end', () => {
        let snapshot;
        try {
          snapshot = JSON.parse(body)?.snapshot;
        } catch {
          snapshot = undefined;
        }
        if (snapshot !== undefined) return res.end(JSON.stringify(snapshot));
        if (!boot.store || store.closed) {
          res.statusCode = 503;
          res.setHeader('x-aac-decline', 'unsupported');
          return res.end('{"error":"TypeScript read fallback required"}');
        }
        res.end(storeStatus());
      });
      return;
    }
    if (req.url === '/api/claude/desktop-profiles' && req.method === 'GET') {
      res.end('{"profiles":[]}');
      return;
    }
    if (req.url.startsWith('/internal/agent/') && req.method === 'POST') {
      let body = '';
      req.on('data', (chunk) => (body += chunk));
      req.on('end', () => {
        const callId = JSON.parse(body).callId;
        res.setHeader('content-type', 'application/x-ndjson');
        res.end(
          JSON.stringify({
            protocol: 1,
            id: callId,
            type: 'result',
            value: { ok: true, verb: req.url.split('/').pop() },
          }) + '\n'
        );
      });
      return;
    }
    // R2b L2: the fed Status read (x-aac-feed) answers view bytes; the envelope read answers JSON.
    // status-fed.json steers the fed path: '"busy"' declines busy, anything else serves.
    if (req.url === '/api/status') {
      if (req.headers['x-aac-feed']) {
        let text = '';
        try {
          text = fs.readFileSync(`${boot.ccsDir}/status-fed.json`, 'utf8');
        } catch {
          text = '';
        }
        if (text === '"busy"') {
          res.statusCode = 503;
          res.setHeader('x-aac-decline', 'busy');
          res.end('{"error":"TypeScript status fallback required"}');
          return;
        }
        res.setHeader('x-aac-view', 'status@1');
        res.setHeader('etag', `W/"${'e'.repeat(64)}"`);
        res.end('{"version":1,"source":"fed"}');
        return;
      }
      let body = '';
      req.on('data', (chunk) => (body += chunk));
      req.on('end', () => res.end('{"version":1,"source":"envelope"}'));
      return;
    }
    res.statusCode = 404;
    res.end('{}');
  });
  server.listen(boot.socketPath, () => fs.chmodSync(boot.socketPath, 0o600));
}
