import { readFileSync } from 'fs';
import { describe, expect, it } from 'bun:test';

describe('agent lifecycle runner', () => {
  it('is named beside transport.py and in the spec guide', () => {
    const transport = readFileSync('tests/spec/agent/transport.py', 'utf8');
    const guide = readFileSync('tests/spec/README.md', 'utf8');
    expect(transport).toContain('tests/spec/agent/lifecycle.mjs');
    expect(guide).toContain('tests/spec/agent/lifecycle.mjs');
    expect(guide).toContain('not in `run.mjs`');
  });
});
