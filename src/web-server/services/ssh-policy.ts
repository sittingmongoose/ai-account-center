/**
 * One SSH policy for every dashboard transport (TARGET-DESIGN section 12).
 *
 * Every transport builds its argv here; none spells out ssh options itself.
 * The fixed rules, shared by all of them:
 *
 * - `-T`: no PTY allocation (the Mac sshd refuses PTYs; nothing needs one).
 * - `BatchMode=yes`: never prompt for a password or passphrase; a host that
 *   needs one fails instead of hanging.
 * - `ConnectTimeout=5`, `ConnectionAttempts=1`: one bounded connect attempt.
 * - Host keys verify against the user's default known_hosts. No transport
 *   sets StrictHostKeyChecking or UserKnownHostsFile, and none may: a new
 *   host key is a refusal, never an automatic accept.
 * - `--` before the alias, so an alias can never parse as an option, and
 *   exactly one remote command argument after it. Command quoting stays
 *   with each transport's own command builder (fixed paths, enumerated
 *   values, shell-specific quoting helpers).
 *
 * Keepalives are the one per-transport choice, sized to each job:
 *
 * | Transport | Interval | CountMax |
 * | --- | ---: | ---: |
 * | analytics-remote-transport | 5 | 6 |
 * | app-update-hosts (update run) | 10 | 2 |
 * | app-update-hosts (helper query/push) | none | none |
 * | claude-desktop-live-service | 5 | 1 |
 * | claude-desktop-transport | 5 | 1 |
 * | claude-host-transport | 5 | 1 |
 * | account-key-store | 5 | 1 |
 * | muse-account-lifecycle | 5 | 1 |
 * | opencode-console-wallet-service | 5 | 1 |
 * | signin-health-service | 5 | 1 |
 * | additional-usage-transport | 5 | 1 |
 * | usage-failover-transport | 5 | 3 |
 * | status/status-collectors (machine probe, pins) | 5 | 2 |
 *
 * Process timeouts and output caps stay with each transport: they are
 * per-action process budgets, not ssh options. Alias validation stays at
 * the call sites too, each with its own error type, using the one grammar
 * `isSafeUsageSshAlias` in dashboard-hosts.ts.
 */

/** The keepalive pair; omit it for jobs that run without one (helper sync). */
export interface SshKeepalive {
  interval: number;
  countMax: number;
}

/** The full ssh argv for one remote command: fixed options, keepalive, `--`, alias, command. */
export function sshArgv(host: string, command: string, keepalive?: SshKeepalive): string[] {
  return [
    '-T',
    '-o',
    'BatchMode=yes',
    '-o',
    'ConnectTimeout=5',
    '-o',
    'ConnectionAttempts=1',
    ...(keepalive
      ? [
          '-o',
          `ServerAliveInterval=${keepalive.interval}`,
          '-o',
          `ServerAliveCountMax=${keepalive.countMax}`,
        ]
      : []),
    '--',
    host,
    command,
  ];
}
