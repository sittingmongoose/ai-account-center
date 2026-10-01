import { prepareCliArguments } from './commands/cli-bootstrap';

/** Select the configuration path before invocation logging or command loads. */
async function runCli(): Promise<void> {
  const bootstrap = await prepareCliArguments(process.argv.slice(2));
  if (bootstrap.exitNow) return;
  const { requiresRuntimeServices, tryHandleRootCommand } = await import(
    './commands/root-command-router'
  );
  // Help, version and migration messages must not load or upgrade private configuration.
  if (!(await requiresRuntimeServices(bootstrap.args))) {
    await tryHandleRootCommand(bootstrap.args);
    return;
  }
  const [{ handleError, runCleanup }, { createLogger, runWithRequestId }, { redactArgv }] =
    await Promise.all([
      import('./errors'),
      import('./services/logging'),
      import('./services/logging/log-redaction'),
    ]);

  process.on('uncaughtException', (error: Error) => handleError(error));
  process.on('unhandledRejection', (reason: unknown) => handleError(reason));
  for (const [signal, exitCode] of [
    ['SIGTERM', 143],
    ['SIGINT', 130],
  ] as const) {
    process.on(signal, () => {
      try {
        runCleanup();
      } catch {
        // Cleanup failure should not block termination.
      }
      if (process.listenerCount(signal) <= 1) process.exit(exitCode);
    });
  }

  const startedAt = Date.now();
  const logger = createLogger('cli:entry');
  await runWithRequestId(async () => {
    logger.stage('intake', 'cli.command.start', 'CLI invocation started', {
      argv: redactArgv(process.argv.slice(2)),
    });
    try {
      if (!bootstrap.exitNow) {
        const { applyGlobalFetchProxy } = await import('./utils/fetch-proxy-setup');
        const fetchProxySetup = applyGlobalFetchProxy();
        if (fetchProxySetup.error) {
          console.error(`[!] Skipping global fetch proxy setup: ${fetchProxySetup.error}`);
        }
        await tryHandleRootCommand(bootstrap.args);
      }
      logger.stage(
        'respond',
        'cli.command.complete',
        'CLI invocation completed',
        { exitCode: process.exitCode ?? 0 },
        { latencyMs: Date.now() - startedAt }
      );
    } catch (err) {
      const error =
        err instanceof Error
          ? { name: err.name, message: err.message, stack: err.stack }
          : { name: 'Error', message: String(err) };
      logger.stage('cleanup', 'cli.command.failed', 'CLI invocation failed', undefined, {
        level: 'error',
        latencyMs: Date.now() - startedAt,
        error,
      });
      handleError(err);
    }
  });
}

void runCli().catch((error: unknown) => {
  console.error(`[X] ${error instanceof Error ? error.message : String(error)}`);
  process.exitCode = 1;
});
