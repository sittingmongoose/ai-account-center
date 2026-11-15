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
const build = JSON.stringify({ commit: 'unknown', sha256, protocol: 2, storeSchema: 2 });
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
function start(boot) {
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
    res.statusCode = 404;
    res.end('{}');
  });
  server.listen(boot.socketPath, () => fs.chmodSync(boot.socketPath, 0o600));
}
