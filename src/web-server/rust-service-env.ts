/** The environment the front door passes to the private Rust service it spawns. */

/**
 * Environment names the service itself needs: home and profile folders, the CCS and provider
 * selectors, locale and temp, and the Windows system names a child process needs. Front-door
 * secrets, NODE_*, LD_* and every other variable stay out. Windows names are case-insensitive.
 * `SSH_AUTH_SOCK` stays out: a passphrase-protected key held only in ssh-agent falls back to the
 * TypeScript path, whose minimal SSH environment retains local agent authentication. The key named for agent transport has to
 * be unencrypted until an owner decides to pass the agent socket in.
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
  'USERPROFILE',
  'HOMEDRIVE',
  'HOMEPATH',
  'APPDATA',
  'LOCALAPPDATA',
  'TEMP',
  'TMP',
  'SYSTEMROOT',
  'SYSTEMDRIVE',
  'WINDIR',
  'COMSPEC',
  'PATHEXT',
];

/**
 * Names only a Local computer's agent verbs read: the provider keys the quota/failover verbs
 * read and the proxy settings their HTTP client honours (L5 review M1). They reach the service
 * only while the agent transport is on (Wonder L1): shadow reads never spawn a Local verb, so
 * the long-lived service holds no provider key for them.
 */
export const RUST_AGENT_ENV_NAMES: readonly string[] = [
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
];

/**
 * The allowlisted subset of `env`, each under its own spelling (no Windows case duplicates).
 * Provider keys and proxies join only when the agent transport is on.
 */
export function rustServiceEnvironment(
  env: NodeJS.ProcessEnv = process.env,
  windows = process.platform === 'win32',
  agentTransport = false
): NodeJS.ProcessEnv {
  const names = agentTransport
    ? [...RUST_SERVICE_ENV_NAMES, ...RUST_AGENT_ENV_NAMES]
    : RUST_SERVICE_ENV_NAMES;
  const allowed = new Set(windows ? names.map((name) => name.toUpperCase()) : names);
  const chosen: NodeJS.ProcessEnv = {};
  for (const [name, value] of Object.entries(env)) {
    const key = windows ? name.toUpperCase() : name;
    if (value === undefined || !allowed.has(key)) continue;
    allowed.delete(key);
    chosen[name] = value;
  }
  return chosen;
}
