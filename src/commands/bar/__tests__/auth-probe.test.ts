import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as http from 'http';
import * as os from 'os';
import * as path from 'path';
import {
  BAR_AUTH_NONCE_HEADER,
  BAR_AUTH_TOKEN_HEADER,
  createBarAuthProof,
} from '../../../utils/bar-auth-token';
import { defaultFindRunningServer, probeRecordedBarServer } from '../bar-server-probe';
import { defaultWaitForServerLive } from '../launch-subcommand';
import { serializeBarServerProcessRecord } from '../bar-process-control';
import { handleBarStatus } from '../status-subcommand';

const token = 'a'.repeat(64);
let temporary: string;
let ccsDir: string;
let previousCcsHome: string | undefined;
let previousExitCode: typeof process.exitCode;
const servers = new Set<http.Server>();

function seedToken(value = token): string {
  const file = path.join(ccsDir, 'bar', '.auth-token');
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${value}\n`, { mode: 0o600 });
  return file;
}

async function listen(handler: http.RequestListener): Promise<string> {
  const server = http.createServer(handler);
  servers.add(server);
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address() as { port: number };
  return `http://127.0.0.1:${address.port}`;
}

describe('authenticated liveness fixtures', () => {
  beforeEach(() => {
    temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'account-center-auth-probe-test-'));
    ccsDir = path.join(temporary, '.ccs');
    previousCcsHome = process.env.CCS_HOME;
    previousExitCode = process.exitCode;
    process.env.CCS_HOME = ccsDir;
    process.exitCode = 0;
  });

  afterEach(async () => {
    for (const server of servers) {
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
    servers.clear();
    if (previousCcsHome === undefined) delete process.env.CCS_HOME;
    else process.env.CCS_HOME = previousCcsHome;
    process.exitCode = previousExitCode ?? 0;
    fs.rmSync(temporary, { recursive: true, force: true });
  });

  describe('provider-free authenticated bar liveness', () => {
    it('discovers only explicitly owned fixture endpoints when targets are injected', async () => {
      seedToken();
      const requests: string[] = [];
      const baseUrl = await listen((request, response) => {
        requests.push(request.url ?? '');
        response.writeHead(200, {
          [BAR_AUTH_TOKEN_HEADER]: createBarAuthProof(
            token,
            String(request.headers[BAR_AUTH_NONCE_HEADER])
          ),
        });
        response.end(JSON.stringify({ status: 'ok' }));
      });
      const port = Number(new URL(baseUrl).port);
      expect(await defaultFindRunningServer(ccsDir, { targets: [{ baseUrl, port }] })).toEqual({
        baseUrl,
        port,
        authRequired: false,
      });
      expect(requests).toEqual(['/api/bar/auth']);
    });

    it('refuses non-loopback injected targets without making a request', async () => {
      seedToken();
      expect(
        await defaultFindRunningServer(ccsDir, {
          targets: [{ baseUrl: 'http://192.0.2.1:3000', port: 3000 }],
        })
      ).toBeNull();
    });

    it('bounds a continually dripping incomplete response with the absolute deadline', async () => {
      seedToken();
      const baseUrl = await listen((request) => {
        request.socket.write('HTTP/1.1 200 OK\r\nX-Incomplete: ');
        const drip = setInterval(() => request.socket.write(' '), 50);
        request.socket.once('close', () => clearInterval(drip));
      });
      const started = Date.now();
      expect(await probeRecordedBarServer(ccsDir, baseUrl)).toBe(false);
      expect(Date.now() - started).toBeGreaterThanOrEqual(1300);
      expect(Date.now() - started).toBeLessThan(3000);
    });

    it('returns the priority owned hit without waiting for a lower streaming endpoint', async () => {
      seedToken();
      const priorityUrl = await listen((request, response) => {
        response.writeHead(200, {
          [BAR_AUTH_TOKEN_HEADER]: createBarAuthProof(
            token,
            String(request.headers[BAR_AUTH_NONCE_HEADER])
          ),
        });
        response.end(JSON.stringify({ status: 'ok' }));
      });
      const streamingUrl = await listen((request) => {
        request.socket.write('HTTP/1.1 200 OK\r\nX-Incomplete: ');
        const drip = setInterval(() => request.socket.write(' '), 50);
        request.socket.once('close', () => clearInterval(drip));
      });
      const started = Date.now();
      const result = await defaultFindRunningServer(ccsDir, {
        targets: [priorityUrl, streamingUrl].map((baseUrl) => ({
          baseUrl,
          port: Number(new URL(baseUrl).port),
        })),
      });
      expect(result?.baseUrl).toBe(priorityUrl);
      expect(Date.now() - started).toBeLessThan(1000);
    });

    it('uses only the auth endpoint, sends a nonce rather than the private token, and preserves files', async () => {
      const file = seedToken();
      const before = [
        fs.readFileSync(file, 'utf8'),
        fs.statSync(file).mtimeMs,
        fs.statSync(file).mode,
      ];
      const seen: { url?: string; nonce: string; request: string }[] = [];
      const baseUrl = await listen((request, response) => {
        const nonce = String(request.headers[BAR_AUTH_NONCE_HEADER] ?? '');
        seen.push({ url: request.url, nonce, request: JSON.stringify(request.headers) });
        response.writeHead(200, { [BAR_AUTH_TOKEN_HEADER]: createBarAuthProof(token, nonce) });
        response.end(JSON.stringify({ status: 'ok' }));
      });

      expect(await probeRecordedBarServer(ccsDir, baseUrl)).toBe(true);
      expect(await probeRecordedBarServer(ccsDir, baseUrl)).toBe(true);
      expect(seen.map((request) => request.url)).toEqual(['/api/bar/auth', '/api/bar/auth']);
      expect(seen[0].nonce).not.toBe(seen[1].nonce);
      expect(seen.every((request) => /^[a-f0-9]{64}$/.test(request.nonce))).toBe(true);
      expect(seen.every((request) => !request.request.includes(token))).toBe(true);
      expect([
        fs.readFileSync(file, 'utf8'),
        fs.statSync(file).mtimeMs,
        fs.statSync(file).mode,
      ]).toEqual(before);
    });

    it('rejects a successful response from a different service', async () => {
      seedToken();
      const baseUrl = await listen((_request, response) => {
        response.writeHead(200);
        response.end(JSON.stringify({ status: 'ok' }));
      });
      expect(await probeRecordedBarServer(ccsDir, baseUrl)).toBe(false);
    });

    it('rejects a proof replayed for the preceding nonce', async () => {
      seedToken();
      let previousProof = '';
      const baseUrl = await listen((request, response) => {
        const freshProof = createBarAuthProof(
          token,
          String(request.headers[BAR_AUTH_NONCE_HEADER])
        );
        response.writeHead(200, { [BAR_AUTH_TOKEN_HEADER]: previousProof || freshProof });
        previousProof = freshProof;
        response.end(JSON.stringify({ status: 'ok' }));
      });
      expect(await probeRecordedBarServer(ccsDir, baseUrl)).toBe(true);
      expect(await probeRecordedBarServer(ccsDir, baseUrl)).toBe(false);
    });

    it('recognizes an authenticated protected server without attempting to sign in', async () => {
      seedToken();
      let requests = 0;
      const baseUrl = await listen((request, response) => {
        requests += 1;
        response.writeHead(401, {
          [BAR_AUTH_TOKEN_HEADER]: createBarAuthProof(
            token,
            String(request.headers[BAR_AUTH_NONCE_HEADER])
          ),
        });
        response.end(JSON.stringify({ error: 'Authentication required' }));
      });
      expect(await probeRecordedBarServer(ccsDir, baseUrl)).toBe(true);
      expect(requests).toBe(1);
    });

    it('rejects untrusted recorded URLs before making any requests', async () => {
      seedToken();
      let requests = 0;
      const baseUrl = await listen((_request, response) => {
        requests += 1;
        response.end();
      });
      for (const url of [
        baseUrl.replace('127.0.0.1', 'localhost'),
        baseUrl.replace('http:', 'https:'),
        baseUrl.replace('://', '://user:pass@'),
        `${baseUrl}/elsewhere`,
        `${baseUrl}?query=1`,
        `${baseUrl}#fragment`,
        'http://192.0.2.1:3000',
        'not a URL',
      ]) {
        expect(await probeRecordedBarServer(ccsDir, url)).toBe(false);
      }
      expect(requests).toBe(0);
    });

    it('does not create a missing authentication token during status checks', async () => {
      let requests = 0;
      const baseUrl = await listen((_request, response) => {
        requests += 1;
        response.end();
      });
      expect(await probeRecordedBarServer(ccsDir, baseUrl)).toBe(false);
      expect(fs.existsSync(ccsDir)).toBe(false);
      expect(requests).toBe(0);
    });

    it('does not follow a token alias into private connection material', async () => {
      const file = seedToken();
      const connection = path.join(ccsDir, 'bar', 'accounts-connection.json');
      fs.writeFileSync(connection, 'private fixture connection', { mode: 0o600 });
      fs.unlinkSync(file);
      fs.symlinkSync(connection, file);
      let requests = 0;
      const baseUrl = await listen((_request, response) => {
        requests += 1;
        response.end();
      });
      expect(await probeRecordedBarServer(ccsDir, baseUrl)).toBe(false);
      expect(requests).toBe(0);
      expect(fs.readFileSync(connection, 'utf8')).toBe('private fixture connection');
      expect(fs.lstatSync(file).isSymbolicLink()).toBe(true);
    });

    it('uses the authenticated auth endpoint when waiting for an explicitly selected server', async () => {
      seedToken();
      const requests: string[] = [];
      const baseUrl = await listen((request, response) => {
        requests.push(request.url ?? '');
        response.writeHead(200, {
          [BAR_AUTH_TOKEN_HEADER]: createBarAuthProof(
            token,
            String(request.headers[BAR_AUTH_NONCE_HEADER])
          ),
        });
        response.end(JSON.stringify({ status: 'ok' }));
      });
      await defaultWaitForServerLive(baseUrl, ccsDir);
      expect(requests).toEqual(['/api/bar/auth']);
    });

    it('reports authenticated login requirements without retrying or attempting a login', async () => {
      seedToken();
      const requests: string[] = [];
      const baseUrl = await listen((request, response) => {
        requests.push(request.url ?? '');
        response.writeHead(401, {
          [BAR_AUTH_TOKEN_HEADER]: createBarAuthProof(
            token,
            String(request.headers[BAR_AUTH_NONCE_HEADER])
          ),
        });
        response.end(JSON.stringify({ error: 'Authentication required' }));
      });
      await expect(defaultWaitForServerLive(baseUrl, ccsDir)).rejects.toThrow(
        'requires dashboard authentication (HTTP 401)'
      );
      expect(requests).toEqual(['/api/bar/auth']);
    });

    it('does not report a live PID as connected when its HTTP service lacks identity proof', async () => {
      seedToken();
      const baseUrl = await listen((_request, response) =>
        response.end(JSON.stringify({ status: 'ok' }))
      );
      const output: string[] = [];
      const previousLog = console.log;
      console.log = (...values: unknown[]) => output.push(values.map(String).join(' '));
      try {
        await handleBarStatus([], {
          getCcsDir: () => ccsDir,
          readPidFile: () =>
            serializeBarServerProcessRecord({ pid: 4242, birthIdentity: 'fixture-birth' }),
          isProcessAlive: () => true,
          readBarJsonBaseUrl: () => baseUrl,
        });
      } finally {
        console.log = previousLog;
      }
      expect(output.join('\n')).toContain('HTTP probe failed');
      expect(output.some((line) => line.startsWith('[OK]'))).toBe(false);
    });
  });
});
