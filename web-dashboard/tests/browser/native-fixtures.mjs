/** Synchronous JS-module adapter for native fixtures. Set AAC_XTASK to a built aac-xtask. */
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
function write(kind, root, options = {}) {
  const binary =
    process.env.AAC_XTASK ??
    path.join(repo, 'target/debug', process.platform === 'win32' ? 'aac-xtask.exe' : 'aac-xtask');
  const args = ['fixture', kind, path.resolve(root), '--json'];
  if (options.now !== undefined) args.push('--now', String(options.now));
  if (kind === 'status') {
    if (options.ccsDir) args.push('--ccs-dir', path.resolve(options.ccsDir));
    if (options.withKinds) args.push('--with-kinds');
    if (options.withProblems) args.push('--with-problems');
    if (!options.withVideo) args.push('--no-video');
    if (options.withQuota === false) args.push('--no-quota');
  }
  const result = spawnSync(binary, args, {
    encoding: 'utf8',
    env: { ...process.env, CCS_DIR: '', CCS_HOME: '' },
    maxBuffer: 8 * 1024 * 1024,
  });
  if (result.error) throw result.error;
  if (result.status !== 0)
    throw new Error(result.stderr.trim() || `aac-xtask fixture exited ${result.status}`);
  return JSON.parse(result.stdout);
}
export function writeStatusFixture(root, options = {}) {
  return write('status', root, options);
}
export function writeScoreboardFixture(root, options = {}) {
  return write('scoreboard', root, options);
}
