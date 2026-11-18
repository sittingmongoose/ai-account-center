/** Max pair 2 M1: observe real IPC and the real Codex router over fixture files. */
import { expect, it, spyOn } from 'bun:test';
import express, { type Request, type Response } from 'express';
import http from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { RustService } from '../../../src/web-server/rust-service';
import codexRouter from '../../../src/web-server/routes/codex-routes';
import { invalidateCodexAuthProfilesCache } from '../../../src/codex-auth/codex-auth-dashboard-service';
import { codexProfileForms, emptyState } from '../../spec/profile-read-fixtures';
import routes from '../../../src/web-server/rust-service-routes.json';

it('keeps all standalone profile comparisons in TS and sends no private profile body', async () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-shadow-private-'));
  const saved = { ...process.env };
  const homeSpy = spyOn(os, 'homedir').mockReturnValue(home);
  let blockedPrivateReads = 0;
  const reads = ['readFileSync', 'statSync', 'existsSync'].map((name) => {
    const method = name as 'readFileSync';
    const original = fs[method];
    return spyOn(fs, method).mockImplementation(((file: unknown, ...args: unknown[]) => {
      if (
        typeof file === 'string' &&
        /\/(?:\.ccs|\.claude|\.codex|\.t3)(?:\/|$)/.test(file) &&
        !path.resolve(file).startsWith(home + '/')
      ) {
        blockedPrivateReads++;
        throw new Error('private file outside fixture refused');
      }
      return (original as (...args: unknown[]) => unknown)(file, ...args);
    }) as never);
  });
  const ccs = path.join(home, '.ccs');
  fs.mkdirSync(ccs);
  Object.assign(process.env, {
    CCS_HOME: home,
    CCS_DIR: ccs,
    CCS_DASHBOARD_AUTH_ENABLED: 'true',
    CODEX_HOME: path.join(home, '.codex'),
  });
  const received: { route: string; body: string }[] = [];
  const sink = http.createServer((req, res) => {
    let body = '';
    req.on('data', (chunk) => (body += chunk));
    req.on('end', () => {
      received.push({ route: req.url!, body });
      res.setHeader('x-aac-service-boot', 'fixture');
      res.setHeader('content-type', 'application/json');
      res.end('{}');
    });
  });
  const service = new RustService('shadow');
  let server: http.Server | undefined;
  try {
    const socketPath = path.join(home, 'http.sock');
    await new Promise<void>((resolve) => sink.listen(socketPath, resolve));
    Object.assign(service, {
      lease: {
        connection: { socketPath, ccsDir: ccs, secret: 'fixture', bootId: 'fixture' },
        owned: false,
        pid: null,
        close: () => undefined,
      },
    });
    codexProfileForms()[1].setup(ccs, home, emptyState());
    invalidateCodexAuthProfilesCache();
    const app = express();
    app.use((req, res, next) => {
      Object.assign(req, { sessionID: 'fixture', session: { authenticated: true } });
      res.locals.rustService = service;
      next();
    });
    app.use('/api/codex', codexRouter);
    server = app.listen(0, '127.0.0.1');
    await new Promise<void>((resolve) => server!.once('listening', resolve));
    const port = (server.address() as import('net').AddressInfo).port;
    const reply = await fetch(`http://127.0.0.1:${port}/api/codex/profiles`);
    expect(reply.status).toBe(200);
    const summary = (await reply.json()) as {
      profiles: Array<{ email: string; authValid: boolean; codexHome: string }>;
    };
    expect(
      summary.profiles.some((p) => p.email && p.authValid && p.codexHome.startsWith(ccs))
    ).toBe(true);
    expect(blockedPrivateReads).toBe(0);
    // Every table-enabled profile route is covered even if another router later uses compose.
    const req = {
      method: 'GET',
      sessionID: 'fixture',
      session: { authenticated: true },
      headers: {},
    } as unknown as Request;
    for (const row of routes.filter((r) => r.group === 'profiles')) {
      const privateFacts = {
        email: 'fixture@example.invalid',
        accountId: 'fixture-private-account',
        authValid: true,
        codexHome: ccs,
        history: 'fixture-private-history',
      };
      let built = false;
      expect(
        await service.compose(
          req,
          row.path,
          () => {
            built = true;
            return privateFacts;
          },
          () => privateFacts,
          () => true
        )
      ).toEqual(privateFacts);
      expect(built).toBe(false);
      await service.ownedValue(
        req,
        row.path,
        ccs,
        () => null,
        () => Promise.resolve(privateFacts)
      );
    }
    // Positive control: a non-profile shadow request actually reaches the protocol sink.
    await service.compose(
      req,
      '/api/accounts/settings',
      {},
      () => ({}),
      () => true
    );
    for (let i = 0; i < 100 && !received.some((r) => r.route === '/api/accounts/settings'); i++)
      await new Promise((resolve) => setTimeout(resolve, 10));
    expect(received.map((r) => r.route)).toEqual(['/api/accounts/settings']);
    expect(JSON.stringify(received)).not.toContain('fixture-private');
    expect(JSON.stringify(received)).not.toContain(ccs);
    expect(blockedPrivateReads).toBe(0);
  } finally {
    service.stop();
    if (server) {
      server.closeAllConnections();
      await new Promise<void>((resolve) => server!.close(() => resolve()));
    }
    sink.closeAllConnections();
    await new Promise<void>((resolve) => sink.close(() => resolve()));
    invalidateCodexAuthProfilesCache();
    for (const read of reads) read.mockRestore();
    homeSpy.mockRestore();
    process.env = saved;
    fs.rmSync(home, { recursive: true, force: true });
  }
});
