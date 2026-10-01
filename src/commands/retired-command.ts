export function showRetiredCommandMessage(command: string, writeLine = console.error): void {
  if (command === 'update' || command === '--update') {
    writeLine('[X] Upstream CCS self-update is retired in AI Account Center.');
    writeLine('    Update AI Account Center from its own release or checkout.');
    writeLine('    Installed application updates remain available in the dashboard.');
  } else {
    writeLine(`[X] "${command}" is an unsupported command or retired upstream CCS workflow.`);
    writeLine(
      '    AI Account Center manages accounts and usage; profile/model CLI launches are retired.'
    );
  }
  writeLine('    Use ai-account-center dashboard, codex-auth, bar, or --help.');
  process.exitCode = 1;
}
