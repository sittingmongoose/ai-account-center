/** Help for the saved Codex logins whose usage the dashboard reads. */
export function printCodexAuthHelp(): void {
  process.stdout.write(`AI Account Center Codex Login Management

Usage
  ai-account-center codex-auth <command> [options]
  ccs codex-auth <command> [options]

Commands
  create <name>          Create a saved Codex login (idempotent)
  login <name>           Run codex login for the saved login (auto-creates if missing)
  show [name]            List saved logins or show details for one (aliases: list, status)
  remove <name>          Delete a saved login after confirmation
  import-default <name>  Save the existing ~/.codex/auth.json as a named login

Examples
  ai-account-center codex-auth show
  ai-account-center codex-auth login work
  ai-account-center codex-auth remove old --yes

Options
  --yes, -y              Skip confirmation for removal
  --force                Repair config link (create), override saved default protection
                         (remove), or overwrite a saved login (import-default)
  --json                 JSON output (show)
  --with-history         Include history.jsonl and sessions when importing (default: off)
  --force-while-running  Allow import while Codex is running

Notes
  Saved auth files remain in the existing account configuration directory.
  remove always protects the current shared login, including with --yes or --force.
  Unverifiable saved identities are retained while a native login exists.
  The dashboard reads each saved login's usage and limits and keeps it signed in.
  AI Account Center no longer switches the shared ~/.codex login (activate, use and
  switch are retired); your agents choose their own accounts.
`);
}

/** stderr only keeps a retired `use` harmless inside an old shell eval. */
export function printRetiredCodexAuthCommand(command: string): void {
  process.stderr.write(`[X] codex-auth ${command} is retired in AI Account Center.\n`);
  process.stderr.write(
    '    Account switching was removed; AI Account Center only reads usage and limits.\n'
  );
  process.stderr.write('    No CODEX_HOME exports or persistent profile changes were made.\n');
}
