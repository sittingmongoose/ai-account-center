import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as yaml from 'js-yaml';
import { execFileSync } from 'node:child_process';
import { getCcsDir } from '../../utils/config-manager';
import {
  invalidateConfigCache,
  loadOrCreateUnifiedConfig,
  mutateConfig,
} from '../config-loader-facade';

let temporary: string;
let previous: string | undefined;

function backups(): string[] {
  return fs.readdirSync(getCcsDir()).filter((name) => name.startsWith('config.yaml.retired-'));
}

function wideCycle(section: string): string {
  return (
    'version: 14\n' +
    section +
    ': &r\n' +
    Array.from({ length: 40 }, (_, i) => `  key_number_${i}: "some value ${i}"\n`).join('') +
    '  self: *r\n'
  );
}

/** Product Node runtime, with a heap limit and timeout: an alias regression cannot kill Bun. */
function saveInBoundedNode(section: string): { elapsed: number; rss: number; self: boolean } {
  const loader = path.resolve(import.meta.dir, '../../../dist/config/unified-config-loader.js');
  const script = `
    const {mutateUnifiedConfig,loadOrCreateUnifiedConfig}=require(process.argv[1]);
    const started=performance.now();
    mutateUnifiedConfig(config=>{config.default='work'});
    const config=loadOrCreateUnifiedConfig();
    const section=config[process.argv[2]];
    process.stdout.write(JSON.stringify({elapsed:performance.now()-started,
      rss:process.resourceUsage().maxRSS,self:!!section && section.self===section}));
  `;
  const env = { ...process.env, CCS_HOME: temporary, AAC_NO_LOCAL_RUST_BUILD: '1' };
  delete env.CCS_DIR;
  return JSON.parse(
    execFileSync('node', ['--max-old-space-size=128', '-e', script, loader, section], {
      cwd: temporary,
      env,
      encoding: 'utf8',
      timeout: 5000,
      maxBuffer: 1024 * 1024,
    })
  );
}
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
    ['document end', '', '...\n', false],
    ['document start', '---\n', '', true],
  ] as const)(
    'keeps a %s marker from breaking an unrelated save',
    (_name, start, end, retained) => {
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
      expect(yaml.load(saved)).toMatchObject({ default: 'work' });
      expect(loadOrCreateUnifiedConfig().cursor).toEqual(retained ? { enabled: true } : undefined);
      expect(backups()).toHaveLength(retained ? 0 : 1);
    }
  );
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
  it.each(['cursor', 'profiles', 'accounts'])(
    'saves a 40-key %s self-reference quickly with bounded memory',
    (section) => {
      fs.mkdirSync(getCcsDir());
      fs.writeFileSync(path.join(getCcsDir(), 'config.yaml'), wideCycle(section));
      const result = saveInBoundedNode(section);
      expect(result.self).toBe(true);
      expect(result.elapsed).toBeLessThan(2000);
      expect(result.rss).toBeLessThan(256 * 1024);
      expect(backups()).toHaveLength(0);
      if (section !== 'cursor') {
        // Captured from df2619152's exact yaml.dump call for this active section.
        const golden = fs
          .readFileSync(
            path.join(import.meta.dir, 'fixtures', `${section}-cycle-main.yaml`),
            'utf8'
          )
          .trim();
        expect(fs.readFileSync(path.join(getCcsDir(), 'config.yaml'), 'utf8')).toContain(golden);
      }
    }
  );
  it.each(['cursor', 'profiles'])('does not expand shared alias graphs under %s', (section) => {
    fs.mkdirSync(getCcsDir());
    let original = `version: 14\n${section}:\n  level_0: &l0 {value: 1}\n`;
    for (let level = 1; level <= 20; level++) {
      original += `  level_${level}: &l${level} [${Array(10)
        .fill(`*l${level - 1}`)
        .join(', ')}]\n`;
    }
    const file = path.join(getCcsDir(), 'config.yaml');
    fs.writeFileSync(file, original);
    const result = saveInBoundedNode(section);
    expect(result.elapsed).toBeLessThan(2000);
    expect(result.rss).toBeLessThan(256 * 1024);
    expect(fs.statSync(file).size).toBeLessThan(20_000);
    const saved = yaml.load(fs.readFileSync(file, 'utf8')) as Record<
      string,
      { level_20: unknown[] }
    >;
    expect(saved[section].level_20[0]).toBe(saved[section].level_20[1]);
    expect(backups()).toHaveLength(0);
  });
  it('backs up the original once before omitting an unsafe raw retired block', () => {
    const dir = getCcsDir();
    fs.mkdirSync(dir);
    const file = path.join(dir, 'config.yaml');
    const original = wideCycle('cursor') + '...\n';
    fs.writeFileSync(file, original);
    mutateConfig((config) => {
      config.default = 'work';
    });
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
    const original = wideCycle('cursor') + '...\n';
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
  it('backs up non-UTF-8 original bytes without decoding or changing them', () => {
    fs.mkdirSync(getCcsDir());
    const original = Buffer.concat([
      Buffer.from('version: 14\ncursor:\n  note: "caf'),
      Buffer.from([0xe9]),
      Buffer.from('"\n...\n'),
    ]);
    fs.writeFileSync(path.join(getCcsDir(), 'config.yaml'), original);
    mutateConfig((config) => {
      config.default = 'work';
    });
    expect(backups()).toHaveLength(1);
    expect(fs.readFileSync(path.join(getCcsDir(), backups()[0]))).toEqual(original);
    expect(loadOrCreateUnifiedConfig().default).toBe('work');
  });
  it('reuses the same-content backup and names it after repeated main-write failures', () => {
    fs.mkdirSync(getCcsDir());
    const file = path.join(getCcsDir(), 'config.yaml');
    const original = Buffer.from(wideCycle('cursor') + '...\n');
    fs.writeFileSync(file, original);
    const rename = fs.renameSync;
    const fault = spyOn(fs, 'renameSync').mockImplementation(
      (...args: Parameters<typeof fs.renameSync>) => {
        if (String(args[1]) === file)
          throw Object.assign(new Error('rename refused'), { code: 'EACCES' });
        return rename(...args);
      }
    );
    try {
      for (let attempt = 0; attempt < 2; attempt++) {
        let failure = '';
        try {
          mutateConfig((config) => {
            config.default = 'work';
          });
        } catch (error) {
          failure = (error as Error).message;
        }
        expect(backups()).toHaveLength(1);
        expect(failure).toContain('rename refused');
        expect(failure).toContain(path.join(getCcsDir(), backups()[0]));
        expect(fs.readFileSync(file)).toEqual(original);
      }
    } finally {
      fault.mockRestore();
    }
    mutateConfig((config) => {
      config.default = 'work';
    });
    expect(backups()).toHaveLength(1);
    expect(loadOrCreateUnifiedConfig().default).toBe('work');
  });
  it('reuses a byte-identical backup with an earlier filename', () => {
    fs.mkdirSync(getCcsDir());
    const original = Buffer.from('version: 14\ncursor: {enabled: true}\n...\n');
    fs.writeFileSync(path.join(getCcsDir(), 'config.yaml'), original);
    const oldName = 'config.yaml.retired-previous';
    fs.writeFileSync(path.join(getCcsDir(), oldName), original, { mode: 0o600 });
    mutateConfig((config) => {
      config.default = 'work';
    });
    expect(backups()).toEqual([oldName]);
    expect(loadOrCreateUnifiedConfig().default).toBe('work');
  });
});
