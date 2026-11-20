/** Help for the saved Codex logins whose usage the dashboard reads. */
export function printCodexAuthHelp(): void {
  process.stdout.write(`AI Account Center Codex Login Management

Usage
  ai-account-center codex-auth <command> [options]
  ccs codex-auth <command> [options]

Commands
  import-default <name>  Save the existing ~/.codex/auth.json as a named login

Managed in the dashboard's Accounts page (the CLI points there):
  Add (create), Sign in again (login), the saved-login list (show/list/status), Remove

Examples
  ai-account-center codex-auth import-default work

Options
  --force                Overwrite a saved login (import-default)
  --with-history         Include history.jsonl and sessions when importing (default: off)
  --force-while-running  Allow import while Codex is running

Notes
  Saved auth files remain in the existing account configuration directory.
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
