#!/usr/bin/env node
/**
 * R1 exit gate, end to end (DESIGN-M3-COMPLETE 1.1): GET /api/views/home through the real front door modules
 * (built dist/) and the real Rust service, today's handler (D7: Home offered, the Home group off) against the fed
 * path (the Home group on), on the same reads. Each Home spec case (tests/spec/views/home/cases.json) gives the
 * dashboard and the page's context; the profiles come from a fixture desktop manifest that both paths read through
 * the service, the Antigravity copy from the case's inventory. For every case:
 *   - both answer the same status;
 *   - the bytes are equal, or, where only the fed path built a view (it reads no server word, so an envelope
 *     `zone`/`fact` decline may build there), equal up to the view (a built view's equality to the JS view is the
 *     spec's gate, crates/aac-views/tests/home_spec.rs);
 *   - a fed decline is answered by today's handler (so its bytes are today's).
 * Each case the fed path answers is asked again: built from the generation's memo, it must be the same bytes (R1b).
 * Then the lifecycle on the real binary: the ETag revalidates (304), a push during a request answers through
 * today's handler and the next request is served, and a service killed mid-feed is pushed again after its restart.
 *
 *   node tests/spec/home-read-parity.mjs [--dist <dist>] [--binary <aac-http>]
 * Fixture data only, in a temporary CCS home; the processes it starts are its own.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import { createRequire } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../..');
const args = process.argv.slice(2);
const option = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const dist = path.resolve(option('dist', path.join(repo, 'dist')));
const binary = path.resolve(option('binary', path.join(dist, 'native/aac-http')));

const scratch = fs.mkdtempSync(path.join(process.env.TMPDIR ?? os.tmpdir(), 'aac-r1-parity-'));
const home = fs.realpathSync(scratch);
// Two CCS roots with the same manifest: each service owns its own process (one root has one service). The fed
// service's is `getCcsDir()` (the TS readers' and a restart's root); today's service reads the other, so today's
// handler reads the profiles with the TS reader there (part 4 proves the two reads equal).
const ccs = path.join(home, 'fed', '.ccs');
const todayCcs = path.join(home, 'today', '.ccs');
for (const dir of [ccs, todayCcs]) fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_') || key.startsWith('AAC_RUST')) delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: path.dirname(ccs),
  CCS_DIR: ccs,
  CCS_DASHBOARD_AUTH_ENABLED: 'true',
  AAC_RUST_HOME_VIEW: 'on',
  AAC_NO_LOCAL_RUST_BUILD: '1',
  NO_COLOR: '1',
});
const MANIFEST = {
  version: 1,
  profiles: ['fixture-a', 'fixture-b', 'fixture-c'].map((id, i) => ({
    id,
    email: `user${i}@example.invalid`,
    mac: {
      launcherName: `Claude ${i}`,
      launcherPath: `/Applications/Claude ${i}.app`,
      profilePath: `/Users/fixture/Library/Application Support/Claude-${i}`,
      sshHost: 'mac',
    },
  })),
};
for (const dir of [ccs, todayCcs])
  fs.writeFileSync(path.join(dir, 'claude-desktop-profiles.json'), JSON.stringify(MANIFEST), { mode: 0o600 });

if (args.includes('--ts-only')) {
  // Off: the Home group is off by default, and with the service off nothing is fed.
  const req = createRequire(import.meta.url);
  const { rustGroupOn } = req(path.join(dist, 'web-server/rust-service-groups.js'));
  const { RustService } = req(path.join(dist, 'web-server/rust-service.js'));
  const { homeFedService } = req(path.join(dist, 'web-server/views/offered-views.js'));
  // The fixture home has no config.yaml: absent is off.
  assert.equal(rustGroupOn('home', {}), false, 'the Home group is off by default');
  assert.equal(homeFedService({ locals: { rustService: new RustService('off') } }), null);
  console.log('home-read-parity: off mode feeds nothing (the Home group is off by default)');
  fs.rmSync(scratch, { recursive: true, force: true });
  process.exit(0);
}
assert.ok(fs.existsSync(binary), `needs the Nas1-built aac-http at ${binary}`);

const require = createRequire(import.meta.url);
const load = (file) => require(path.join(dist, file));
const express = require(path.join(repo, 'node_modules/express'));
const { RustService } = load('web-server/rust-service.js');
const { createHomeViewRouter } = load('web-server/routes/home-view-routes.js');
const { withPassedResetsAt } = load('web-server/services/account-dashboard-service.js');
const { homeFedService } = load('web-server/views/offered-views.js');
const { getClaudeOpenOperations } = load('web-server/services/claude-open-operations.js');

const { cases } = JSON.parse(fs.readFileSync(path.join(here, 'views/home/cases.json'), 'utf8'));
const services = [];
const servers = [];

async function service(group, root) {
  process.env.AAC_RUST_GROUP_HOME = group;
  process.env.CCS_DIR = root;
  const s = new RustService('on');
  await s.start(binary);
  process.env.CCS_DIR = ccs;
  assert.equal(s.lease?.connection.ccsDir, root);
  assert.ok(s.lease, 'the Rust service runs');
  // Each service reads its group once, now.
  assert.equal(homeFedService({ locals: { rustService: s } }) === s, group === 'on');
  services.push(s);
  return s;
}

/** The current case's reads, the same for both paths. */
let current = null;
function app(rust, root) {
  const instance = express();
  instance.use((req, res, next) => {
    Object.assign(req, { session: { authenticated: true }, sessionID: 'fixture-parity-session' });
    res.locals.rustService = rust;
    next();
  });
  instance.use(
    '/api/views',
    createHomeViewRouter({
      readHomeDashboard: async () => ({ dashboard: structuredClone(current.dashboard), now: current.ctx.now }),
      getDashboard: async () => withPassedResetsAt(structuredClone(current.dashboard), current.ctx.now),
      runtimeInventory: async () => structuredClone(current.inventory),
      readInventory: (req, res, read) =>
        load('web-server/routes/antigravity-routes.js').readAntigravityInventory(
          req,
          res,
          read ?? { value: structuredClone(current.inventory) }
        ),
      scope: () => root,
    })
  );
  return new Promise((resolve) => {
    const server = instance.listen(0, '127.0.0.1', () => resolve(`http://127.0.0.1:${server.address().port}`));
    servers.push(server);
  });
}

function get(base, ctx, headers = {}) {
  const platform = ctx?.platform === 'windows' ? 'windows' : 'mac';
  return new Promise((resolve, reject) => {
    const request = http.get(
      `${base}/api/views/home?platform=${platform}`,
      { headers: { ...(ctx ? { 'x-aac-home-context': encodeURIComponent(JSON.stringify(ctx)) } : {}), ...headers } },
      (response) => {
        const chunks = [];
        response.on('data', (c) => chunks.push(c));
        response.on('end', () =>
          resolve({ status: response.statusCode, headers: response.headers, body: Buffer.concat(chunks).toString('utf8') })
        );
      }
    );
    request.on('error', reject);
  });
}

/**
 * Whether the fed path answered: its ETag is the revalidated-view SHA-256 the service wrote. Today's handler
 * answers through `res.json` (Express's own weak ETag) and, on the fed server too, may itself be served by D7's
 * Rust compose, so the route's served counter cannot tell the two apart.
 */
const fedAnswered = (reply) => /^W\/"[a-f0-9]{64}"$/.test(String(reply.headers.etag));
const withoutView = (body) => body.slice(0, body.lastIndexOf(',"view":'));

/** The runtime inventory a case's public copy came from (its rows are already public), or the empty Ubuntu one. */
function inventoryOf(antigravity) {
  return antigravity && typeof antigravity === 'object' && antigravity.hostId === 'ubuntu'
    ? antigravity
    : { schemaVersion: 1, hostId: 'ubuntu', profiles: [] };
}

const report = { cases: 0, equal: 0, fedViewOnly: 0, fedDeclined: 0, statusOnly: 0, failures: [] };
try {
  const today = await service('off', todayCcs);
  const fed = await service('on', ccs);
  const todayUrl = await app(today, todayCcs);
  const fedUrl = await app(fed, ccs);
  for (const c of cases) {
    report.cases++;
    current = { dashboard: c.dashboard, ctx: c.ctx, inventory: inventoryOf(c.antigravity) };
    const a = await get(todayUrl, c.ctx);
    const b = await get(fedUrl, c.ctx);
    const rust = fedAnswered(b);
    // R1b: the same request again is built from the generation's memo (its clock-independent parts kept by the
    // first): the same bytes and tag.
    if (rust) {
      const again = await get(fedUrl, c.ctx);
      if (again.body !== b.body || again.headers.etag !== b.headers.etag)
        report.failures.push(`${c.name}: the memo's second answer differs from the first`);
      else report.memoAgain = (report.memoAgain ?? 0) + 1;
    }
    if (a.status !== b.status) {
      report.failures.push(`${c.name}: status ${a.status} today, ${b.status} fed`);
      continue;
    }
    if (a.status !== 200) {
      if (a.body === b.body) report.statusOnly++;
      else report.failures.push(`${c.name}: ${a.status} bodies differ`);
      continue;
    }
    if (!rust) {
      // Declined: today's handler answered on the fed server too.
      report.fedDeclined++;
      if (a.body !== b.body) report.failures.push(`${c.name}: a fed decline answered other bytes`);
      continue;
    }
    if (a.body === b.body) {
      report.equal++;
      continue;
    }
    const va = JSON.parse(a.body).view;
    const vb = JSON.parse(b.body).view;
    if (withoutView(a.body) === withoutView(b.body) && va === null && vb !== null) {
      report.fedViewOnly++;
      continue;
    }
    report.failures.push(`${c.name}: served bytes differ from today's`);
  }
  assert.ok(report.equal + report.fedViewOnly >= cases.length / 2, `most cases served: ${JSON.stringify(report)}`);

  // ---- lifecycle on the real binary ----------------------------------------------------------------
  const rich = cases.find((c) => c.name === 'home-page#1') ?? cases[0];
  current = { dashboard: rich.dashboard, ctx: rich.ctx, inventory: inventoryOf(rich.antigravity) };
  const first = await get(fedUrl, rich.ctx);
  assert.equal(first.status, 200);
  assert.ok(fedAnswered(first), 'the fed path answers with its ETag');
  assert.equal(first.headers['cache-control'], 'no-store');
  const again = await get(fedUrl, rich.ctx, { 'if-none-match': first.headers.etag });
  assert.equal(again.status, 304, 'a matching ETag revalidates');
  assert.equal(again.body, '');
  report.etag = 'W/"<sha256>", 304 on If-None-Match';

  // A push during a request: another front door's push lands first (a generation far ahead of this one's), so
  // this request names one the service no longer holds: `stale`, and today's handler answers. The front door then
  // moves past the held number: one more request is today's (its push loses once), then the fed path serves.
  const routes = () => fed.diagnostics().routes['/api/views/home'];
  const lease = fed.lease;
  const { pushRustFeedText } = load('web-server/rust-service-transport.js');
  const source = `home-dashboard-${rich.ctx.platform === 'windows' ? 'windows' : 'mac'}-1`;
  const facts = JSON.stringify({ order: {}, epochs: {} });
  const ahead = '9000000000000000';
  const pushed = await pushRustFeedText(lease.connection, source, ahead, JSON.stringify({ ...rich.dashboard, updatedAt: '' }), facts, new Set());
  assert.equal(pushed.body.generation, ahead);
  const reference = await get(todayUrl, rich.ctx);
  const raced = await get(fedUrl, rich.ctx);
  assert.equal(raced.body, reference.body, "the raced request is answered by today's handler");
  assert.ok((routes().declined.stale ?? 0) >= 1, 'counted as stale');
  assert.ok(!fedAnswered(raced));
  let servedAgain = 0;
  for (let i = 0; i < 3 && !servedAgain; i++) {
    const reply = await get(fedUrl, rich.ctx);
    assert.equal(withoutView(reply.body), withoutView(reference.body));
    if (fedAnswered(reply)) servedAgain = i + 1;
  }
  assert.ok(servedAgain, 'served again after the race');
  report.stale = { declined: routes().declined.stale, servedAgainAfter: servedAgain };

  // A service killed mid-feed: the supervisor restarts it, the new boot holds nothing, the front door pushes again.
  const oldBoot = lease.connection.bootId;
  process.kill(fed.lease.pid ?? 0, 'SIGKILL');
  const deadline = Date.now() + 15_000;
  while ((!fed.lease || fed.lease.connection.bootId === oldBoot) && Date.now() < deadline) {
    await get(fedUrl, rich.ctx);
    await new Promise((r) => setTimeout(r, 200));
  }
  assert.ok(fed.lease && fed.lease.connection.bootId !== oldBoot, 'restarted with a new boot');
  let after = await get(fedUrl, rich.ctx);
  for (let i = 0; i < 3 && !fedAnswered(after); i++) after = await get(fedUrl, rich.ctx);
  assert.equal(after.status, 200);
  assert.ok(fedAnswered(after), "the new boot is pushed again and serves");
  assert.equal(withoutView(after.body), withoutView((await get(todayUrl, rich.ctx)).body));
  report.restart = 'new boot pushed again and served';

  // An Open in progress: both paths draw no server view (the progress line is the page's).
  const operations = getClaudeOpenOperations();
  let finish = () => {};
  for (const root of [ccs, todayCcs])
    operations.start(root, 'fixture-a', 'mac', () => new Promise((resolve) => (finish = resolve)));
  const openA = await get(todayUrl, rich.ctx);
  const openB = await get(fedUrl, rich.ctx);
  assert.equal(openA.body, openB.body, 'an Open in progress: the same answer');
  assert.ok(!fedAnswered(openB), "the fed view declines (open-progress) and today's handler answers");
  finish();
  report.openInProgress = 'equal';

  console.log(JSON.stringify(report, null, 1));
  assert.deepEqual(report.failures, [], 'no served byte differs from today');
  console.log(
    `[OK] home-read-parity: ${report.cases} cases, ${report.equal} byte-equal (${report.memoAgain ?? 0} asked again from the memo), ${report.fedViewOnly} equal but for a view only the fed path built, ${report.fedDeclined} declined to today's handler, ${report.statusOnly} equal refusals`
  );
} finally {
  for (const server of servers) server.close();
  for (const s of services) s.stop();
  await new Promise((r) => setTimeout(r, 300));
  fs.rmSync(scratch, { recursive: true, force: true });
}
