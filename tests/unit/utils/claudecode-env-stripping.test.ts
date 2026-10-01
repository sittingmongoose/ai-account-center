import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  mock,
  spyOn,
} from 'bun:test';
import { EventEmitter } from 'events';
import * as childProcess from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

type SpawnCall = {
  command: string;
  args: string[];
  options: Record<string, unknown> | undefined;
};

type SpawnSyncCall = {
  command: string;
  args: string[];
  options: Record<string, unknown> | undefined;
};

const spawnCalls: SpawnCall[] = [];
const spawnSyncCalls: SpawnSyncCall[] = [];
const launchSettingsSnapshots: Array<{ path: string; content: string }> = [];
const originalPlatform = process.platform;
let baselineSigintListeners: Array<(...args: unknown[]) => void> = [];
let baselineSigtermListeners: Array<(...args: unknown[]) => void> = [];
let baselineSighupListeners: Array<(...args: unknown[]) => void> = [];
let originalCcsHome: string | undefined;
let originalCcsClaudePath: string | undefined;
let originalDisableAutoUpdater: string | undefined;
let originalClaudeConfigDir: string | undefined;
let originalTmux: string | undefined;
const realSpawn = childProcess.spawn.bind(childProcess);
const realSpawnSync = childProcess.spawnSync.bind(childProcess);
const realExecSync = childProcess.execSync.bind(childProcess);

function createMockChild(): EventEmitter & {
  stdout: EventEmitter;
  stderr: EventEmitter;
  exitCode: number | null;
  killed: boolean;
  pid: number;
  unref: () => EventEmitter;
  kill: () => boolean;
} {
  const child = new EventEmitter() as EventEmitter & {
    stdout: EventEmitter;
    stderr: EventEmitter;
    exitCode: number | null;
    killed: boolean;
    pid: number;
    unref: () => EventEmitter;
    kill: () => boolean;
  };

  child.stdout = new EventEmitter();
  child.stderr = new EventEmitter();
  child.exitCode = null;
  child.killed = false;
  child.pid = process.pid;
  child.unref = () => child;
  child.kill = () => {
    child.killed = true;
    child.exitCode = 1;
    return true;
  };

  return child;
}

function shouldMockCommand(command: string): boolean {
  const normalized = command.toLowerCase();
  return normalized.includes('claude');
}

function registerChildProcessMock(): void {
  mock.module('child_process', () => ({
    ...childProcess,
    spawn: (...spawnArgs: unknown[]) => {
      const command = String(spawnArgs[0] ?? '');
      const maybeArgs = spawnArgs[1];
      const args = Array.isArray(maybeArgs) ? (maybeArgs as string[]) : [];
      const options = (Array.isArray(maybeArgs) ? spawnArgs[2] : spawnArgs[1]) as
        | Record<string, unknown>
        | undefined;

      if (!shouldMockCommand(command)) {
        return realSpawn(command, args, options as Parameters<typeof childProcess.spawn>[2]);
      }

      spawnCalls.push({ command, args, options });
      const settingsIndex = args.indexOf('--settings');
      if (settingsIndex >= 0) {
        const settingsPath = args[settingsIndex + 1];
        if (settingsPath && fs.existsSync(settingsPath)) {
          launchSettingsSnapshots.push({
            path: settingsPath,
            content: fs.readFileSync(settingsPath, 'utf8'),
          });
        }
      }

      const child = createMockChild();
      setTimeout(() => child.emit('close', 0), 0);
      return child;
    },
    spawnSync: (...spawnArgs: unknown[]) => {
      const command = String(spawnArgs[0] ?? '');
      const maybeArgs = spawnArgs[1];
      const args = Array.isArray(maybeArgs) ? (maybeArgs as string[]) : [];
      const options = (Array.isArray(maybeArgs) ? spawnArgs[2] : spawnArgs[1]) as
        | Record<string, unknown>
        | undefined;

      if (command === 'tmux') {
        spawnSyncCalls.push({ command, args, options });
        return {
          pid: process.pid,
          output: ['', '', ''],
          stdout: '',
          stderr: '',
          status: 0,
          signal: null,
        };
      }

      return realSpawnSync(command, args, options as Parameters<typeof childProcess.spawnSync>[2]);
    },
    execSync: (...execArgs: unknown[]) =>
      realExecSync(
        execArgs[0] as Parameters<typeof childProcess.execSync>[0],
        execArgs[1] as Parameters<typeof childProcess.execSync>[1]
      ),
  }));
}

const tempCcsHomes = new Set<string>();

function createTempCcsHome(prefix: string): string {
  const tempHome = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  tempCcsHomes.add(tempHome);
  return tempHome;
}

function writeConfigWithAutoUpdatePreference(enabled: boolean): void {
  const tempHome = createTempCcsHome('ccs-auto-update-pref-');
  process.env.CCS_HOME = tempHome;
  const ccsDir = path.join(tempHome, '.ccs');
  fs.mkdirSync(ccsDir, { recursive: true });
  const yaml = `version: 8
preferences:
  auto_update: ${enabled ? 'true' : 'false'}
`;
  fs.writeFileSync(path.join(ccsDir, 'config.yaml'), yaml, 'utf8');
}

function writeConfigWithWebSearchSettings(yamlBody: string): void {
  const tempHome = createTempCcsHome('ccs-websearch-env-');
  process.env.CCS_HOME = tempHome;
  const ccsDir = path.join(tempHome, '.ccs');
  fs.mkdirSync(ccsDir, { recursive: true });
  const yaml = `version: 8
preferences:
  auto_update: true
websearch:
${yamlBody}
`;
  fs.writeFileSync(path.join(ccsDir, 'config.yaml'), yaml, 'utf8');
}

let execClaude: typeof import('../../../src/utils/shell-executor').execClaude;
let stripAnthropicRoutingEnv: typeof import('../../../src/utils/shell-executor').stripAnthropicRoutingEnv;
let stripClaudeCodeEnv: typeof import('../../../src/utils/shell-executor').stripClaudeCodeEnv;
let SharedManager: typeof import('../../../src/management/shared-manager').default;

beforeAll(async () => {
  registerChildProcessMock();

  const shellExecutor = await import('../../../src/utils/shell-executor');
  execClaude = shellExecutor.execClaude;
  stripAnthropicRoutingEnv = shellExecutor.stripAnthropicRoutingEnv;
  stripClaudeCodeEnv = shellExecutor.stripClaudeCodeEnv;

  const sharedManagerModule = await import('../../../src/management/shared-manager');
  SharedManager = sharedManagerModule.default;
});

afterAll(() => {
  mock.restore();
});

describe('CLAUDECODE environment stripping', () => {
  beforeEach(() => {
    spawnCalls.length = 0;
    spawnSyncCalls.length = 0;
    launchSettingsSnapshots.length = 0;
    process.env.CCS_QUIET = '1';

    // Save original env values for restoration in afterEach
    originalCcsHome = process.env.CCS_HOME;
    originalCcsClaudePath = process.env.CCS_CLAUDE_PATH;
    originalDisableAutoUpdater = process.env.DISABLE_AUTOUPDATER;
    originalClaudeConfigDir = process.env.CLAUDE_CONFIG_DIR;
    originalTmux = process.env.TMUX;

    // Clear CCS-managed env vars that leak from host sessions
    delete process.env.DISABLE_AUTOUPDATER;
    delete process.env.CLAUDE_CONFIG_DIR;
    delete process.env.TMUX;

    baselineSigintListeners = process.listeners('SIGINT');
    baselineSigtermListeners = process.listeners('SIGTERM');
    baselineSighupListeners = process.listeners('SIGHUP');
  });

  afterEach(async () => {
    Object.defineProperty(process, 'platform', { value: originalPlatform });
    delete process.env.CLAUDECODE;
    delete process.env.claudecode;
    delete process.env.CCS_QUIET;
    delete process.env.CCS_WEBSEARCH_TRACE;
    if (originalCcsHome !== undefined) process.env.CCS_HOME = originalCcsHome;
    else delete process.env.CCS_HOME;
    if (originalCcsClaudePath !== undefined) process.env.CCS_CLAUDE_PATH = originalCcsClaudePath;
    else delete process.env.CCS_CLAUDE_PATH;
    if (originalDisableAutoUpdater !== undefined) {
      process.env.DISABLE_AUTOUPDATER = originalDisableAutoUpdater;
    } else {
      delete process.env.DISABLE_AUTOUPDATER;
    }
    if (originalClaudeConfigDir !== undefined)
      process.env.CLAUDE_CONFIG_DIR = originalClaudeConfigDir;
    else delete process.env.CLAUDE_CONFIG_DIR;
    if (originalTmux !== undefined) process.env.TMUX = originalTmux;
    else delete process.env.TMUX;
    delete process.env.ANTHROPIC_BASE_URL;
    delete process.env.ANTHROPIC_AUTH_TOKEN;
    delete process.env.ANTHROPIC_API_KEY;
    delete process.env.ANTHROPIC_MODEL;
    delete process.env.ANTHROPIC_DEFAULT_OPUS_MODEL;
    delete process.env.ANTHROPIC_DEFAULT_SONNET_MODEL;
    delete process.env.ANTHROPIC_DEFAULT_HAIKU_MODEL;
    delete process.env.ANTHROPIC_DEFAULT_FABLE_MODEL;
    delete process.env.ANTHROPIC_SMALL_FAST_MODEL;
    delete process.env.CLAUDE_CODE_MAX_OUTPUT_TOKENS;

    for (const listener of process.listeners('SIGINT')) {
      if (!baselineSigintListeners.includes(listener)) {
        process.removeListener('SIGINT', listener as (...args: unknown[]) => void);
      }
    }
    for (const listener of process.listeners('SIGTERM')) {
      if (!baselineSigtermListeners.includes(listener)) {
        process.removeListener('SIGTERM', listener as (...args: unknown[]) => void);
      }
    }
    for (const listener of process.listeners('SIGHUP')) {
      if (!baselineSighupListeners.includes(listener)) {
        process.removeListener('SIGHUP', listener as (...args: unknown[]) => void);
      }
    }

    for (const tempHome of tempCcsHomes) {
      fs.rmSync(tempHome, { recursive: true, force: true });
    }
    tempCcsHomes.clear();
  });

  it('stripClaudeCodeEnv removes CLAUDECODE case-insensitively', () => {
    const input: NodeJS.ProcessEnv = {
      CLAUDECODE: 'upper',
      claudecode: 'lower',
      ClAuDeCoDe: 'mixed',
      PATH: '/usr/bin',
    };

    const result = stripClaudeCodeEnv(input);
    expect(Object.keys(result).map((k) => k.toUpperCase())).not.toContain('CLAUDECODE');
    expect(result.PATH).toBe('/usr/bin');
  });

  it('stripAnthropicRoutingEnv preserves routing/auth keys from preserveFrom on the non-proxy settings path', () => {
    const settingsEnv: NodeJS.ProcessEnv = {
      ANTHROPIC_BASE_URL: 'https://ollama.com',
      ANTHROPIC_AUTH_TOKEN: 'profile-token',
      ANTHROPIC_API_KEY: 'profile-api-key',
      ANTHROPIC_MODEL: 'minimax-m2.7:cloud',
      OTHER: 'keep',
    };
    const globalEnv: NodeJS.ProcessEnv = {
      ANTHROPIC_BASE_URL: 'http://127.0.0.1:9999',
      ANTHROPIC_AUTH_TOKEN: 'global-stale',
    };

    const merged = stripAnthropicRoutingEnv({ ...globalEnv, ...settingsEnv }, settingsEnv);

    expect(merged.ANTHROPIC_BASE_URL).toBe('https://ollama.com');
    expect(merged.ANTHROPIC_AUTH_TOKEN).toBe('profile-token');
    expect(merged.ANTHROPIC_API_KEY).toBe('profile-api-key');
    expect(merged.ANTHROPIC_MODEL).toBe('minimax-m2.7:cloud');
    expect(merged.OTHER).toBe('keep');
  });

  it('stripAnthropicRoutingEnv with empty preserveFrom strips all routing keys', () => {
    const result = stripAnthropicRoutingEnv(
      {
        ANTHROPIC_BASE_URL: 'http://inherited',
        ANTHROPIC_AUTH_TOKEN: 'inherited-token',
        ANTHROPIC_MODEL: 'claude-opus-4-7',
      },
      { ANTHROPIC_MODEL: 'claude-opus-4-7' }
    );
    expect(result.ANTHROPIC_BASE_URL).toBeUndefined();
    expect(result.ANTHROPIC_AUTH_TOKEN).toBeUndefined();
    expect(result.ANTHROPIC_MODEL).toBe('claude-opus-4-7');
  });

  it('stripAnthropicRoutingEnv preserveFrom only emits keys present in preserveFrom', () => {
    const env = {
      ANTHROPIC_BASE_URL: 'http://inherited',
      ANTHROPIC_AUTH_TOKEN: 'inherited-token',
    };
    expect(stripAnthropicRoutingEnv(env, { ANTHROPIC_BASE_URL: 'https://ollama.com' })).toEqual({
      ANTHROPIC_BASE_URL: 'https://ollama.com',
    });
    expect(stripAnthropicRoutingEnv(env, { ANTHROPIC_AUTH_TOKEN: 'profile-token' })).toEqual({
      ANTHROPIC_AUTH_TOKEN: 'profile-token',
    });
    expect(stripAnthropicRoutingEnv(env, { ANTHROPIC_API_KEY: 'profile-api-key' })).toEqual({
      ANTHROPIC_API_KEY: 'profile-api-key',
    });
    expect(stripAnthropicRoutingEnv(env, { ANTHROPIC_BASE_URL: '' })).toEqual({
      ANTHROPIC_BASE_URL: '',
    });
  });

  it('stripAnthropicRoutingEnv removes routing/auth env case-insensitively while preserving model vars', () => {
    const input: NodeJS.ProcessEnv = {
      anthropic_base_url: 'http://127.0.0.1:8317/api/provider/codex',
      Anthropic_Auth_Token: 'parent-routing-token',
      ANTHROPIC_API_KEY: 'parent-api-key',
      ANTHROPIC_MODEL: 'gpt-5.4',
      ANTHROPIC_DEFAULT_SONNET_MODEL: 'gpt-5.4',
      PATH: '/usr/bin',
    };

    const result = stripAnthropicRoutingEnv(input);
    expect(result.anthropic_base_url).toBeUndefined();
    expect(result.Anthropic_Auth_Token).toBeUndefined();
    expect(result.ANTHROPIC_API_KEY).toBeUndefined();
    expect(result.ANTHROPIC_MODEL).toBe('gpt-5.4');
    expect(result.ANTHROPIC_DEFAULT_SONNET_MODEL).toBe('gpt-5.4');
    expect(result.PATH).toBe('/usr/bin');
  });

  it('execClaude strips CLAUDECODE from merged env (including overrides)', () => {
    process.env.CLAUDECODE = 'from-parent';
    process.env.claudecode = 'from-parent-lower';

    execClaude('claude', ['--version'], {
      CCS_PROFILE_TYPE: 'default',
      CLAUDECODE: 'from-override',
      CCS_WEBSEARCH_SKIP: '1',
    });

    expect(spawnCalls.length).toBeGreaterThan(0);
    const env = spawnCalls[0].options?.env as NodeJS.ProcessEnv;
    expect(env).toBeDefined();
    expect(Object.keys(env).map((k) => k.toUpperCase())).not.toContain('CLAUDECODE');
    expect(env.CCS_WEBSEARCH_ENABLED || env.CCS_WEBSEARCH_SKIP).toBeDefined();
  });

  it('execClaude keeps behavior when CLAUDECODE is absent', () => {
    execClaude('claude', ['--help'], { CCS_PROFILE_TYPE: 'default' });

    expect(spawnCalls.length).toBeGreaterThan(0);
    const env = spawnCalls[0].options?.env as NodeJS.ProcessEnv;
    expect(env).toBeDefined();
    expect(Object.keys(env).map((k) => k.toUpperCase())).not.toContain('CLAUDECODE');
    expect(env.CCS_PROFILE_TYPE).toBe('default');
  });

  it('execClaude strips CLAUDECODE on Windows shell launch path', () => {
    Object.defineProperty(process, 'platform', { value: 'win32' });
    process.env.CLAUDECODE = 'set';

    execClaude('claude.cmd', ['--version'], { CCS_PROFILE_TYPE: 'default' });

    expect(spawnCalls.length).toBeGreaterThan(0);
    const env = spawnCalls[0].options?.env as NodeJS.ProcessEnv;
    expect(Object.keys(env).map((k) => k.toUpperCase())).not.toContain('CLAUDECODE');
    expect(spawnCalls[0].options?.shell).toBe('C:\\Windows\\System32\\cmd.exe');
  });

  it('execClaude sets DISABLE_AUTOUPDATER=1 when preferences.auto_update is false', () => {
    writeConfigWithAutoUpdatePreference(false);
    execClaude('claude', ['--version'], { CCS_PROFILE_TYPE: 'default' });

    expect(spawnCalls.length).toBeGreaterThan(0);
    const env = spawnCalls[0].options?.env as NodeJS.ProcessEnv;
    expect(env.DISABLE_AUTOUPDATER).toBe('1');
  });

  it('execClaude does not force DISABLE_AUTOUPDATER when preferences.auto_update is true', () => {
    writeConfigWithAutoUpdatePreference(true);
    execClaude('claude', ['--version'], { CCS_PROFILE_TYPE: 'default' });

    expect(spawnCalls.length).toBeGreaterThan(0);
    const env = spawnCalls[0].options?.env as NodeJS.ProcessEnv;
    expect(env.DISABLE_AUTOUPDATER).toBeUndefined();
  });

  it('execClaude overrides stale inherited WebSearch provider flags with config-derived values', () => {
    writeConfigWithWebSearchSettings(`  enabled: true
  providers:
    duckduckgo:
      enabled: true
    searxng:
      enabled: false
      url: ''
`);
    process.env.CCS_WEBSEARCH_SEARXNG = '1';
    process.env.CCS_WEBSEARCH_SEARXNG_URL = 'https://search.example.com';
    process.env.CCS_WEBSEARCH_SKIP = '1';

    execClaude('claude', ['--version'], { CCS_PROFILE_TYPE: 'settings' });

    expect(spawnCalls.length).toBeGreaterThan(0);
    const env = spawnCalls[0].options?.env as NodeJS.ProcessEnv;
    expect(env.CCS_WEBSEARCH_ENABLED).toBe('1');
    expect(env.CCS_WEBSEARCH_SKIP).toBe('0');
    expect(env.CCS_WEBSEARCH_DUCKDUCKGO).toBe('1');
    expect(env.CCS_WEBSEARCH_SEARXNG).toBe('0');
  });

  it('execClaude normalizes shared plugin metadata before default-profile launch', () => {
    const normalizeSpy = spyOn(
      SharedManager.prototype,
      'normalizeSharedPluginMetadataPaths'
    ).mockImplementation(() => {});

    execClaude('claude', ['--help'], { CCS_PROFILE_TYPE: 'default' });

    expect(normalizeSpy).toHaveBeenCalledWith(undefined);
  });

  it('execClaude normalizes shared plugin metadata using CLAUDE_CONFIG_DIR when provided', () => {
    const normalizeSpy = spyOn(
      SharedManager.prototype,
      'normalizeSharedPluginMetadataPaths'
    ).mockImplementation(() => {});
    const instancePath = path.join(os.tmpdir(), 'ccs-shell-executor-instance');

    execClaude('claude', ['--help'], {
      CCS_PROFILE_TYPE: 'settings',
      CLAUDE_CONFIG_DIR: instancePath,
    });

    expect(normalizeSpy).toHaveBeenCalledWith(instancePath);
  });

  it('execClaude strips inherited ANTHROPIC routing env but keeps model intent for settings-profile Claude launches', () => {
    process.env.ANTHROPIC_BASE_URL = 'http://127.0.0.1:8317/api/provider/codex';
    process.env.ANTHROPIC_AUTH_TOKEN = 'ccs-internal-managed';
    process.env.ANTHROPIC_API_KEY = 'stale-api-key';
    process.env.ANTHROPIC_MODEL = 'gpt-5.4';
    process.env.ANTHROPIC_DEFAULT_OPUS_MODEL = 'gpt-5.4';
    process.env.ANTHROPIC_DEFAULT_SONNET_MODEL = 'gpt-5.4';
    process.env.ANTHROPIC_DEFAULT_HAIKU_MODEL = 'gpt-5.4-mini';
    process.env.ANTHROPIC_SMALL_FAST_MODEL = 'gpt-5-codex-mini';

    execClaude('claude', ['--help'], {
      CCS_PROFILE_TYPE: 'settings',
      CCS_STRIP_INHERITED_ANTHROPIC_ENV: '1',
      CLAUDE_CONFIG_DIR: path.join(os.tmpdir(), 'ccs-settings-profile-instance'),
      CCS_WEBSEARCH_SKIP: '1',
    });

    expect(spawnCalls.length).toBeGreaterThan(0);
    const env = spawnCalls[0].options?.env as NodeJS.ProcessEnv;
    expect(env.CCS_PROFILE_TYPE).toBe('settings');
    expect(env.CLAUDE_CONFIG_DIR).toContain('ccs-settings-profile-instance');
    expect(env.ANTHROPIC_BASE_URL).toBeUndefined();
    expect(env.ANTHROPIC_AUTH_TOKEN).toBeUndefined();
    expect(env.ANTHROPIC_API_KEY).toBeUndefined();
    expect(env.ANTHROPIC_MODEL).toBe('gpt-5.4');
    expect(env.ANTHROPIC_DEFAULT_OPUS_MODEL).toBe('gpt-5.4');
    expect(env.ANTHROPIC_DEFAULT_SONNET_MODEL).toBe('gpt-5.4');
    expect(env.ANTHROPIC_DEFAULT_HAIKU_MODEL).toBe('gpt-5.4-mini');
    expect(env.ANTHROPIC_SMALL_FAST_MODEL).toBe('gpt-5-codex-mini');
  });

  it('execClaude preserves routing env supplied via explicit settings-profile envVars', () => {
    process.env.ANTHROPIC_BASE_URL = 'http://parent-leaked';
    process.env.ANTHROPIC_AUTH_TOKEN = 'parent-leaked-token';

    execClaude('claude', ['--help'], {
      CCS_PROFILE_TYPE: 'settings',
      CCS_STRIP_INHERITED_ANTHROPIC_ENV: '1',
      ANTHROPIC_BASE_URL: 'http://127.0.0.1:8317/api/provider/codex',
      ANTHROPIC_AUTH_TOKEN: 'reintroduced-routing-token',
      ANTHROPIC_API_KEY: 'reintroduced-api-key',
      ANTHROPIC_MODEL: 'gpt-5.4',
      ANTHROPIC_DEFAULT_SONNET_MODEL: 'gpt-5.4',
    });

    expect(spawnCalls.length).toBeGreaterThan(0);
    const env = spawnCalls[0].options?.env as NodeJS.ProcessEnv;
    expect(env.ANTHROPIC_BASE_URL).toBe('http://127.0.0.1:8317/api/provider/codex');
    expect(env.ANTHROPIC_AUTH_TOKEN).toBe('reintroduced-routing-token');
    expect(env.ANTHROPIC_API_KEY).toBe('reintroduced-api-key');
    expect(env.ANTHROPIC_MODEL).toBe('gpt-5.4');
    expect(env.ANTHROPIC_DEFAULT_SONNET_MODEL).toBe('gpt-5.4');
  });

  it('execClaude sanitizes tmux teammate env for bridge-backed settings launches while keeping the launched child on the runtime proxy', () => {
    process.env.TMUX = 'session-1';
    process.env.ANTHROPIC_BASE_URL = 'http://127.0.0.1:8317/api/provider/codex';
    process.env.ANTHROPIC_AUTH_TOKEN = 'parent-routing-token';
    process.env.ANTHROPIC_API_KEY = 'parent-api-key';
    process.env.ANTHROPIC_MODEL = 'gpt-5.4';
    process.env.ANTHROPIC_DEFAULT_SONNET_MODEL = 'gpt-5.4';
    process.env.ANTHROPIC_DEFAULT_FABLE_MODEL = 'gpt-5.4-mini';

    execClaude('claude', ['--help'], {
      CCS_PROFILE_TYPE: 'settings',
      CLAUDE_CONFIG_DIR: path.join(os.tmpdir(), 'ccs-settings-profile-instance'),
      ANTHROPIC_BASE_URL: 'http://127.0.0.1:3456',
      ANTHROPIC_AUTH_TOKEN: 'fresh-runtime-token',
      ANTHROPIC_MODEL: 'gpt-5.4',
    });

    expect(spawnCalls.length).toBeGreaterThan(0);
    const childEnv = spawnCalls[0].options?.env as NodeJS.ProcessEnv;
    expect(childEnv.ANTHROPIC_BASE_URL).toBe('http://127.0.0.1:3456');
    expect(childEnv.ANTHROPIC_AUTH_TOKEN).toBe('fresh-runtime-token');

    const unsetBaseUrlCall = spawnSyncCalls.find(
      (call) => call.command === 'tmux' && call.args.join(' ') === 'setenv -u ANTHROPIC_BASE_URL'
    );
    const unsetAuthTokenCall = spawnSyncCalls.find(
      (call) => call.command === 'tmux' && call.args.join(' ') === 'setenv -u ANTHROPIC_AUTH_TOKEN'
    );
    const modelCall = spawnSyncCalls.find(
      (call) =>
        call.command === 'tmux' &&
        call.args[0] === 'setenv' &&
        call.args[1] === 'ANTHROPIC_MODEL' &&
        call.args[2] === 'gpt-5.4'
    );
    const fableModelCall = spawnSyncCalls.find(
      (call) =>
        call.command === 'tmux' &&
        call.args[0] === 'setenv' &&
        call.args[1] === 'ANTHROPIC_DEFAULT_FABLE_MODEL' &&
        call.args[2] === 'gpt-5.4-mini'
    );

    expect(unsetBaseUrlCall).toBeDefined();
    expect(unsetAuthTokenCall).toBeDefined();
    expect(modelCall).toBeDefined();
    expect(fableModelCall).toBeDefined();
  });
});
