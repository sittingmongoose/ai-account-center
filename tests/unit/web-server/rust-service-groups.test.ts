/** The per-route-group switches: home default-ready (on when unset), status off; the env wins; invalid is off. */
import { afterAll, beforeAll, describe, expect, it } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { rustGroupOn } from '../../../src/web-server/rust-service-groups';
import { saveUnifiedConfig } from '../../../src/config/unified-config-loader';
import {
  createEmptyUnifiedConfig,
  type UnifiedConfig,
} from '../../../src/config/schemas/unified-config';

const saved = {
  ccsHome: process.env.CCS_HOME,
  ccsDir: process.env.CCS_DIR,
  home: process.env.AAC_RUST_GROUP_HOME,
  status: process.env.AAC_RUST_GROUP_STATUS,
};
let root: string;
let yaml: string;

/** Write the fixture config's rustService block (absent leaves the file without one). */
function config(rustService: UnifiedConfig['rustService']): void {
  const value = createEmptyUnifiedConfig();
  value.rustService = rustService;
  saveUnifiedConfig(value);
}

beforeAll(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-groups-'));
  yaml = path.join(root, '.ccs', 'config.yaml');
  process.env.CCS_HOME = root;
  delete process.env.CCS_DIR;
  delete process.env.AAC_RUST_GROUP_HOME;
  delete process.env.AAC_RUST_GROUP_STATUS;
});

afterAll(() => {
  for (const [key, value] of [
    ['CCS_HOME', saved.ccsHome],
    ['CCS_DIR', saved.ccsDir],
    ['AAC_RUST_GROUP_HOME', saved.home],
    ['AAC_RUST_GROUP_STATUS', saved.status],
  ] as const)
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
  fs.rmSync(root, { recursive: true, force: true });
});

describe('rustGroupOn', () => {
  it('home is default-ready: on when neither the env nor the config names it', () => {
    config(undefined);
    expect(rustGroupOn('home')).toBe(true);
    expect(rustGroupOn('status')).toBe(false);
  });

  it('the config turns home off and status on', () => {
    config({ mode: 'on', groups: { home: 'off', status: 'on' } });
    expect(rustGroupOn('home')).toBe(false);
    expect(rustGroupOn('status')).toBe(true);
  });

  it('the environment wins over the config', () => {
    config({ mode: 'on', groups: { home: 'off', status: 'on' } });
    process.env.AAC_RUST_GROUP_HOME = 'on';
    process.env.AAC_RUST_GROUP_STATUS = 'off';
    try {
      expect(rustGroupOn('home')).toBe(true);
      expect(rustGroupOn('status')).toBe(false);
    } finally {
      delete process.env.AAC_RUST_GROUP_HOME;
      delete process.env.AAC_RUST_GROUP_STATUS;
    }
  });

  it('an explicit invalid value is off, not the default', () => {
    // 'yes' is outside the schema's words; the file still parses as YAML.
    fs.mkdirSync(path.dirname(yaml), { recursive: true });
    fs.writeFileSync(yaml, 'version: 1\nrustService:\n  mode: on\n  groups:\n    home: yes\n    status: on\n');
    expect(rustGroupOn('home')).toBe(false);
    expect(rustGroupOn('status')).toBe(true);
    process.env.AAC_RUST_GROUP_HOME = '1';
    try {
      expect(rustGroupOn('home')).toBe(false);
    } finally {
      delete process.env.AAC_RUST_GROUP_HOME;
    }
  });

  it('an unreadable config takes the default (the mode is the ceiling elsewhere)', () => {
    fs.writeFileSync(yaml, 'rustService: [broken\n');
    expect(rustGroupOn('home')).toBe(true);
    expect(rustGroupOn('status')).toBe(false);
  });

  it('a groups value that is not an object takes the default', () => {
    fs.writeFileSync(yaml, 'version: 1\nrustService:\n  mode: on\n  groups: home\n');
    expect(rustGroupOn('home')).toBe(true);
    expect(rustGroupOn('status')).toBe(false);
  });
});