import { describe, expect, it } from 'bun:test';
import * as os from 'os';
import * as path from 'path';
import {
  museSessionIdForFile,
  normalizeMuseRecordedAt,
  parseMuseUsageLine,
  resolveMuseSessionsDir,
} from '../../../../src/web-server/usage/muse-native-usage-collector';

function completed(overrides: Record<string, unknown> = {}) {
  return {
    schema_version: 1,
    id: 'r-1',
    recorded_at: Date.parse('2026-10-01T15:10:00Z'),
    payload: {
      event: {
        kind: 'model_completed',
        model: 'muse-spark-1.3-contributor',
        usage: {
          input_tokens: 26179,
          output_tokens: 512,
          cached_tokens: 26097,
          cache_read_tokens: 26097,
          cache_write_tokens: 12,
          reasoning_tokens: 100,
        },
      },
    },
    ...overrides,
  };
}

describe('muse usage lines', () => {
  it('maps model_completed records, counting cache reads once', () => {
    const entry = parseMuseUsageLine(JSON.stringify(completed()), 'session-uuid');
    expect(entry).toMatchObject({
      // input_tokens includes the cache reads: 26179 - 26097 uncached.
      inputTokens: 82,
      outputTokens: 512,
      cacheReadTokens: 26097,
      cacheCreationTokens: 12,
      model: 'muse-spark-1.3-contributor',
      sessionId: 'session-uuid',
      target: 'muse',
      // Muse names no routing provider; the tool is never priced as one.
      provider: '',
    });
    expect(entry?.timestamp).toBe(new Date('2026-10-01T15:10:00Z').toISOString());
  });

  it('unpacks one record_json level', () => {
    const inner = JSON.stringify(completed());
    const line = JSON.stringify({
      recorded_at: Date.parse('2026-10-01T15:10:00Z'),
      children: [{ record_json: inner }],
    });
    const entry = parseMuseUsageLine(line, 's');
    expect(entry?.model).toBe('muse-spark-1.3-contributor');
    const dictLine = JSON.stringify({
      recorded_at: Date.parse('2026-10-01T15:10:00Z'),
      children: [{ record_json: completed() }],
    });
    expect(parseMuseUsageLine(dictLine, 's')?.model).toBe('muse-spark-1.3-contributor');
  });

  it('never counts goal_usage_attribution or other kinds', () => {
    const attributed = completed();
    (attributed.payload.event as Record<string, unknown>).kind = 'goal_usage_attribution';
    expect(parseMuseUsageLine(JSON.stringify(attributed), 's')).toBeNull();
    expect(parseMuseUsageLine(JSON.stringify({ recorded_at: 1 }), 's')).toBeNull();
    expect(parseMuseUsageLine('a conversation fragment without json', 's')).toBeNull();
  });

  it('normalizes recorded_at across ms, us and ns units', () => {
    expect(normalizeMuseRecordedAt(Date.parse('2026-10-01T15:10:00Z'))).toBe(
      Date.parse('2026-10-01T15:10:00Z')
    );
    expect(normalizeMuseRecordedAt(Date.parse('2026-10-01T15:10:00Z') * 1000)).toBe(
      Date.parse('2026-10-01T15:10:00Z')
    );
    expect(normalizeMuseRecordedAt(Date.parse('2026-10-01T15:10:00Z') * 1000000)).toBe(
      Date.parse('2026-10-01T15:10:00Z')
    );
    expect(normalizeMuseRecordedAt(Date.parse('2026-10-01T15:10:00Z') / 1000)).toBe(
      Date.parse('2026-10-01T15:10:00Z')
    );
    expect(normalizeMuseRecordedAt(-5)).toBeNull();
    expect(normalizeMuseRecordedAt('nope')).toBeNull();
    const micro = completed({ recorded_at: Date.parse('2026-10-01T15:10:00Z') * 1000 });
    expect(parseMuseUsageLine(JSON.stringify(micro), 's')?.timestamp).toBe(
      new Date('2026-10-01T15:10:00Z').toISOString()
    );
  });

  it('derives session ids and roots without reading content', () => {
    expect(
      museSessionIdForFile('/h/.local/share/muse/sessions/2026/10/01/uuid/session.jsonl')
    ).toBe('uuid');
    expect(resolveMuseSessionsDir({ homeDir: '/h', env: {} })).toBe(
      path.join('/h', '.local', 'share', 'muse', 'sessions')
    );
    expect(resolveMuseSessionsDir({ homeDir: '/h', env: { MUSE_SESSIONS_DIR: '/custom' } })).toBe(
      '/custom'
    );
    expect(os.homedir().length).toBeGreaterThan(0);
  });
});
