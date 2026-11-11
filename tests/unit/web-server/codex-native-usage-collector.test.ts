import { describe, expect, it } from 'bun:test';
import {
  createCodexNativeParserState,
  parseCodexNativeUsageLine,
  type CodexNativeParserState,
} from '../../../src/web-server/usage/codex-native-usage-collector';
import type { RawUsageEntry } from '../../../src/web-server/jsonl-parser';

function parseLines(
  lines: unknown[],
  state: CodexNativeParserState = createCodexNativeParserState(),
  includeCliproxySessions = false
): RawUsageEntry[] {
  const entries: RawUsageEntry[] = [];
  for (const line of lines) {
    const entry = parseCodexNativeUsageLine(
      JSON.stringify(line),
      state,
      includeCliproxySessions
    );
    if (entry) entries.push(entry);
  }
  return entries;
}

function rolloutLines(options: { sessionId?: string; modelProvider?: string; model?: string } = {}) {
  const sessionId = options.sessionId ?? 'codex-session-1';
  return [
    {
      timestamp: '2026-03-02T10:00:00.000Z',
      type: 'session_meta',
      payload: {
        id: sessionId,
        timestamp: '2026-03-02T10:00:00.000Z',
        cwd: '/tmp/codex-project',
        cli_version: '0.126.0',
        source: 'cli',
        model_provider: options.modelProvider ?? 'openai',
      },
    },
    {
      timestamp: '2026-03-02T10:00:01.000Z',
      type: 'turn_context',
      payload: {
        turn_id: 'turn-1',
        cwd: '/tmp/codex-project',
        model: options.model ?? 'gpt-5',
        effort: 'high',
      },
    },
    {
      timestamp: '2026-03-02T10:00:02.000Z',
      type: 'event_msg',
      payload: { type: 'token_count', info: null },
    },
    {
      timestamp: '2026-03-02T10:05:00.000Z',
      type: 'event_msg',
      payload: {
        type: 'token_count',
        info: {
          total_token_usage: {
            input_tokens: 100,
            cached_input_tokens: 20,
            output_tokens: 5,
            reasoning_output_tokens: 2,
            total_tokens: 105,
          },
          last_token_usage: {
            input_tokens: 100,
            cached_input_tokens: 20,
            output_tokens: 5,
            reasoning_output_tokens: 2,
            total_tokens: 105,
          },
          model_context_window: 200000,
        },
      },
    },
    // An identical repeat of the cumulative total is a duplicate report, not new usage.
    {
      timestamp: '2026-03-02T10:05:30.000Z',
      type: 'event_msg',
      payload: {
        type: 'token_count',
        info: {
          total_token_usage: {
            input_tokens: 100,
            cached_input_tokens: 20,
            output_tokens: 5,
            reasoning_output_tokens: 2,
            total_tokens: 105,
          },
          last_token_usage: {
            input_tokens: 100,
            cached_input_tokens: 20,
            output_tokens: 5,
            reasoning_output_tokens: 2,
            total_tokens: 105,
          },
          model_context_window: 200000,
        },
      },
    },
    {
      timestamp: '2026-03-02T10:10:00.000Z',
      type: 'event_msg',
      payload: {
        type: 'token_count',
        info: {
          total_token_usage: {
            input_tokens: 150,
            cached_input_tokens: 30,
            output_tokens: 10,
            reasoning_output_tokens: 3,
            total_tokens: 160,
          },
          last_token_usage: {
            input_tokens: 50,
            cached_input_tokens: 10,
            output_tokens: 5,
            reasoning_output_tokens: 1,
            total_tokens: 55,
          },
          model_context_window: 200000,
        },
      },
    },
  ];
}

describe('codex native usage parser', () => {
  it('parses token_count events into raw usage entries and suppresses duplicates', () => {
    const entries = parseLines(rolloutLines());

    expect(entries).toHaveLength(2);
    expect(entries[0]).toMatchObject({
      sessionId: 'codex-session-1',
      projectPath: '/tmp/codex-project',
      model: 'gpt-5',
      version: '0.126.0',
      target: 'codex',
      inputTokens: 80,
      cacheReadTokens: 20,
      outputTokens: 5,
    });
    expect(entries[1]).toMatchObject({
      inputTokens: 40,
      cacheReadTokens: 10,
      outputTokens: 5,
    });
  });

  it('counts cache reads, writes, and reasoning exactly once against Codex reported totals', () => {
    const usage = {
      input_tokens: 100,
      cached_input_tokens: 40,
      cache_write_input_tokens: 60,
      output_tokens: 10,
      reasoning_output_tokens: 5,
      total_tokens: 110,
    };
    const next = {
      input_tokens: 160,
      cached_input_tokens: 50,
      cache_write_input_tokens: 100,
      output_tokens: 20,
      reasoning_output_tokens: 11,
      total_tokens: 180,
    };
    const event = (snapshot: typeof usage, timestamp: string) => ({
      timestamp,
      type: 'event_msg',
      payload: { type: 'token_count', info: { total_token_usage: snapshot } },
    });
    const entries = parseLines([
      { type: 'session_meta', payload: { id: 'disjoint-usage', model_provider: 'openai' } },
      { type: 'turn_context', payload: { model: 'gpt-5.6' } },
      event(usage, '2026-10-01T15:00:00Z'),
      event(usage, '2026-10-01T15:01:00Z'),
      event(next, '2026-10-01T15:02:00Z'),
    ]);
    expect(entries).toHaveLength(2);
    expect(entries[0]).toMatchObject({
      inputTokens: 0,
      cacheReadTokens: 40,
      cacheCreationTokens: 60,
      outputTokens: 10,
    });
    expect(entries[1]).toMatchObject({
      inputTokens: 10,
      cacheReadTokens: 10,
      cacheCreationTokens: 40,
      outputTokens: 10,
    });
    expect(
      entries.reduce(
        (sum, entry) =>
          sum +
          entry.inputTokens +
          entry.outputTokens +
          entry.cacheReadTokens +
          entry.cacheCreationTokens,
        0
      )
    ).toBe(next.total_tokens);
  });

  it('skips cliproxy-backed codex sessions by default to avoid double counting', () => {
    const entries = parseLines(rolloutLines({ modelProvider: 'cliproxy' }));
    expect(entries).toHaveLength(0);
  });

  it('also skips ccs_runtime-backed codex bridge sessions by default', () => {
    const entries = parseLines(rolloutLines({ modelProvider: 'ccs_runtime' }));
    expect(entries).toHaveLength(0);
  });
});
