import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { handleBarLaunch } from '../launch-subcommand';
import { handleBarVersion } from '../version-subcommand';
import { handleBarCommand } from '../index';

let temporary: string;
let appsDir: string;
let ccsDir: string;
let output: string[];
let originalLog: typeof console.log;
let originalError: typeof console.error;
let originalExitCode: typeof process.exitCode;
let originalCcsHome: string | undefined;

function write(file: string, value: string, mode = 0o600): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, value, { mode });
}
function makeApp(identifier = 'party.sittingmongoose.ccs.accounts-bar'): string {
  const app = path.join(appsDir, 'AI Account Center.app');
  write(
    path.join(app, 'Contents/Info.plist'),
    `<?xml version="1.0"?><plist><dict><key>CFBundleIdentifier</key><string>${identifier}</string><key>CFBundleExecutable</key><string>CCSBar</string><key>CFBundleShortVersionString</key><string>2.0.0</string></dict></plist>`
  );
  write(path.join(app, 'Contents/MacOS/CCSBar'), 'offline executable fixture', 0o700);
  return app;
}
function privateSnapshot(): string {
  return JSON.stringify(
    ['bar/accounts-connection.json', 'codex-auto-switch.json', 'bar/.version'].map((name) => {
      const file = path.join(ccsDir, name);
      return [
        name,
        fs.readFileSync(file, 'utf8'),
        fs.statSync(file).mode & 0o777,
        fs.statSync(file).mtimeMs,
      ];
    })
  );
}

describe('read-only command fixtures', () => {
  beforeEach(() => {
    temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'bar-read-only-test-'));
    appsDir = path.join(temporary, 'Applications');
    ccsDir = path.join(temporary, 'private-ccs');
    write(path.join(ccsDir, 'bar/accounts-connection.json'), '{"offline":"connection fixture"}');
    write(path.join(ccsDir, 'codex-auto-switch.json'), '{"enabled":true,"thresholdPercent":5}');
    write(path.join(ccsDir, 'bar/.version'), '9.8.7');
    output = [];
    originalLog = console.log;
    originalError = console.error;
    originalExitCode = process.exitCode;
    originalCcsHome = process.env.CCS_HOME;
    process.env.CCS_HOME = ccsDir;
    console.log = (...values: unknown[]) => output.push(values.map(String).join(' '));
    console.error = (...values: unknown[]) => output.push(values.map(String).join(' '));
    process.exitCode = 0;
  });
  afterEach(() => {
    console.log = originalLog;
    console.error = originalError;
    process.exitCode = originalExitCode ?? 0;
    if (originalCcsHome === undefined) delete process.env.CCS_HOME;
    else process.env.CCS_HOME = originalCcsHome;
    fs.rmSync(temporary, { recursive: true, force: true });
  });

  describe('bar CLI read-only and launch boundaries', () => {
    it('rejects unsupported native launch before config reads, server probes or process work', async () => {
      const before = privateSnapshot();
      let touched = 0;
      await handleBarLaunch([], {
        getPlatform: () => 'linux',
        getCcsDir: () => {
          touched += 1;
          return ccsDir;
        },
        findRunningServer: async () => {
          touched += 1;
          return null;
        },
        spawnDetachedServer: () => {
          touched += 1;
        },
      });
      expect(touched).toBe(0);
      expect(process.exitCode).toBe(1);
      expect(privateSnapshot()).toBe(before);
    });
    it('reads the actual owned canonical version instead of a stale version pin', async () => {
      makeApp();
      fs.symlinkSync('AI Account Center.app', path.join(appsDir, 'CCS Bar.app'));
      const before = privateSnapshot();
      let configCalls = 0;
      await handleBarVersion({
        getAppsDir: () => appsDir,
        getVersion: () => '8.10.0',
        getCcsDir: () => {
          configCalls += 1;
          return ccsDir;
        },
      });
      expect(output.some((line) => line.includes('AI Account Center macOS app: v2.0.0'))).toBe(
        true
      );
      expect(output.some((line) => line.includes('v9.8.7'))).toBe(false);
      expect(configCalls).toBe(0);
      expect(privateSnapshot()).toBe(before);
    });
    it('reports foreign native metadata without probing or rewriting saved state', async () => {
      makeApp('foreign.fixture');
      const before = privateSnapshot();
      let configCalls = 0;
      await handleBarVersion({
        getAppsDir: () => appsDir,
        getVersion: () => '8.10.0',
        getCcsDir: () => {
          configCalls += 1;
          return ccsDir;
        },
      });
      expect(output.some((line) => line.includes('metadata unavailable'))).toBe(true);
      expect(configCalls).toBe(0);
      expect(privateSnapshot()).toBe(before);
    });
    it('does not follow a recorded-version alias into saved connection material', async () => {
      const pin = path.join(ccsDir, 'bar/.version');
      const connection = path.join(ccsDir, 'bar/accounts-connection.json');
      fs.unlinkSync(pin);
      fs.symlinkSync(connection, pin);
      const before = fs.readFileSync(connection);
      await handleBarVersion({
        getAppsDir: () => appsDir,
        getVersion: () => '8.10.0',
        getCcsDir: () => ccsDir,
      });
      expect(output.join('\n')).not.toContain('connection fixture');
      expect(fs.readFileSync(connection)).toEqual(before);
      expect(fs.lstatSync(pin).isSymbolicLink()).toBe(true);
    });

    it('shows packaged-install help without changing private files', async () => {
      const before = privateSnapshot();
      await handleBarCommand(['install', '--help']);
      expect(output.join('\n')).toContain('ai-account-center bar');
      expect(output.join('\n')).toContain('native sources included in this package');
      expect(output.join('\n')).not.toContain('ccs-bar-latest');
      expect(output.join('\n')).not.toContain('xattr -dr');
      expect(privateSnapshot()).toBe(before);
      expect(fs.readdirSync(ccsDir).sort()).toEqual(['bar', 'codex-auto-switch.json']);
    });
  });
});
