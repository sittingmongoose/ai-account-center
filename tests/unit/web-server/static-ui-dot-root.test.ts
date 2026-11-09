/**
 * The packaged UI is served from wherever the package is installed, which is usually under a
 * dot directory (~/.local/lib/node_modules/...). Express 5's `send` treats any dot segment of an
 * absolute sendFile path as a hidden file and answers 404, so page routes must send relative to
 * the UI root. Regression for the 2026-11-09 Express 5 install, where /, /login and /app/status
 * all answered 404.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import express from 'express';
import fs from 'fs';
import http from 'http';
import type { AddressInfo } from 'net';
import os from 'os';
import path from 'path';

import { loadStaticUi, pageRouteHandler } from '../../../src/web-server/static-ui';

const servers: http.Server[] = [];
const roots: string[] = [];

afterEach(() => {
  for (const server of servers.splice(0)) server.close();
  for (const root of roots.splice(0)) fs.rmSync(root, { recursive: true, force: true });
});

async function serveFrom(uiRoot: string): Promise<number> {
  const app = express();
  app.get('/{*splat}', pageRouteHandler(loadStaticUi(uiRoot), new Set(['claude', 'codex'])));
  const server = await new Promise<http.Server>((resolve) => {
    const started = app.listen(0, '127.0.0.1', () => resolve(started));
  });
  servers.push(server);
  return (server.address() as AddressInfo).port;
}

function get(port: number, requestPath: string): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    http
      .get({ host: '127.0.0.1', port, path: requestPath }, (res) => {
        let body = '';
        res.setEncoding('utf8');
        res.on('data', (chunk: string) => (body += chunk));
        res.on('end', () => resolve({ status: res.statusCode ?? 0, body }));
      })
      .on('error', reject);
  });
}

describe('page routes under a dot directory', () => {
  it('serves index.html and the Status page when the install path has a dot segment', async () => {
    const base = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-dot-root-'));
    roots.push(base);
    const uiRoot = path.join(base, '.local', 'lib', 'node_modules', 'aac', 'dist', 'ui');
    fs.mkdirSync(path.join(uiRoot, 'app'), { recursive: true });
    fs.writeFileSync(path.join(uiRoot, 'index.html'), '<!doctype html><p>shell</p>');
    fs.writeFileSync(path.join(uiRoot, 'app', 'index.html'), '<!doctype html><p>status page</p>');
    const port = await serveFrom(uiRoot);

    const shell = await get(port, '/');
    expect(shell.status).toBe(200);
    expect(shell.body).toContain('shell');

    const status = await get(port, '/app/status');
    expect(status.status).toBe(200);
    expect(status.body).toContain('status page');
  });
});
