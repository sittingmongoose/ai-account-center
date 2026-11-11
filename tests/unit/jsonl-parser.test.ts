/**
 * Unit tests for the JSONL line parser
 */

import { describe, expect, test } from 'bun:test';
import { parseUsageEntry } from '../../src/web-server/jsonl-parser';

// ============================================================================
// TEST FIXTURES
// ============================================================================

const VALID_ASSISTANT_ENTRY = JSON.stringify({
  type: 'assistant',
  sessionId: 'test-session-123',
  timestamp: '2025-12-09T10:00:00.000Z',
  version: '2.0.60',
  cwd: '/home/user/project',
  message: {
    model: 'claude-sonnet-4-5',
    usage: {
      input_tokens: 1000,
      output_tokens: 500,
      cache_creation_input_tokens: 200,
      cache_read_input_tokens: 100,
    },
  },
});

const ASSISTANT_ENTRY_NO_CACHE = JSON.stringify({
  type: 'assistant',
  sessionId: 'test-session-456',
  timestamp: '2025-12-09T11:00:00.000Z',
  message: {
    model: 'gemini-2.5-pro',
    usage: {
      input_tokens: 2000,
      output_tokens: 1000,
    },
  },
});

const USER_ENTRY = JSON.stringify({
  type: 'user',
  sessionId: 'test-session-123',
  timestamp: '2025-12-09T09:59:00.000Z',
  message: {
    role: 'user',
    content: 'Hello world',
  },
});

const ASSISTANT_NO_USAGE = JSON.stringify({
  type: 'assistant',
  sessionId: 'test-session-123',
  timestamp: '2025-12-09T10:01:00.000Z',
  message: {
    role: 'assistant',
    content: [{ type: 'text', text: 'response' }],
  },
});

const FILE_HISTORY_ENTRY = JSON.stringify({
  type: 'file-history-snapshot',
  messageId: 'some-uuid',
  snapshot: {},
});

// ============================================================================
// parseUsageEntry Tests
// ============================================================================

describe('parseUsageEntry', () => {
  test('parses valid assistant entry with full usage data', () => {
    const result = parseUsageEntry(VALID_ASSISTANT_ENTRY, '/home/user/project');

    expect(result).not.toBeNull();
    expect(result!.inputTokens).toBe(1000);
    expect(result!.outputTokens).toBe(500);
    expect(result!.cacheCreationTokens).toBe(200);
    expect(result!.cacheReadTokens).toBe(100);
    expect(result!.model).toBe('claude-sonnet-4-5');
    expect(result!.sessionId).toBe('test-session-123');
    expect(result!.timestamp).toBe('2025-12-09T10:00:00.000Z');
    expect(result!.version).toBe('2.0.60');
  });

  test('parses assistant entry without cache tokens (defaults to 0)', () => {
    const result = parseUsageEntry(ASSISTANT_ENTRY_NO_CACHE, '/home/user/project');

    expect(result).not.toBeNull();
    expect(result!.inputTokens).toBe(2000);
    expect(result!.outputTokens).toBe(1000);
    expect(result!.cacheCreationTokens).toBe(0);
    expect(result!.cacheReadTokens).toBe(0);
    expect(result!.model).toBe('gemini-2.5-pro');
  });

  test('returns null for user entries', () => {
    const result = parseUsageEntry(USER_ENTRY, '/home/user/project');
    expect(result).toBeNull();
  });

  test('returns null for assistant entries without usage data', () => {
    const result = parseUsageEntry(ASSISTANT_NO_USAGE, '/home/user/project');
    expect(result).toBeNull();
  });

  test('returns null for file-history-snapshot entries', () => {
    const result = parseUsageEntry(FILE_HISTORY_ENTRY, '/home/user/project');
    expect(result).toBeNull();
  });

  test('returns null for empty lines', () => {
    expect(parseUsageEntry('', '/test')).toBeNull();
    expect(parseUsageEntry('   ', '/test')).toBeNull();
    expect(parseUsageEntry('\n', '/test')).toBeNull();
  });

  test('returns null for malformed JSON', () => {
    expect(parseUsageEntry('{invalid json}', '/test')).toBeNull();
    expect(parseUsageEntry('not json at all', '/test')).toBeNull();
  });

  test('strips UTF-8 BOM from line before parsing', () => {
    // UTF-8 BOM character (\uFEFF) can appear at start of files
    const bomEntry = '\uFEFF' + VALID_ASSISTANT_ENTRY;
    const result = parseUsageEntry(bomEntry, '/test');

    expect(result).not.toBeNull();
    expect(result!.model).toBe('claude-sonnet-4-5');
    expect(result!.inputTokens).toBe(1000);
  });

  test('includes project path in result', () => {
    const result = parseUsageEntry(VALID_ASSISTANT_ENTRY, '/custom/project/path');
    expect(result!.projectPath).toBe('/home/user/project');
  });

  test('falls back to the decoded project path when cwd is missing', () => {
    const withoutCwd = JSON.stringify({
      ...JSON.parse(VALID_ASSISTANT_ENTRY),
      cwd: undefined,
    });

    const result = parseUsageEntry(withoutCwd, '/custom/project/path');
    expect(result!.projectPath).toBe('/custom/project/path');
  });

  test('parses string target when present', () => {
    const withTarget = JSON.stringify({
      ...JSON.parse(VALID_ASSISTANT_ENTRY),
      target: 'droid',
    });
    const result = parseUsageEntry(withTarget, '/test');
    expect(result).not.toBeNull();
    expect(result!.target).toBe('droid');
  });

  test('ignores non-string target values', () => {
    const withNumericTarget = JSON.stringify({
      ...JSON.parse(VALID_ASSISTANT_ENTRY),
      target: 123,
    });
    const result = parseUsageEntry(withNumericTarget, '/test');
    expect(result).not.toBeNull();
    expect(result!.target).toBeUndefined();
  });

  test('returns null when required token fields are invalid', () => {
    const withInvalidUsage = JSON.stringify({
      ...JSON.parse(VALID_ASSISTANT_ENTRY),
      message: {
        model: 'claude-sonnet-4-5',
        usage: {
          input_tokens: '1500',
          output_tokens: -10,
          cache_creation_input_tokens: 'bad',
          cache_read_input_tokens: null,
        },
      },
    });

    const result = parseUsageEntry(withInvalidUsage, '/test');
    expect(result).toBeNull();
  });

  test('returns null when timestamp is missing', () => {
    const withoutTimestamp = JSON.stringify({
      ...JSON.parse(VALID_ASSISTANT_ENTRY),
      timestamp: undefined,
    });

    expect(parseUsageEntry(withoutTimestamp, '/test')).toBeNull();
  });
});
