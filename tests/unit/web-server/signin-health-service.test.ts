import { describe, expect, it } from 'bun:test';
import {
  allowProfile,
  cacheTtlFor,
  nextDeadlineAt,
  parseRows,
  sanitizeName,
  type SignInHealth,
} from '../../../src/web-server/services/signin-health-service';

/** Pure helpers of the sign-in health service: the publish allowlist, the
 * per-host row parsing, and the deadline-aware cache lifetime. The runners
 * themselves are thin execFile wrappers exercised in production; the remote
 * machines run the same collector through the usage helper's stdin bundle. */

const row = (overrides: Record<string, unknown> = {}) => ({
  name: 'me',
  provider: 'claude',
  configDir: '~/.claude',
  accessExpiresAt: '2026-11-04T09:43:00Z',
  refreshExpiresAt: '2026-11-30T23:04:00Z',
  loggedOutAt: null,
  state: 'ok',
  fixCommand: 'CLAUDE_CONFIG_DIR="$HOME/.claude" claude auth login',
  ...overrides,
});

const health = (rows: Array<Record<string, unknown>>): SignInHealth => ({
  generatedAt: '2026-11-03T12:00:00Z',
  machines: { ubuntu: rows as never },
});

describe('allowProfile', () => {
  it('projects a valid row with the machine on platform and machine', () => {
    const out = allowProfile(row(), 'mac');
    expect(out?.machine).toBe('mac');
    expect(out?.platform).toBe('mac');
    expect(out?.state).toBe('ok');
    expect(out?.configDir).toBe('~/.claude');
    expect(out?.provider).toBe('claude');
    expect(allowProfile(row({ provider: 'codex' }), 'mac')?.provider).toBe('codex');
    expect(allowProfile(row({ provider: { junk: true } }), 'mac')?.provider).toBe('claude');
  });

  it('keeps a row whose name the strict pattern refuses, sanitized', () => {
    const email = allowProfile(row({ name: 'person@mail.com' }), 'mac');
    expect(email?.name).toBe('person@mail.com');
    const apostrophe = allowProfile(row({ name: "it's" }), 'mac');
    expect(apostrophe).not.toBeNull();
    expect(apostrophe?.name).not.toContain("'");
    expect(apostrophe?.name.length).toBeGreaterThan(0);
    expect(allowProfile(row({ name: 'bad name!' }), 'mac')).not.toBeNull();
    expect(allowProfile(row({ name: '' }), 'mac')?.name).toBe('unnamed');
    expect(allowProfile(row({ name: 42 }), 'mac')?.name).toBe('unnamed');
    expect(sanitizeName('a'.repeat(80))).toHaveLength(64);
    expect(sanitizeName('   ')).toBe('unnamed');
  });

  it('never drops a row over a bound; bounds clip instead', () => {
    const clipped = allowProfile(
      row({ configDir: 'x'.repeat(300), fixCommand: 'y'.repeat(300) }),
      'mac'
    );
    expect(clipped).not.toBeNull();
    expect(clipped?.configDir).toHaveLength(200);
    expect(clipped?.fixCommand).toHaveLength(200);
  });

  it('reads malformed stamps as absent, never as a reason to drop the row', () => {
    const out = allowProfile(
      row({ accessExpiresAt: 'not-a-time', refreshExpiresAt: 'x'.repeat(41) }),
      'mac'
    );
    expect(out).not.toBeNull();
    expect(out?.accessExpiresAt).toBeNull();
    expect(out?.refreshExpiresAt).toBeNull();
    expect(allowProfile(row({ accessExpiresAt: 12345 }), 'mac')?.accessExpiresAt).toBeNull();
  });

  it('never passes helper-added fields through to the published DTO', () => {
    const out = allowProfile(row({ accessToken: 'sk-ant-secret', extra: 'field' }), 'ubuntu');
    expect(out).not.toBeNull();
    expect(JSON.stringify(out)).not.toContain('sk-ant-secret');
    expect(JSON.stringify(out)).not.toContain('extra');
  });

  it('carries a single-line bounded error only on unavailable rows', () => {
    const failed = allowProfile(
      row({ state: 'unavailable', error: 'ssh: refused\n\n   connection timed out   ' }),
      'ubuntu'
    );
    expect(failed?.error).toBe('ssh: refused connection timed out');
    expect(failed?.error?.length).toBeLessThanOrEqual(120);
    expect(allowProfile(row({ error: 'some error' }), 'ubuntu')?.error).toBeUndefined();
    expect(
      allowProfile(row({ state: 'unavailable', error: 'connection refused' }), 'ubuntu')?.error
    ).toBe('connection refused');
  });

  it('caps fieldsAvailable on unsupported rows', () => {
    const out = allowProfile(
      row({
        provider: 'codex',
        state: 'unsupported',
        fixCommand: '',
        fieldsAvailable: [...Array(40).keys()].map((n) => `field_${n}`),
      }),
      'ubuntu'
    );
    expect(out?.fieldsAvailable).toHaveLength(32);
    const noFields = allowProfile(row({ state: 'unsupported', fieldsAvailable: 'nope' }), 'ubuntu');
    expect(noFields?.fieldsAvailable).toBeUndefined();
  });
});

describe('parseRows', () => {
  const envelope = (profiles: unknown[]) =>
    JSON.stringify({ schemaVersion: 1, generatedAt: '2026-11-03T12:00:00Z', profiles });

  it('parses the collector envelope and names the machine on every row', () => {
    const rows = parseRows(envelope([row(), row({ name: 'party' })]), 'windows');
    expect(rows).toHaveLength(2);
    expect(rows.every((r) => r.machine === 'windows' && r.platform === 'windows')).toBe(true);
  });

  it('reads banner-prefixed or unreadable output as nothing', () => {
    expect(parseRows(`banner line\n${envelope([])}`, 'mac')).toEqual([]);
    expect(parseRows('{"schemaVersion":2,"profiles":[]}', 'mac')).toEqual([]);
    expect(parseRows('not json at all', 'mac')).toEqual([]);
  });
});

describe('the cache outlives no deadline it published', () => {
  const NOW = Date.parse('2026-11-03T12:00:00Z');

  it('finds the earliest future stamp across machines', () => {
    const report = health([
      row({ refreshExpiresAt: '2026-11-03T12:04:00Z' }),
      row({ name: 'party', accessExpiresAt: '2026-11-03T12:02:00Z' }),
    ]);
    expect(nextDeadlineAt(report, NOW)).toBe(Date.parse('2026-11-03T12:02:00Z'));
    expect(nextDeadlineAt(health([]), NOW)).toBeNull();
    expect(
      nextDeadlineAt(
        health([row({ accessExpiresAt: '2026-11-03T11:00:00Z', refreshExpiresAt: null })]),
        NOW
      )
    ).toBeNull();
  });

  it('clamps the cache lifetime to the next deadline, with a floor and a ceiling', () => {
    const twoMinutes = health([row({ refreshExpiresAt: '2026-11-03T12:02:00Z' })]);
    expect(cacheTtlFor(twoMinutes, NOW)).toBe(2 * 60_000);
    // A deadline ten seconds away must not pin every publish to a miss.
    const tenSeconds = health([row({ refreshExpiresAt: '2026-11-03T12:00:10Z' })]);
    expect(cacheTtlFor(tenSeconds, NOW)).toBe(30_000);
    // Far deadlines and no deadlines use the full five minutes.
    const far = health([row({ refreshExpiresAt: '2026-11-30T23:04:00Z' })]);
    expect(cacheTtlFor(far, NOW)).toBe(5 * 60_000);
    expect(cacheTtlFor(health([]), NOW)).toBe(5 * 60_000);
  });
});
