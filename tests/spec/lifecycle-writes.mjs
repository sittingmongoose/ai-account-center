import { execFileSync } from 'node:child_process';
import path from 'node:path';

const tsOnly = process.argv.includes('--ts-only');
const binary = process.env.AAC_RUST_SERVICE_BINARY ?? path.resolve('target/debug/aac-http');
const args = ['test', 'tests/unit/web-server/account-lifecycle-routes.test.ts'];
if (process.platform === 'win32') {
  args.push(
    '--test-name-pattern',
    'registered lifecycle state write parity|needs a browser session|replaces a key|re-checks a pending|purges one trash|reads recovered sign-in jobs'
  );
}
for (const mode of tsOnly ? ['off', 'shadow'] : ['off', 'shadow', 'on']) {
  execFileSync('bun', args, {
    stdio: 'inherit',
    env: {
      ...process.env,
      AAC_SPEC_LIFECYCLE_WRITE_MODE: mode,
      AAC_RUST_GROUP_WRITES: 'on',
      AAC_AGENT_TRANSPORT: 'off',
      AAC_AGENT_VERBS: 'off',
      AAC_STORE: 'off',
      AAC_RUST_SERVICE_BINARY: binary,
      AAC_SPEC_APPROVAL_BINARY: mode === 'on' ? binary : '',
    },
  });
}
console.log(
  'registered lifecycle state write parity: ' + (tsOnly ? 'off/shadow' : 'off/shadow/on')
);
