// Native Rust upgrade harness for web-dashboard/tests/browser/upgrade.
// Serves the built Leptos app through the real native Rust dashboard server (dist/native/ai-account-center and
// dist/native/aac-http, both compiled from crates/aac-http and serving static assets via public_auth/static_ui.rs).
// Uses an isolated temporary CCS_HOME (never the user's real home or real account data) with real loopback auth
// setup/login, real APIs and real /ws. A thin Node http router (standard library only, no Express and no TS static
// helpers) multiplexes builds on one origin so a browser context can hold one service-worker scope and session cookie
// while the server switches from an older build to a newer build, injects a single failing asset path, delays classic
// routes, or replays the frozen previous-release service worker (fixtures/previous).
//   node native-server.mjs <port> <repoRoot> <runTempDir>
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const port = Number(process.argv[2]);
const repo = path.resolve(process.argv[3]);
const runTempDir = path.resolve(process.argv[4]);

if (!Number.isInteger(port) || port <= 0) throw new Error(`Invalid listen port: ${process.argv[2]}`);

const ext = process.platform === 'win32' ? '.exe' : '';
const cliBin = path.join(repo, 'dist', 'native', `ai-account-center${ext}`);
const httpBin = path.join(repo, 'dist', 'native', `aac-http${ext}`);
const uiDir = process.env.AAC_UPGRADE_UI_DIR
  ? path.resolve(process.env.AAC_UPGRADE_UI_DIR)
  : path.join(repo, 'dist', 'ui');
const OLD = path.join(__dirname, 'fixtures', 'previous');
/** The previous release's shell stamp, as its installed worker carries it (any 12 hex digits other than ours). */
const OLD_STAMP = 'f52e1a7c6000';

for (const [label, target] of [
  ['native dashboard binary', cliBin],
  ['native aac-http binary', httpBin],
  ['built Leptos frame', path.join(uiDir, 'app', 'index.html')],
  ['UI build manifest', path.join(uiDir, 'ui-build-manifest.json')],
]) {
  if (!fs.existsSync(target)) {
    throw new Error(`Missing ${label} at ${target}. Run \`bun run build\` on a Rust build host first.`);
  }
}

const flipHex = (id) => id.replace(/[0-9a-f]/g, (c) => ((parseInt(c, 16) + 8) % 16).toString(16));

function readManifest(dir) {
  return JSON.parse(fs.readFileSync(path.join(dir, 'ui-build-manifest.json'), 'utf8'));
}

/**
 * Derives an older packaged UI from `sourceUiDir` inside the run's temporary folder (never mutating repo/dist):
 * its runtime folder and shell build id are flipped to distinct 12-hex ids, and precompressed variants are omitted
 * so Rust StaticUi serves the rewritten files directly.
 */
function buildOlderUi(sourceUiDir, destDir) {
  const manifest = readManifest(sourceUiDir);
  const pkg = manifest.buildId;
  const stamp = manifest.shellBuildId;
  const ids = { pkg: flipHex(pkg), stamp: flipHex(stamp) };
  fs.rmSync(destDir, { recursive: true, force: true });
  fs.cpSync(sourceUiDir, destDir, {
    recursive: true,
    filter: (source) => !/\.(br|gz)$/.test(source),
  });
  fs.renameSync(path.join(destDir, 'pkg', pkg), path.join(destDir, 'pkg', ids.pkg));
  const rewrite = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const file = path.join(dir, entry.name);
      if (entry.isDirectory()) rewrite(file);
      else if (/\.(html|js|mjs|json)$/.test(entry.name)) {
        const text = fs.readFileSync(file, 'utf8');
        let next = text.replaceAll(`pkg/${pkg}/`, `pkg/${ids.pkg}/`).replaceAll(`pkg/${pkg}`, `pkg/${ids.pkg}`);
        if (entry.name === 'sw.js' && dir === destDir) next = next.replace(`'${stamp}'`, `'${ids.stamp}'`);
        if (next !== text) fs.writeFileSync(file, next);
      }
    }
  };
  rewrite(destDir);
  const record = { ...manifest, buildId: ids.pkg, shellBuildId: ids.stamp, precompressed: [] };
  if (manifest.wasm?.path) record.wasm = { ...manifest.wasm, path: manifest.wasm.path.replace(pkg, ids.pkg) };
  fs.writeFileSync(path.join(destDir, 'ui-build-manifest.json'), `${JSON.stringify(record, null, 2)}\n`);
  return record;
}

function freePort() {
  return new Promise((resolve, reject) => {
    const probe = net.createServer();
    probe.unref();
    probe.on('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const addr = probe.address();
      const chosen = typeof addr === 'object' && addr ? addr.port : 0;
      probe.close((err) => (err ? reject(err) : resolve(chosen)));
    });
  });
}

function isolatedEnv(homeDir, extra = {}) {
  const base = { ...process.env };
  for (const key of [
    'AAC_T3_STATE_DB',
    'AAC_AGENT_PACKAGE_ROOT',
    'AAC_AGENT_CATALOG',
    'AAC_AGENT_IDENTITY_FILE',
    'AAC_AGENT_KNOWN_HOSTS_FILE',
    'AAC_RUST_COMPOSE_UPSTREAM',
    'AAC_RUST_COMPOSE_PROOF',
    'AAC_RUST_PUBLIC_LISTEN',
    'AAC_RUST_UI_DIR',
    'AAC_RUST_HUB_CACHE',
    'AAC_RUST_PUBLIC_EVENT_STDIN',
  ]) {
    delete base[key];
  }
  return {
    ...base,
    CCS_HOME: homeDir,
    CCS_DIR: path.join(homeDir, '.ccs'),
    HOME: homeDir,
    USERPROFILE: homeDir,
    APPDATA: path.join(homeDir, 'AppData', 'Roaming'),
    LOCALAPPDATA: path.join(homeDir, 'AppData', 'Local'),
    XDG_CONFIG_HOME: path.join(homeDir, '.config'),
    XDG_DATA_HOME: path.join(homeDir, '.local', 'share'),
    XDG_STATE_HOME: path.join(homeDir, '.local', 'state'),
    XDG_CACHE_HOME: path.join(homeDir, '.cache'),
    CODEX_HOME: path.join(homeDir, '.codex'),
    CLAUDE_CONFIG_DIR: path.join(homeDir, '.claude'),
    AAC_AGENT_REPORTS_DIR: path.join(homeDir, 'agent-reports'),
    AAC_PM_MAIL_REGISTRY_DIR: path.join(homeDir, 'agent-reports', 'pm-mail-registry'),
    AAC_STATUS_DOORBELL: 'off',
    AAC_AGENT_VERBS: 'off',
    AAC_AGENT_TRANSPORT: 'off',
    AAC_STORE_SHARE_BACKUP_DIR: 'off',
    AAC_CURSOR_BILLING_ENABLED: '0',
    AAC_CURSOR_API_BASE: 'http://127.0.0.1:1',
    SSH_AUTH_SOCK: '',
    ...extra,
  };
}

function healthCheck(targetPort) {
  return new Promise((resolve) => {
    const req = http.request({ host: '127.0.0.1', port: targetPort, path: '/api/health', timeout: 1000 }, (res) => {
      res.resume();
      resolve(res.statusCode === 200);
    });
    req.on('error', () => resolve(false));
    req.on('timeout', () => {
      req.destroy();
      resolve(false);
    });
    req.end();
  });
}

async function waitForHealth(name, child, targetPort, timeoutMs = 45000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`Native server "${name}" exited early with code ${child.exitCode}`);
    }
    if (await healthCheck(targetPort)) return;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`Timed out waiting for native server "${name}" on 127.0.0.1:${targetPort}`);
}

const children = [];
let shuttingDown = false;

function stopChildren() {
  shuttingDown = true;
  for (const { child } of children) {
    if (child.exitCode === null) {
      try {
        child.kill('SIGTERM');
      } catch {
        // already stopped
      }
    }
  }
}

function spawnTracked(name, command, args, env) {
  const child = spawn(command, args, { cwd: repo, env, stdio: ['ignore', 'pipe', 'pipe'] });
  const tail = [];
  const remember = (chunk) => {
    for (const line of chunk.toString('utf8').split(/\r?\n/)) {
      if (!line) continue;
      tail.push(line);
      if (tail.length > 40) tail.shift();
    }
  };
  child.stdout?.on('data', remember);
  child.stderr?.on('data', remember);
  child.on('exit', (code, signal) => {
    if (!shuttingDown) {
      console.error(`[upgrade-native] child "${name}" exited unexpectedly (code=${code}, signal=${signal})`);
      if (tail.length) console.error(tail.join('\n'));
      stopChildren();
      process.exit(1);
    }
  });
  children.push({ name, child, tail });
  return child;
}

fs.mkdirSync(runTempDir, { recursive: true });
const home = path.join(runTempDir, 'home');
const reports = path.join(home, 'agent-reports');
fs.mkdirSync(path.join(home, '.ccs'), { recursive: true });
fs.mkdirSync(path.join(reports, 'pm-mail-registry'), { recursive: true });

// Seed synthetic Status and Scoreboard fixtures into the temporary home so no real share is read.
{
  const { writeStatusFixture } = await import(pathToFileURL(path.join(repo, 'scripts', 'status-fixture.mjs')).href);
  writeStatusFixture(reports, { ccsDir: path.join(home, '.ccs'), withKinds: true });
  const { writeScoreboardFixture } = await import(
    pathToFileURL(path.join(repo, 'scripts', 'scoreboard-fixture.mjs')).href
  );
  writeScoreboardFixture(reports);
}

const currentManifest = readManifest(uiDir);
const olderUiDir = process.env.AAC_UPGRADE_OLDER_UI
  ? path.resolve(process.env.AAC_UPGRADE_OLDER_UI)
  : path.join(runTempDir, 'older-ui');
const olderManifest = process.env.AAC_UPGRADE_OLDER_UI
  ? readManifest(olderUiDir)
  : buildOlderUi(uiDir, olderUiDir);

const buildSummary = (dir, m) => ({
  dir,
  buildId: m.buildId,
  shellBuildId: m.shellBuildId,
  wasmPath: m.wasm?.path ?? `pkg/${m.buildId}/aac_web_bg.wasm`,
});

const builds = {
  current: buildSummary(uiDir, currentManifest),
  older: buildSummary(olderUiDir, olderManifest),
};
// Keep 'second' as an alias for 'older' so callers using either name stay compatible.
builds.second = builds.older;

const backends = {};

// Primary native Rust dashboard server (ai-account-center, built from crates/aac-http): owns the temporary CCS_HOME,
// real loopback auth setup/login, real APIs, /ws, and serves the current built UI via Rust StaticUi.
const mainPort = await freePort();
const mainChild = spawnTracked(
  'current (ai-account-center)',
  cliBin,
  ['dashboard', '--no-open', '--host', '127.0.0.1', '--port', String(mainPort)],
  isolatedEnv(home, { AAC_RUST_UI_DIR: uiDir })
);
backends.current = mainPort;

// Secondary native Rust static server (aac-http --public) serving the older UI build via the exact same Rust StaticUi.
async function startStaticBackend(key, targetUiDir) {
  const staticPort = await freePort();
  const staticHome = path.join(runTempDir, `static-${key}`);
  fs.mkdirSync(path.join(staticHome, '.ccs'), { recursive: true });
  const child = spawnTracked(
    `${key} (aac-http --public)`,
    httpBin,
    ['--public'],
    isolatedEnv(staticHome, {
      AAC_RUST_PUBLIC_LISTEN: `127.0.0.1:${staticPort}`,
      AAC_RUST_UI_DIR: targetUiDir,
    })
  );
  backends[key] = staticPort;
  return { name: key, child, port: staticPort };
}

const started = [
  { name: 'current', child: mainChild, port: mainPort },
  await startStaticBackend('older', olderUiDir),
];
backends.second = backends.older;

for (const [name, envKey] of [
  ['previous', 'AAC_UPGRADE_PREVIOUS_UI'],
  ['fix1', 'AAC_UPGRADE_FIX1_UI'],
]) {
  if (process.env[envKey]) {
    const dir = path.resolve(process.env[envKey]);
    builds[name] = buildSummary(dir, readManifest(dir));
    started.push(await startStaticBackend(name, dir));
  }
}

await Promise.all(started.map(({ name, child, port: p }) => waitForHealth(name, child, p)));

const state = { oldWorker: false, uiVersion: 'current', delayClassicMs: 0, failPath: null };
const isApiPath = (pathname) => /^\/(?:api|v0)(?:\/|$)|^\/ws$/i.test(pathname);

function readJsonBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let bytes = 0;
    req.on('data', (chunk) => {
      bytes += chunk.length;
      if (bytes > 65536) {
        reject(new Error('Control request body too large.'));
        req.destroy();
        return;
      }
      chunks.push(chunk);
    });
    req.on('end', () => {
      try {
        resolve(chunks.length ? JSON.parse(Buffer.concat(chunks).toString('utf8')) : {});
      } catch (err) {
        reject(err);
      }
    });
    req.on('error', reject);
  });
}

function forwardHttp(req, res, targetPort, { stripCookie = false } = {}) {
  const headers = { ...req.headers };
  if (stripCookie) delete headers.cookie;
  const upstream = http.request(
    { host: '127.0.0.1', port: targetPort, path: req.url, method: req.method, headers },
    (upstreamRes) => {
      if (!res.headersSent) res.writeHead(upstreamRes.statusCode || 502, upstreamRes.headers);
      upstreamRes.pipe(res);
    }
  );
  upstream.on('error', () => {
    if (!res.headersSent) res.writeHead(502, { 'content-type': 'text/plain; charset=utf-8' });
    if (!res.writableEnded) res.end('Upstream native server unavailable.');
  });
  req.on('aborted', () => upstream.destroy());
  res.on('close', () => {
    if (!res.writableEnded) upstream.destroy();
  });
  req.pipe(upstream);
}

const server = http.createServer(async (req, res) => {
  let pathname = '/';
  try {
    pathname = new URL(req.url || '/', 'http://127.0.0.1').pathname;
  } catch {
    pathname = '/';
  }

  // Plain text: an HTML page here would be mirrored to the start page by the old worker.
  if (req.method === 'GET' && pathname === '/__upgrade/ready') {
    res.writeHead(200, { 'content-type': 'text/plain; charset=utf-8', 'cache-control': 'no-store' });
    res.end('fixture only');
    return;
  }
  if (pathname === '/__upgrade/control') {
    if (req.method === 'GET') {
      res.writeHead(200, { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store' });
      res.end(JSON.stringify({ ...state, builds }));
      return;
    }
    if (req.method === 'POST') {
      try {
        const body = await readJsonBody(req);
        if (typeof body.oldWorker === 'boolean') state.oldWorker = body.oldWorker;
        if (typeof body.uiVersion === 'string') state.uiVersion = body.uiVersion;
        if (typeof body.delayClassicMs === 'number' && Number.isFinite(body.delayClassicMs) && body.delayClassicMs >= 0) {
          state.delayClassicMs = body.delayClassicMs;
        }
        if (body.failPath === null || typeof body.failPath === 'string') state.failPath = body.failPath;
        res.writeHead(200, { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store' });
        res.end(JSON.stringify({ ...state, builds }));
      } catch {
        res.writeHead(400, { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store' });
        res.end(JSON.stringify({ error: 'Invalid control payload.' }));
      }
      return;
    }
  }

  // One address that fails on demand (a new worker's install must then fail and leave the installed copy in charge).
  if (state.failPath && pathname === state.failPath) {
    res.writeHead(503, { 'content-type': 'text/plain; charset=utf-8', 'cache-control': 'no-store' });
    res.end('Fixture failure.');
    return;
  }

  // The frozen previous release's worker while the test replays it.
  if (state.oldWorker && req.method === 'GET' && pathname === '/sw.js') {
    const source = fs.readFileSync(path.join(OLD, 'sw.js'), 'utf8').replace('__AAC_BUILD_ID__', OLD_STAMP);
    res.writeHead(200, { 'content-type': 'text/javascript; charset=utf-8', 'cache-control': 'no-cache' });
    res.end(source);
    return;
  }
  if (state.oldWorker && req.method === 'GET' && pathname === '/sw-route.js') {
    const source = fs.readFileSync(path.join(OLD, 'sw-route.js'));
    res.writeHead(200, { 'content-type': 'text/javascript; charset=utf-8', 'cache-control': 'no-cache' });
    res.end(source);
    return;
  }

  // Real auth, real APIs and /ws always go to the primary native Rust dashboard server.
  if (isApiPath(pathname)) {
    forwardHttp(req, res, mainPort);
    return;
  }

  // A slow start: classic page routes (not /app/<tab>, not files) answer late, as a busy server does after an update.
  const classicRoute = !pathname.startsWith('/app') && !path.posix.basename(pathname).includes('.');
  const forwardUi = () => {
    const targetPort = backends[state.uiVersion];
    if (!targetPort) {
      res.writeHead(500, { 'content-type': 'text/plain; charset=utf-8' });
      res.end(`Unknown or unconfigured UI version: ${state.uiVersion}`);
      return;
    }
    forwardHttp(req, res, targetPort, { stripCookie: targetPort !== mainPort });
  };

  if (state.delayClassicMs > 0 && classicRoute) {
    const timer = setTimeout(forwardUi, state.delayClassicMs);
    req.on('close', () => clearTimeout(timer));
  } else {
    forwardUi();
  }
});

// Forward WebSocket upgrades (/ws) straight to the primary native Rust dashboard server.
server.on('upgrade', (req, socket, head) => {
  const upstream = net.connect(mainPort, '127.0.0.1', () => {
    let lines = `${req.method} ${req.url} HTTP/1.1\r\n`;
    for (let i = 0; i < req.rawHeaders.length; i += 2) {
      lines += `${req.rawHeaders[i]}: ${req.rawHeaders[i + 1]}\r\n`;
    }
    lines += '\r\n';
    upstream.write(lines);
    if (head?.length) upstream.write(head);
    upstream.pipe(socket).pipe(upstream);
  });
  upstream.on('error', () => socket.destroy());
  socket.on('error', () => upstream.destroy());
});

function shutdown() {
  if (shuttingDown) return;
  shuttingDown = true;
  server.close();
  stopChildren();
  const wait = Promise.all(
    children.map(({ child }) =>
      child.exitCode !== null ? Promise.resolve() : new Promise((resolve) => child.once('exit', resolve))
    )
  );
  const timer = setTimeout(() => {
    for (const { child } of children) {
      if (child.exitCode === null) {
        try {
          child.kill('SIGKILL');
        } catch {
          // already gone
        }
      }
    }
    process.exit(0);
  }, 2000);
  timer.unref();
  wait.then(() => {
    clearTimeout(timer);
    process.exit(0);
  });
}

process.on('exit', stopChildren);
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, shutdown);

server.listen(port, '127.0.0.1', () => {
  console.log(
    `native upgrade server on http://127.0.0.1:${port}/ (current=${mainPort}, older=${backends.older})`
  );
});
