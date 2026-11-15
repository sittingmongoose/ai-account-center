/** The environment the front door passes to the private Rust service it spawns. */

/**
 * Environment names a Local computer's agent verbs read, passed to the service it spawns them
 * from (L5 review M1). Only these: home and profile folders, the CCS and provider selectors,
 * locale and temp, the provider key names the quota/failover verbs read, the proxy settings their
 * HTTP client honours, and the Windows system names a child process needs. Front-door secrets,
 * NODE_*, LD_* and every other variable stay out. Windows names are case-insensitive.
 */
export const RUST_SERVICE_ENV_NAMES: readonly string[] = [
  'PATH',
  'HOME',
  'USER',
  'LOGNAME',
  'LANG',
  'LC_ALL',
  'LC_CTYPE',
  'TZ',
  'TMPDIR',
  'XDG_CONFIG_HOME',
  'XDG_DATA_HOME',
  'XDG_CACHE_HOME',
  'XDG_STATE_HOME',
  'CCS_HOME',
  'CCS_DIR',
  'CODEX_HOME',
  'CLAUDE_CONFIG_DIR',
  'CLAUDE_SECURESTORAGE_CONFIG_DIR',
  'USE_LOCAL_OAUTH',
  'USE_STAGING_OAUTH',
  'CLAUDE_LOCAL_OAUTH_API_BASE',
  'CLAUDE_CODE_CUSTOM_OAUTH_URL',
  'MUSE_SESSIONS_DIR',
  'PI_CODING_AGENT_DIR',
  'OMP_SESSION_DIRS',
  'ZCODE_DB_PATH',
  'ANTIGRAVITY_DATA_DIR',
  'KIMI_CODE_API_KEY',
  'KIMI_API_KEY',
  'ZAI_API_KEY',
  'ZHIPU_API_KEY',
  'BAILIAN_TOKEN_PLAN_API_KEY',
  'OPENCODE_GO_API_KEY',
  'OPENCODE_API_KEY',
  'HTTPS_PROXY',
  'https_proxy',
  'HTTP_PROXY',
  'http_proxy',
  'ALL_PROXY',
  'all_proxy',
  'NO_PROXY',
  'no_proxy',
  'USERPROFILE',
  'HOMEDRIVE',
  'HOMEPATH',
  'APPDATA',
  'LOCALAPPDATA',
  'TEMP',
  'TMP',
  'SYSTEMROOT',
  'WINDIR',
  'COMSPEC',
  'PATHEXT',
];

/** The allowlisted subset of `env`, each under its own spelling (no Windows case duplicates). */
export function rustServiceEnvironment(
  env: NodeJS.ProcessEnv = process.env,
  windows = process.platform === 'win32'
): NodeJS.ProcessEnv {
  const allowed = new Set(
    windows ? RUST_SERVICE_ENV_NAMES.map((name) => name.toUpperCase()) : RUST_SERVICE_ENV_NAMES
  );
  const chosen: NodeJS.ProcessEnv = {};
  for (const [name, value] of Object.entries(env)) {
    const key = windows ? name.toUpperCase() : name;
    if (value === undefined || !allowed.has(key)) continue;
    allowed.delete(key);
    chosen[name] = value;
  }
  return chosen;
}
