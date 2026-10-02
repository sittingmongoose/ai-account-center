import { describe, expect, it } from 'bun:test';
import * as path from 'path';
import { resolveZcodeDbPath } from '../../../../src/web-server/usage/zcode-native-usage-collector';

describe('zcode database path', () => {
  it('resolves the default database path', () => {
    expect(resolveZcodeDbPath({ homeDir: '/h', env: {} })).toBe(
      path.join('/h', '.zcode', 'cli', 'db', 'db.sqlite')
    );
  });

  it('honors an absolute ZCODE_DB_PATH override', () => {
    expect(
      resolveZcodeDbPath({ homeDir: '/h', env: { ZCODE_DB_PATH: '/tmp/fixture.sqlite' } })
    ).toBe('/tmp/fixture.sqlite');
  });

  it('ignores relative overrides', () => {
    expect(resolveZcodeDbPath({ homeDir: '/h', env: { ZCODE_DB_PATH: 'relative.sqlite' } })).toBe(
      path.join('/h', '.zcode', 'cli', 'db', 'db.sqlite')
    );
  });
});
