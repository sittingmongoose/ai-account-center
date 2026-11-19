/**
 * M4a launch argv parity table (lane F). Generates `launch-table.json` from the REAL
 * `app-launcher.ts` builders: every app shape on every OS, with the unit name, the
 * systemd-run argv and the ordered environment entries the reference produces.
 * `crates/aac-agent/src/launch.rs`'s table test compares the Rust port against it.
 *
 *   bun tests/spec/agent/launch-table.ts          # rewrite the checked-in table
 *   bun tests/spec/agent/launch-table.ts --check  # fail when it drifted
 *
 * No launch happens here: only the exported pure builders run.
 */
import fs from 'fs';
import path from 'path';
import {
  buildSystemdRunArgs,
  sanitizeAppName,
  systemdRunEnvironment,
  transientUnitName,
  type AppLaunchSpec,
} from '../../../src/utils/app-launcher';

interface Case {
  name: string;
  spec: AppLaunchSpec;
  pid: number;
  suffix: string;
  inheritedXDG?: string;
}

const APP_SERVER_ENV = {
  HOME: '/home/u',
  PATH: '/usr/bin:/bin',
  OPENAI_API_KEY: 'sk-fixture-secret',
  EMPTY: undefined,
};

const cases: Case[] = [
  {
    name: 'codex-app-server-daemon',
    spec: {
      app: 'codex-app-server',
      exe: '/usr/bin/codex',
      args: ['app-server'],
      cwd: '/tmp/work',
      env: APP_SERVER_ENV,
      logFile: '/tmp/app-server.log',
    },
    pid: 123,
    suffix: 'abcdef12',
    inheritedXDG: '/run/user/1000',
  },
  {
    name: 'codex-desktop-gui',
    spec: { app: 'codex-desktop', exe: '/usr/lib/chatgpt/ChatGPT', args: [] },
    pid: 1,
    suffix: 'x',
  },
  {
    name: 'codex-desktop-mac',
    spec: {
      app: 'codex-desktop',
      exe: '/Applications/ChatGPT.app/Contents/MacOS/ChatGPT',
      args: ['--flag', 'value with spaces'],
      cwd: '/Users/u',
    },
    pid: 4242,
    suffix: 'deadbeef',
    inheritedXDG: '/run/user/501',
  },
  {
    name: 'codex-app-server-windows-exe',
    spec: {
      app: 'codex-app-server',
      exe: 'C:\\tools\\codex\\codex.exe',
      args: ['app-server', '--listen', 'unix://C:\\Users\\x\\.codex\\sock'],
      cwd: 'C:\\Users\\x',
      env: { USERPROFILE: 'C:\\Users\\x', PATH: 'C:\\Windows\\system32' },
      logFile: 'C:\\Users\\x\\.codex\\app-server.log',
    },
    pid: 999,
    suffix: 'abc123',
  },
  {
    name: 'chatgpt-windows-gui',
    spec: {
      app: 'codex-desktop',
      exe: 'C:\\Users\\x\\AppData\\Local\\Programs\\ChatGPT\\ChatGPT.exe',
      args: [],
    },
    pid: 7,
    suffix: 'q9',
  },
  {
    name: 'loader-env-trampoline',
    spec: {
      app: 'codex-desktop',
      exe: '/opt/app',
      args: ['--flag'],
      env: { KEEP: 'yes', LD_PRELOAD: '/secret/preload.so', LD_LIBRARY_PATH: '/secret/lib' },
    },
    pid: 5,
    suffix: 'loader01',
  },
  {
    name: 'loader-env-and-dollar-exe',
    spec: {
      app: 'app',
      exe: '/opt/a$b/app',
      args: ['$x', '$HOME'],
      env: { LD_PRELOAD: '/p.so' },
    },
    pid: 6,
    suffix: 'dollar01',
  },
  {
    name: 'dollar-args-escaped',
    spec: {
      app: 'app',
      exe: '/opt/app',
      args: ['--user-data-dir=/d$HOME/x', '$$', '${X}', 'plain'],
    },
    pid: 8,
    suffix: 'esc00001',
  },
  {
    name: 'invalid-env-names-dropped',
    spec: {
      app: 'codex-desktop',
      exe: '/usr/lib/chatgpt/ChatGPT',
      args: [],
      env: { KEEP: 'yes', 'BASH_FUNC_ml%%': '() { :; }', 'a.b': '1', '1BAD': '1', NUL: 'a\0b' },
    },
    pid: 9,
    suffix: 'env00001',
  },
  {
    name: 'empty-cwd-and-log-are-absent',
    spec: { app: 'app', exe: '/bin/true', args: [], cwd: '', logFile: '' },
    pid: 10,
    suffix: 'empty001',
  },
  {
    name: 'xdg-existing-key-keeps-position',
    spec: {
      app: 'codex-app-server',
      exe: '/usr/bin/codex',
      args: ['app-server'],
      env: { XDG_RUNTIME_DIR: '/run/user/stale', OPENAI_API_KEY: 'sk-fixture-secret' },
    },
    pid: 11,
    suffix: 'xdg00001',
    inheritedXDG: '/run/user/dashboard',
  },
  {
    name: 'xdg-empty-inherited-not-carried',
    spec: {
      app: 'app',
      exe: '/bin/true',
      args: [],
      env: { KEEP: 'yes' },
    },
    pid: 12,
    suffix: 'xdg00002',
    inheritedXDG: '',
  },
  {
    name: 'empty-env-object',
    spec: { app: 'app', exe: '/bin/true', args: [], env: {} },
    pid: 13,
    suffix: 'env00002',
    inheritedXDG: '/run/user/1000',
  },
  {
    name: 'sanitize-punctuation-app',
    spec: { app: 'Codex CLI!', exe: '/opt/app', args: [] },
    pid: 14,
    suffix: 'san00001',
  },
  {
    name: 'sanitize-empty-app',
    spec: { app: '!!!', exe: '/opt/app', args: [] },
    pid: 15,
    suffix: 'san00002',
  },
  {
    name: 'claude-profile-launcher-shape',
    spec: {
      app: 'claude-work2',
      exe: '/usr/bin/open',
      args: ['-n', '-a', 'Claude', '--args', '--user-data-dir=/home/u/Library/Application Support/Claude-work2'],
      cwd: '/home/u',
      env: { HOME: '/home/u' },
    },
    pid: 16,
    suffix: 'claude01',
  },
];

const platforms = ['linux', 'darwin', 'win32'] as const;

function build() {
  const rows: unknown[] = [];
  for (const c of cases) {
    for (const platform of platforms) {
      const unit = transientUnitName(c.spec.app, c.pid, c.suffix);
      const env = systemdRunEnvironment(c.spec, {
        ...(c.inheritedXDG === undefined ? {} : { XDG_RUNTIME_DIR: c.inheritedXDG }),
      });
      rows.push({
        name: `${c.name}/${platform}`,
        // JSON drops the undefined env values exactly as the verb's stdin envelope does.
        spec: JSON.parse(JSON.stringify(c.spec)),
        platform,
        pid: c.pid,
        suffix: c.suffix,
        inheritedXDG: c.inheritedXDG ?? null,
        expected: {
          sanitized: sanitizeAppName(c.spec.app),
          unit,
          route: platform === 'linux' ? 'systemd-service' : 'direct',
          argv: buildSystemdRunArgs(c.spec, unit),
          envEntries: env === undefined ? null : Object.entries(env),
        },
      });
    }
  }
  return {
    note: 'Generated by tests/spec/agent/launch-table.ts from src/utils/app-launcher.ts. Do not edit.',
    cases: rows,
  };
}

const target = path.join(import.meta.dir, 'launch-table.json');
const text = JSON.stringify(build(), null, 2) + '\n';
if (process.argv.includes('--check')) {
  const held = fs.readFileSync(target, 'utf8');
  if (held !== text) {
    console.error('launch-table.json drifted; run: bun tests/spec/agent/launch-table.ts');
    process.exit(1);
  }
  console.log(`launch-table: ${JSON.parse(text).cases.length} cases current`);
} else {
  fs.writeFileSync(target, text);
  console.log(`launch-table: wrote ${JSON.parse(text).cases.length} cases`);
}
