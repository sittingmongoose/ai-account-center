import { describe, expect, test } from 'bun:test';
import * as fs from 'node:fs';
import * as path from 'node:path';

const repoRoot = path.resolve(import.meta.dir, '../../../..');
const codeFile = /\.(c|m)?[jt]s$/;

function collect(directory: string, files: string[] = []): string[] {
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    if (entry.name === 'node_modules') continue;
    const fullPath = path.join(directory, entry.name);
    if (entry.isDirectory()) collect(fullPath, files);
    else if (codeFile.test(entry.name)) files.push(fullPath);
  }
  return files;
}

function testSources(): string[] {
  const underSrc = collect(path.join(repoRoot, 'src')).filter((file) =>
    file.split(path.sep).includes('__tests__')
  );
  return [...collect(path.join(repoRoot, 'tests')), ...underSrc];
}

describe('tests run on a GitHub-hosted Ubuntu runner', () => {
  // GitHub installs Node with `n` under /usr/local and setup-node adds its own copy from the
  // tool cache; there is no /usr/bin/node there. A test that copied it failed only on GitHub,
  // and the bucket runner hid the failure. Use process.execPath (the runtime running the test)
  // or a PATH lookup instead.
  test('test code never relies on /usr/bin/node', () => {
    const systemNode = new RegExp(`['"\`]${['', 'usr', 'bin', 'node'].join('/')}(js)?['"\`]`);
    const offenders = testSources()
      .filter((file) => systemNode.test(fs.readFileSync(file, 'utf8')))
      .map((file) => path.relative(repoRoot, file));

    expect(offenders).toEqual([]);
  });
});
