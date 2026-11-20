import fs from 'node:fs';
import path from 'node:path';
export default (ctx) => {
  const repo = path.resolve(ctx.configDir, '..', '..', '..');
  const nativeName = process.platform === 'win32' ? 'ai-account-center.exe' : 'ai-account-center';
  const cli = [
    process.env.AAC_M6_TARGET_CLI,
    process.env.CARGO_TARGET_DIR && path.join(process.env.CARGO_TARGET_DIR, 'debug', nativeName),
    process.env.CARGO_TARGET_DIR && path.join(process.env.CARGO_TARGET_DIR, 'release', nativeName),
    path.join(repo, 'dist', 'native', nativeName),
  ].find((candidate) => candidate && fs.existsSync(candidate));
  if (!cli) {
    throw new Error(
      'share-route: no native dashboard CLI (set AAC_M6_TARGET_CLI or build dist/native/ai-account-center)'
    );
  }
  return {
    name: 'aac-share-sandbox',
    async setup() {
      const home = path.join(ctx.dir, 'fixture');
      fs.mkdirSync(home, { recursive: true });
      return { home, port: await ctx.freePort(), cli };
    },
    server: (_, s) => ({
      command: ['bun', path.join(ctx.configDir, 'server.ts'), String(s.port), s.home, s.cli],
      url: `http://127.0.0.1:${s.port}/`,
      timeoutMs: 240000,
      env: {
        CCS_HOME: path.join(s.home, 'ccs'),
        CCS_DIR: path.join(s.home, 'ccs'),
        HOME: s.home,
        USERPROFILE: s.home,
        APPDATA: path.join(s.home, 'AppData', 'Roaming'),
        LOCALAPPDATA: path.join(s.home, 'AppData', 'Local'),
        XDG_CONFIG_HOME: path.join(s.home, '.config'),
        XDG_DATA_HOME: path.join(s.home, '.local', 'share'),
        XDG_STATE_HOME: path.join(s.home, '.local', 'state'),
        XDG_CACHE_HOME: path.join(s.home, '.cache'),
        CODEX_HOME: path.join(s.home, '.codex'),
        CLAUDE_CONFIG_DIR: path.join(s.home, '.claude'),
        CCS_DASHBOARD_AUTH_ENABLED: 'true',
        AAC_TS_SIDECAR: 'off',
        AAC_AGENT_VERBS: 'off',
        AAC_AGENT_TRANSPORT: 'off',
        AAC_STORE_SHARE_BACKUP_DIR: 'off',
        AAC_STATUS_DOORBELL: 'off',
        AAC_T3_STATE_DB: path.join(s.home, 'pins.sqlite'),
        AAC_AGENT_REPORTS_DIR: path.join(s.home, 'agent-reports'),
        AAC_PM_MAIL_REGISTRY_DIR: path.join(s.home, 'registry'),
        SSH_AUTH_SOCK: '',
      },
    }),
    teardown(_, s) {
      if (s) fs.rmSync(s.home, { recursive: true, force: true });
    },
    consoleErrors: { fail: false }, // hostile content is expected to generate CSP refusals
    failedRequests: { fail: false },
  };
};
