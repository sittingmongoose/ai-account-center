/** Retained legacy bin names provide migration guidance without launching old engines. */
import * as path from 'path';

const invokedName = path
  .basename(process.argv[1] || 'legacy CCS alias')
  .replace(/\.(?:js|ts)$/, '');
console.error(`[X] ${invokedName} is a retired upstream CCS launcher.`);
console.error('    Use ai-account-center dashboard for accounts and usage.');
console.error('    Use ai-account-center codex-auth show to inspect saved Codex logins.');
console.error(
  '    Use ai-account-center codex-auth activate <saved-login> to activate the shared login.'
);
console.error(
  '    Shell exports, isolated CODEX_HOME launches, Droid routing, and CLIProxy launches are retired.'
);
process.exitCode = 1;
