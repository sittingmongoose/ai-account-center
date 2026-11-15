import { readFileSync } from 'fs';
import { describe, expect, it } from 'bun:test';

describe('check-windows ssh', () => {
  it('refuses host-key updates on the build-host connection', () => {
    const line = readFileSync('scripts/foundation/check-windows.sh', 'utf8')
      .split('\n')
      .find((row) => row.includes('TASK_SSH='));
    expect(line).toContain('UpdateHostKeys=no');
  });
});
