// Native dashboard fixture: boots the cut launcher and proxies the deliverable route.
import { spawn, type ChildProcess } from 'node:child_process';
import { createServer as createHttpServer, request as httpRequest } from 'node:http';
import { createServer as createNetServer } from 'node:net';
import { createInterface } from 'node:readline';
import fs from 'node:fs';
import path from 'node:path';

const [port, root, cli] = process.argv.slice(2);
if (!port || !root || !cli) {
  console.error('usage: server.ts <port> <fixture-root> <native-cli>');
  process.exit(2);
}
if (!fs.existsSync(cli)) {
  console.error(`native dashboard missing: ${cli}`);
  process.exit(2);
}

const share = path.join(root, 'share');
fs.mkdirSync(share, { recursive: true });
fs.writeFileSync(
  path.join(share, 'concept.html'),
  `<!doctype html><h1>Concept</h1>
<script>document.body.dataset.executed='yes';try{document.body.dataset.stolen=document.cookie}catch{}
fetch('/api/private');</script><img src="/api/private"><iframe src="/api/private"></iframe>`
);

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createNetServer();
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      if (!address || typeof address === 'string') {
        server.close();
        reject(new Error('no port'));
        return;
      }
      server.close((error) => (error ? reject(error) : resolve(address.port)));
    });
    server.on('error', reject);
  });
}

function ready(child: ChildProcess, timeoutMs: number): Promise<void> {
  return new Promise((resolve, reject) => {
    if (!child.stdout) {
      reject(new Error('native dashboard has no stdout'));
      return;
    }
    let settled = false;
    const timeout = setTimeout(
      () => finish(new Error('native dashboard did not start')),
      timeoutMs
    );
    const finish = (error?: Error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      if (error) reject(error);
      else resolve();
    };
    child.once('error', (error) => finish(error));
    child.once('exit', (status) => finish(new Error(`native dashboard exited with ${status}`)));
    const output = createInterface({ input: child.stdout });
    output.on('line', (line) => {
      process.stdout.write(`native: ${line}\n`);
      if (/aac-http public listening on 127\.0\.0\.1:\d+/.test(line)) finish();
    });
  });
}

const nativePort = await freePort();
const child = spawn(
  '/usr/bin/bwrap',
  [
    '--die-with-parent',
    '--bind',
    '/',
    '/',
    '--dev',
    '/dev',
    '--proc',
    '/proc',
    '--bind',
    share,
    '/mnt/Cursor/share',
    cli,
    'dashboard',
    '--no-open',
    '--host',
    '127.0.0.1',
    '--port',
    String(nativePort),
  ],
  { env: process.env, stdio: ['ignore', 'pipe', 'pipe'] }
);
child.stderr?.on('data', (chunk: Buffer) => {
  process.stderr.write(chunk);
});
await ready(child, 120_000);

let hits = 0;
const proxy = createHttpServer((req, res) => {
  const url = req.url || '/';
  if (url === '/hits') {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ hits }));
    return;
  }
  if (url === '/api/private' || url.startsWith('/api/private?')) {
    hits += 1;
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ secret: 'fixture-only' }));
    return;
  }
  const upstream = httpRequest(
    {
      hostname: '127.0.0.1',
      port: nativePort,
      path: url,
      method: req.method,
      headers: req.headers,
    },
    (up) => {
      res.writeHead(up.statusCode ?? 502, up.headers);
      up.pipe(res);
    }
  );
  upstream.on('error', () => {
    res.statusCode = 502;
    res.end('native dashboard proxy failed');
  });
  req.pipe(upstream);
});

const stop = () => {
  child.kill('SIGTERM');
  proxy.close();
};
process.on('SIGTERM', stop);
process.on('SIGINT', stop);
child.on('exit', (code) => {
  proxy.close();
  process.exit(code ?? 1);
});
proxy.listen(Number(port), '127.0.0.1', () => {
  process.stdout.write(`aac-http public listening on 127.0.0.1:${port}\n`);
});
