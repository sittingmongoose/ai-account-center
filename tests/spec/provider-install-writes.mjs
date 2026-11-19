import { execFileSync } from 'node:child_process';
import path from 'node:path';

const modes = process.argv.includes('--ts-only') ? ['off', 'shadow'] : ['off', 'shadow', 'on'];
const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.resolve('target/debug/aac-http');
for (const mode of modes) {
  const files = ['tests/unit/web-server/codex-account-lifecycle.test.ts'];
  if (process.platform !== 'win32')
    files.push(
      'tests/unit/web-server/antigravity-signin-flow.test.ts',
      'tests/unit/web-server/account-key-sweep.test.ts',
      'tests/unit/web-server/claude-account-lifecycle.test.ts'
    );
  for (const file of files) {
    execFileSync('bun', ['test', file], {
      stdio: 'inherit',
      env: {
        ...process.env,
        AAC_SPEC_PROVIDER_WRITE_MODE: mode,
        AAC_RUST_SERVICE_BINARY: binary,
        AAC_AGENT_VERBS: 'off',
        AAC_STORE: 'off',
      },
    });
  }
}
console.log('provider install writes: ' + modes.join('/'));
