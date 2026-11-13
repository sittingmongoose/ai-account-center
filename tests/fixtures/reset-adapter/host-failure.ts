/** An isolated module graph: transport mocks never reach another test file. */
import { mock } from 'bun:test';
import express from 'express';
import { mkdirSync, writeFileSync } from 'fs';
import { join } from 'path';

const mode = process.argv[2];
if (mode !== 'lost-reply' && mode !== 'entry-throw') throw new Error('Invalid fixture mode');
const root = process.env.CCS_HOME;
if (!root) throw new Error('Fixture requires isolated CCS_HOME');
const ccsDir = join(root, '.ccs');
mkdirSync(ccsDir, { recursive: true });
writeFileSync(
  join(ccsDir, 'claude-desktop-profiles.json'),
  JSON.stringify({
    version: 1,
    profiles: [
      {
        id: 'adapter-fixture',
        email: 'fixture@example.test',
        windows: {
          launcherName: 'Fixture',
          sshHost: 'fixture-windows',
        },
      },
    ],
  })
);
const transportPath = import.meta
  .resolve('../../../src/web-server/services/usage-failover-transport.ts');
const transport = { ...(await import(transportPath)) };
let entered = 0;
mock.module(transportPath, () => ({
  ...transport,
  runPrivateUsageHelper: () => {
    // Entry itself is the last moment at which the adapter can mark dispatch.
    entered++;
    if (mode === 'entry-throw') throw new Error('synthetic failure at helper entry');
    return Promise.resolve().then(() => {
      throw new Error('synthetic lost helper reply');
    });
  },
}));
const { createResetSourceAdapter } =
  await import('../../../src/web-server/services/reset-source-adapter');
const { createResetClaimRouter } =
  await import('../../../src/web-server/routes/reset-claim-routes');
const write = {
  routeAccount: 'claude:adapter-fixture',
  method: 'claude.reset_rate_limits',
  body: {},
  identity: { provider: 'claude' },
  attemptKey: 'a'.repeat(64),
  sourceBinding: 'fixture',
};
const adapter = createResetSourceAdapter();
let rejected = false;
let directOutcome: string | null = null;
try {
  directOutcome = (await adapter.send(write)).outcome;
} catch {
  rejected = true;
}

const app = express();
app.use(express.json());
app.use((req, _res, next) => {
  (req as any).session = { authenticated: true };
  (req as any).sessionID = 'fixture-session';
  next();
});
let reads = 0;
let finishOutcome: string | null = null;
app.use(
  '/accounts',
  createResetClaimRouter({
    ccsDir: root,
    source: {
      read: async () => {
        reads++;
        return {};
      },
      send: (request) => adapter.send(request),
    },
    native: {
      async call(_method, input) {
        const request = JSON.parse(input);
        if (request.op === 'begin') return JSON.stringify(write);
        if (request.op === 'finish') {
          finishOutcome = request.outcome;
          // This spy exposes the native finish contract. Real Store restart/U1
          // behavior for Ambiguous is checked by the native and Leptos suites.
          const pending = request.outcome === 'ambiguous';
          return JSON.stringify({
            outcome: pending ? 'claimed-unconfirmed' : request.outcome,
            pending,
          });
        }
        return '{}';
      },
    },
  })
);
const server = app.listen(0, '127.0.0.1');
await new Promise<void>((resolve) => server.on('listening', resolve));
try {
  const origin = `http://127.0.0.1:${(server.address() as { port: number }).port}`;
  const response = await fetch(`${origin}/accounts/claude%3Aadapter-fixture/reset-claims`, {
    method: 'POST',
    headers: { Origin: origin, 'Content-Type': 'application/json' },
    body: JSON.stringify({ grantKey: 'b'.repeat(64), confirmationToken: 'c'.repeat(64) }),
  });
  console.log(
    JSON.stringify({
      entered,
      rejected,
      directOutcome,
      reads,
      finishOutcome,
      status: response.status,
      receipt: await response.json(),
    })
  );
} finally {
  await new Promise<void>((resolve) => server.close(() => resolve()));
}
