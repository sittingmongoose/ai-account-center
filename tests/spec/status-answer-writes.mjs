import { execFileSync } from 'node:child_process';
import path from 'node:path';

const fixture = 'takes an answer set only|shows a send summary that cannot|shows a written response|says nothing about a send summary';
const modes = process.argv.includes('--ts-only') ? ['off', 'shadow'] : ['off', 'shadow', 'on'];
for (const mode of modes) {
  execFileSync(
    'bun',
    ['test', 'tests/unit/web-server/status/status-routes.test.ts', '--test-name-pattern', fixture],
    {
      stdio: 'inherit',
      env: {
        ...process.env,
        AAC_SPEC_STATUS_ANSWER_WRITE_MODE: mode,
        AAC_RUST_GROUP_WRITES: 'on',
        AAC_STORE: 'off',
        AAC_RUST_SERVICE_BINARY:
          process.env.AAC_RUST_SERVICE_BINARY ?? path.resolve('target/debug/aac-http'),
      },
    }
  );
}
console.log('Status answer HTTP, replay and delivery fixtures: ' + modes.join('/'));
