import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
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
  it.each([
    ['document end', '', '...\n'],
    ['document start', '---\n', ''],
  ])('keeps a %s marker from breaking an unrelated save', (_name, start, end) => {
    const dir = getCcsDir();
    fs.mkdirSync(dir);
    const file = path.join(dir, 'config.yaml');
    fs.writeFileSync(
      file,
      start + 'version: 14\naccounts: {}\nprofiles: {}\ncursor: {enabled: true}\n' + end
    );
    mutateConfig((config) => {
      config.default = 'work';
    });
    const saved = fs.readFileSync(file, 'utf8');
    expect(yaml.load(saved)).toMatchObject({ default: 'work', cursor: { enabled: true } });
    expect(loadOrCreateUnifiedConfig().cursor).toEqual({ enabled: true });
    expect(fs.readdirSync(dir).filter((name) => name.startsWith('config.yaml.retired-'))).toEqual(
      []
    );
  });
  it('preserves document-marker text inside an opaque scalar', () => {
    const dir = getCcsDir();
    fs.mkdirSync(dir);
    fs.writeFileSync(
      path.join(dir, 'config.yaml'),
      'version: 14\ncursor:\n  markers: |\n    ---\n    ...\n'
    );
    mutateConfig((config) => {
      config.default = 'work';
    });
    expect(loadOrCreateUnifiedConfig().cursor).toEqual({ markers: '---\n...\n' });
  });
  it('backs up the original once before omitting an unencodable retired alias cycle', () => {
    const dir = getCcsDir();
    fs.mkdirSync(dir);
    const file = path.join(dir, 'config.yaml');
    const original = 'version: 14\ncursor: &retired\n  self: *retired\n';
    fs.writeFileSync(file, original);
    mutateConfig((config) => {
      config.default = 'work';
    });
    const backups = () =>
      fs.readdirSync(dir).filter((name) => name.startsWith('config.yaml.retired-'));
    expect(backups()).toHaveLength(1);
    const backup = path.join(dir, backups()[0]);
    expect(fs.readFileSync(backup, 'utf8')).toBe(original);
    expect(fs.statSync(backup).mode & 0o777).toBe(0o600);
    expect(loadOrCreateUnifiedConfig().cursor).toBeUndefined();
    expect(loadOrCreateUnifiedConfig().default).toBe('work');
    mutateConfig((config) => {
      config.default = 'other';
    });
    expect(backups()).toHaveLength(1);
    expect(loadOrCreateUnifiedConfig().default).toBe('other');
  });
  it('leaves the original unchanged when the required recovery backup cannot be created', () => {
    const dir = getCcsDir();
    fs.mkdirSync(dir);
    const file = path.join(dir, 'config.yaml');
    const original = 'version: 14\ncursor: &retired\n  self: *retired\n';
    fs.writeFileSync(file, original);
    const open = fs.openSync;
    const fault = spyOn(fs, 'openSync').mockImplementation(
      (...args: Parameters<typeof fs.openSync>) => {
        if (String(args[0]).startsWith(`${file}.retired-`)) {
          throw Object.assign(new Error('backup refused'), { code: 'EACCES' });
        }
        return open(...args);
      }
    );
    try {
      expect(() =>
        mutateConfig((config) => {
          config.default = 'work';
        })
      ).toThrow('backup refused');
      expect(fs.readFileSync(file, 'utf8')).toBe(original);
      expect(fs.readdirSync(dir)).toEqual(['config.yaml']);
    } finally {
      fault.mockRestore();
    }
  });
});
