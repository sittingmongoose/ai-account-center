import { describe, expect, test } from 'bun:test';
import * as fs from 'node:fs';
import * as path from 'node:path';

const workflowDirectory = path.resolve(import.meta.dir, '../../../../.github/workflows');

describe('provider review automation retirement', () => {
  test('does not retain the inherited AI review workflow', () => {
    expect(fs.existsSync(path.join(workflowDirectory, 'ai-review.yml'))).toBe(false);
  });

  test('active workflows do not invoke provider review actions or inherited review credentials', () => {
    const inheritedIntegration =
      /qodo-ai\/pr-agent|anthropics\/claude-code-action|CCS_REVIEWER_|AI_REVIEW_(?:API_KEY|BASE_URL|MODEL|REASONING_EFFORT)|ccs-reviewer\[bot\]/;
    const workflowFiles = fs
      .readdirSync(workflowDirectory)
      .filter((file) => /\.ya?ml$/.test(file));

    expect(workflowFiles.length).toBeGreaterThan(0);
    for (const file of workflowFiles) {
      const workflow = fs.readFileSync(path.join(workflowDirectory, file), 'utf8');
      expect(workflow, `${file} must not restore the inherited provider review integration`).not.toMatch(
        inheritedIntegration
      );
    }
  });
});
