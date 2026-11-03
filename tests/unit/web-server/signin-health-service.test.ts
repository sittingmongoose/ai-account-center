import { describe, expect, it } from 'bun:test';
import {
  allowProfile,
  hostCollectors,
  parseRows,
} from '../../../src/web-server/services/signin-health-service';

/** Pure helpers of the sign-in health service: the publish allowlist, the
 * per-host row parsing, and the host/collector pairing. The ssh and local
 * runners themselves are thin execFile wrappers exercised in production. */

const row = (overrides: Record<string, unknown> = {}) => ({
  name: 'me',
  provider: 'claude',
  configDir: '/home/person/.claude',
  accessExpiresAt: '2026-11-04T09:43:00Z',
  refreshExpiresAt: '2026-11-30T23:04:00Z',
  loggedOutAt: null,
  state: 'ok',
  fixCommand: "CLAUDE_CONFIG_DIR='/home/person/.claude' claude auth login",
  ...overrides,
});

describe('allowProfile', () => {
  it('projects a valid row with the machine on platform and machine', () => {
    const out = allowProfile(row(), 'mac');
    expect(out).not.toBeNull();
    expect(out?.machine).toBe('mac');
    expect(out?.platform).toBe('mac');
    expect(out?.state).toBe('ok');
    expect(out?.configDir).toBe('/home/person/.claude');
  });

  it('drops rows with an unknown name or state; other providers narrow to claude', () => {
    expect(allowProfile(row({ name: '' }), 'mac')).toBeNull();
    expect(allowProfile(row({ name: 'a'.repeat(65) }), 'mac')).toBeNull();
    expect(allowProfile(row({ name: '../escape' }), 'mac')).toBeNull();
    expect(allowProfile(row({ state: 'healthy' }), 'mac')).toBeNull();
    expect(allowProfile(row({ provider: 'mystery' }), 'mac')?.provider).toBe('claude');
    expect(allowProfile(row({ provider: 'codex' }), 'mac')?.provider).toBe('codex');
  });

  it('drops rows whose bounded fields are the wrong type or too long', () => {
    expect(allowProfile(row({ accessExpiresAt: 12345 }), 'mac')).toBeNull();
    expect(allowProfile(row({ accessExpiresAt: 'x'.repeat(41) }), 'mac')).toBeNull();
    expect(allowProfile(row({ fixCommand: 'x'.repeat(201) }), 'mac')).toBeNull();
    expect(allowProfile(row({ configDir: 'x'.repeat(201) }), 'mac')).toBeNull();
  });

  it('never passes helper-added fields through to the published DTO', () => {
    const out = allowProfile(row({ accessToken: 'sk-ant-secret', extra: 'field' }), 'ubuntu');
    expect(out).not.toBeNull();
    expect(JSON.stringify(out)).not.toContain('sk-ant-secret');
    expect(JSON.stringify(out)).not.toContain('extra');
  });

  it('carries a bounded error only on unavailable rows', () => {
    const failed = allowProfile(
      row({ state: 'unavailable', error: 'x'.repeat(300) }),
      'ubuntu'
    );
    expect(failed?.error).toBe('x'.repeat(120));
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
    expect(parseRows(envelope([row({ name: 'bad name!' })]), 'mac')).toEqual([]);
  });
});

describe('hostCollectors', () => {
  it('pairs every requested host with its own collector, ubuntu always first', () => {
    expect(hostCollectors([]).map((pair) => pair.host)).toEqual(['ubuntu']);
    expect(hostCollectors(['mac', 'windows']).map((pair) => pair.host)).toEqual([
      'ubuntu',
      'mac',
      'windows',
    ]);
    expect(hostCollectors(['windows']).map((pair) => pair.host)).toEqual(['ubuntu', 'windows']);
  });

  it('ignores host names that have no collector', () => {
    expect(hostCollectors(['nas1', 'ubuntu']).map((pair) => pair.host)).toEqual(['ubuntu']);
  });
});
