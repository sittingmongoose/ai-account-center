import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as yaml from 'js-yaml';
import { getCcsDir } from '../../utils/config-manager';
import {
  invalidateConfigCache,
  loadOrCreateUnifiedConfig,
  mutateConfig,
} from '../config-loader-facade';

let temporary: string;
let previous: string | undefined;
beforeEach(() => {
  temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-retired-config-'));
  previous = process.env.CCS_HOME;
  process.env.CCS_HOME = temporary;
  invalidateConfigCache();
});
afterEach(() => {
  if (previous === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = previous;
  invalidateConfigCache();
  fs.rmSync(temporary, { recursive: true, force: true });
});

describe('retired launcher config', () => {
  it('does not generate retired sections in a new config', () => {
    const config = loadOrCreateUnifiedConfig();
    for (const key of [
      'websearch',
      'global_env',
      'continuity',
      'copilot',
      'cursor',
      'thinking',
      'runtime',
      'channels',
      'browser',
      'image_analysis',
    ])
      expect(Object.hasOwn(config, key)).toBe(false);
    expect(fs.existsSync(getCcsDir())).toBe(false);
  });
  it('ignores invalid retired sections without modifying an old file on read', () => {
    const dir = getCcsDir();
    fs.mkdirSync(dir);
    const file = path.join(dir, 'config.yaml');
    const original =
      'version: 1\nbrowser: invalid\nwebsearch: false\nimage_analysis: [unrecognised]\n';
    fs.writeFileSync(file, original);
    loadOrCreateUnifiedConfig();
    expect(fs.readFileSync(file, 'utf8')).toBe(original);
  });
  it('preserves opaque values and ordinary YAML blocks during an unrelated guarded write', () => {
    const dir = getCcsDir();
    fs.mkdirSync(dir);
    const file = path.join(dir, 'config.yaml');
    const ignored =
      "browser:\n  # Retained as written, with no interpretation.\n  claude: 'off'\nwebsearch: invalid\nimage_analysis:\n  future: [a, b]\n";
    fs.writeFileSync(file, 'version: 14\n' + ignored + 'future_section:\n  untouched: true\n');
    mutateConfig((config) => {
      config.dashboard_auth = { ...config.dashboard_auth!, session_lifetime_days: 30 };
    });
    const saved = fs.readFileSync(file, 'utf8');
    expect(saved).toContain(ignored);
    const parsed = yaml.load(saved) as Record<string, unknown>;
    expect(parsed.future_section).toEqual({ untouched: true });
    expect(loadOrCreateUnifiedConfig().dashboard_auth?.session_lifetime_days).toBe(30);
  });
});
