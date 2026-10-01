import { describe, expect, test } from 'bun:test';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';

const repoRoot = resolve(import.meta.dir, '../../../..');
const workflowDir = resolve(repoRoot, '.github/workflows');

describe('product release boundary', () => {
  test('does not retain inherited package, deployment, project or webhook workflows', () => {
    const retiredWorkflows = [
      'release.yml',
      'dev-release.yml',
      'docker-release.yml',
      'promote-release.yml',
      'publish-npm.yml.deprecated',
      'deploy-ccs-worker.yml',
      'smoke-test-compose-url.yml',
      'sync-dev-after-release.yml',
      'sync-ccs-backlog-project.yml',
      'label-pending-release.yml',
    ];

    for (const file of retiredWorkflows) {
      expect(existsSync(resolve(workflowDir, file))).toBe(false);
    }

    for (const file of readdirSync(workflowDir).filter((file) => /\.ya?ml$/.test(file))) {
      const workflow = readFileSync(resolve(workflowDir, file), 'utf8');
      expect(workflow).not.toMatch(/npm publish|semantic-release|gh release|docker\/build-push-action/);
      expect(workflow).not.toMatch(/PAT_TOKEN|NPM_TOKEN|DISCORD_WEBHOOK_URL|CLOUDFLARE_API_TOKEN/);
      expect(workflow).not.toMatch(/kaitranntt|ccs\.kaitran\.ca|(?:contents|packages|id-token): write/);
    }
  });

  test('does not configure or invoke package publication from the product package', () => {
    for (const file of ['.releaserc', '.releaserc.cjs', '.releaserc.js', 'release.config.js']) {
      expect(existsSync(resolve(repoRoot, file))).toBe(false);
    }
    const packageJson = JSON.parse(readFileSync(resolve(repoRoot, 'package.json'), 'utf8'));
    const packageScripts = Object.values(packageJson.scripts).join('\n');
    expect(packageScripts).not.toMatch(/npm publish|semantic-release|gh release|wrangler deploy/);
    expect(packageJson.release).toBeUndefined();
  });
});
