/**
 * G5: the Rust sign-in poll and the settle/merge projection against the TS adapters, over the
 * real private socket and a fixture sign-in agent. Fixture rows only; no host, key or provider
 * is touched. Run through `python3 tests/spec/agent/transport.py <agent> <aac-http> --signin`.
 */
import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import path from 'node:path';
import type { ClaudeSignInRenewal } from '../../../src/web-server/services/claude-signin-renewal';
import {
  cacheTtlFor,
  downgradeCrossedRows,
  mergeRenewal,
  parseRows,
  type SignInHealth,
} from './signin-reference';

const fixture = JSON.parse(await Bun.stdin.text()) as {
  root: string;
  socketPath: string;
  secret: string;
  bootId: string;
};

Object.assign(process.env, {
  CCS_DIR: fixture.root,
  CCS_HOME: fixture.root,
  AAC_AGENT_VERBS: 'on',
  AAC_AGENT_TRANSPORT: 'on',
  AAC_NO_LOCAL_RUST_BUILD: '1',
});

// The modules read CCS_DIR and the agent gates when they load, so the fixture environment is
// assigned above before these imports; a static import would evaluate them first.
const { registerRustAgentConnection, pollRustSignIns, projectRustSignIns } =
  await import('../../../src/web-server/rust-service-transport');

const close = registerRustAgentConnection(
  fixture.root,
  { socketPath: fixture.socketPath, secret: fixture.secret, bootId: fixture.bootId },
  new Set()
);
const at = Date.parse('2026-01-01T00:00:00Z');

try {
  // The sanitizer boundary: a long clipped configDir, a dropped rest, a field allowlist,
  // duplicate names, and the generated suffix collision.
  const raw = {
    schemaVersion: 1,
    profiles: [
      {
        name: 'Alice',
        configDir: 'x'.repeat(199) + '😀',
        provider: 'codex',
        state: 'unsupported',
        fieldsAvailable: ['tokens', 'fixture-field'],
        fixCommand: 'line\nbreak',
        accessExpiresAt: '2026-02-30T00:00:00.000Z',
      },
      {
        name: 'Alice',
        state: 'unavailable',
        error: 'fixture private message',
        fixCommand: 'fixture command',
        refreshExpiresAt: '2026-01-01T05:30:00.000+05:30',
      },
      { name: 'Alice-2', state: 'ok' },
      { name: 'Alice ', state: 'ok' },
      { name: '😀', state: 'ok' },
    ],
  };
  writeFileSync(path.join(fixture.root, 'signin-fixture.json'), JSON.stringify(raw));
  writeFileSync(path.join(fixture.root, 'scenario'), 'signin-rows');
  const scheduled = await pollRustSignIns(fixture.root, [
    { host: 'ubuntu', platform: 'ubuntu', sshHost: 'fixture' },
    { host: 'mac', platform: 'mac', sshHost: 'unconfigured' },
  ]);
  assert.deepEqual(scheduled, {
    machines: { ubuntu: JSON.parse(JSON.stringify(parseRows(JSON.stringify(raw), 'ubuntu'))) },
    referenceHosts: ['mac'],
  });

  const settledAt = at + 5000;
  const machines: SignInHealth['machines'] = {
    mac: [
      {
        name: 'fixture',
        machine: 'mac',
        platform: 'mac',
        provider: 'claude',
        state: 'ok',
        fixCommand: '',
        accessExpiresAt: new Date(at + 1000).toISOString(),
      },
    ],
  };
  const expectedHealth: SignInHealth = {
    generatedAt: new Date(settledAt).toISOString(),
    machines: {
      ...structuredClone(machines),
      ubuntu: [
        {
          name: 'ubuntu',
          provider: 'claude',
          platform: 'ubuntu',
          machine: 'ubuntu',
          state: 'unavailable',
          fixCommand: '',
          error: 'no collection route from this host',
        },
      ],
    },
  };
  const downgraded = downgradeCrossedRows(expectedHealth, at, settledAt);
  assert.equal(downgraded, true);
  const health = await projectRustSignIns(fixture.root, {
    machines,
    collectedAt: at,
    now: settledAt,
  });
  assert.deepEqual(health, {
    machines: expectedHealth.machines,
    ttlMs: downgraded ? 30_000 : cacheTtlFor(expectedHealth, settledAt),
  });

  const renewal = {
    stale: false,
    hosts: [
      { host: 'mac', error: null },
      { host: 'windows', error: 'unavailable' },
    ],
    signIns: [
      {
        profile: 'fixture',
        host: 'mac',
        state: 'ok',
        logoutAt: null,
        signInEndsAt: null,
        email: 'fixture@example.test',
        autoRenew: true,
      },
      {
        profile: 'fixture',
        host: 'windows',
        state: 'logged_out',
        logoutAt: null,
        signInEndsAt: null,
        autoRenew: false,
      },
    ],
  } as ClaudeSignInRenewal;
  const both = {
    ...machines,
    windows: [{ ...machines.mac[0], platform: 'windows', machine: 'windows' }],
  };
  const merged = await projectRustSignIns(fixture.root, {
    machines: both,
    renewal,
    now: settledAt,
  });
  assert.deepEqual(merged, {
    machines: JSON.parse(JSON.stringify(mergeRenewal(both, renewal, settledAt))),
  });
  console.log('PASS G5 sign-in poll rows, crossed expiry, TTL and Claude renewal merge');
} finally {
  close();
}
