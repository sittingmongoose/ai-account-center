import { describe, expect, test } from 'bun:test';
import * as fs from 'node:fs';
import * as path from 'node:path';

const workflowDirectory = path.resolve(import.meta.dir, '../../../../.github/workflows');
// This pure Compose diff check must cover fork PRs without executing their source.
const forkSafeWorkflows = new Set(['breaking-change-guard.yml']);

function readWorkflow(file: string) {
  return fs.readFileSync(path.join(workflowDirectory, file), 'utf8');
}

function activeWorkflowFiles() {
  return fs.readdirSync(workflowDirectory).filter((file) => /\.ya?ml$/.test(file));
}

function withoutComments(workflow: string) {
  return workflow.replace(/^\s*#.*$/gm, '');
}

function jobBlocks(workflow: string) {
  const jobs = workflow.slice(workflow.indexOf('\njobs:\n') + '\njobs:\n'.length);
  return [...jobs.matchAll(/^ {2}([\w-]+):\s*\n([\s\S]*?)(?=^ {2}[\w-]+:\s*\n|$(?![\s\S]))/gm)].map(
    ([, name, source]) => ({ name, source })
  );
}

describe('workflow execution safety', () => {
  test('uses runnable hosted Linux CI and preserves the existing manual Mac runner', () => {
    const workflowFiles = activeWorkflowFiles();
    expect(workflowFiles.length).toBeGreaterThan(0);

    for (const file of workflowFiles) {
      const workflow = withoutComments(readWorkflow(file));
      const jobs = jobBlocks(workflow);
      expect(jobs.length, `${file} must expose its jobs to the safety check`).toBeGreaterThan(0);

      for (const { name, source } of jobs) {
        if (file === 'bar-release.yml') {
          expect(source, `${file}:${name} must keep the existing Mac packaging runner`).toMatch(
            /^\s*runs-on:.*\bself-hosted\b.*\bmacos\b/m
          );
          continue;
        }
        expect(source, `${file}:${name} must have an available hosted Linux runner`).toMatch(
          /^\s*runs-on: ubuntu-latest\s*$/m
        );
        expect(source).not.toContain('self-hosted');
      }
    }
  });

  test('each PR job that executes checked-out source keeps its trusted-author gate', () => {
    const trustedAuthorGate =
      'contains(fromJSON(\'["COLLABORATOR","MEMBER","OWNER"]\'), github.event.pull_request.author_association)';

    for (const file of activeWorkflowFiles()) {
      const workflow = withoutComments(readWorkflow(file));
      const triggers = workflow.split('\njobs:\n')[0];
      if (/^ {2}pull_request_target:/m.test(triggers)) {
        expect(workflow, `${file} must not check out source with pull_request_target`).not.toMatch(
          /^\s*uses: actions\/checkout@/m
        );
      }
      if (!/^ {2}pull_request:/m.test(triggers) || forkSafeWorkflows.has(file)) continue;

      for (const { name, source } of jobBlocks(workflow)) {
        if (!/^\s*uses: actions\/checkout@/m.test(source)) {
          continue;
        }
        const jobSettings = source.split('\n    steps:')[0];
        expect(jobSettings, `${file}:${name} must gate checkout at the job level`).toContain(
          trustedAuthorGate
        );
      }
    }
  });

  test('the universal Compose guard parses the contract without running untrusted source', () => {
    const workflow = withoutComments(readWorkflow('breaking-change-guard.yml'));

    expect(workflow).toContain('persist-credentials: false');
    expect(workflow.match(/^\s*uses: (.+)$/gm)).toEqual([
      '        uses: actions/checkout@v4',
    ]);
    expect(workflow).not.toMatch(
      /\b(?:bun|npm|yarn|pnpm|cargo|wasm-pack|node|python\d*|bash|sh|curl|wget)\s/
    );
    expect(workflow).toContain('OLD_RAW=$(service_field_value "$BASE_COMPOSE" ccs image)');
    expect(workflow).toContain('NEW_RAW=$(service_field_value docker/compose.yaml ccs image)');
    expect(workflow).toContain('has_service_network_effective_name docker/compose.yaml ccs ccs-net');
    expect(workflow).toContain('OLD_CN=$(service_field_value "$BASE_COMPOSE" ccs container_name');
  });

  test('manual Mac packaging checks out the fork default branch without publishing access', () => {
    const workflow = withoutComments(readWorkflow('bar-release.yml'));
    const triggers = workflow.split('\npermissions:')[0];
    const packageJob = jobBlocks(workflow).find(({ name }) => name === 'package');

    expect(triggers).toMatch(/^ {2}workflow_dispatch:/m);
    expect(triggers).not.toMatch(/^ {2}(?:push|pull_request(?:_target)?|release|schedule):/m);
    expect(packageJob).toBeDefined();
    expect(packageJob!.source.split('\n    steps:')[0]).toContain(
      "github.repository == 'sittingmongoose/ai-account-center' && github.ref_name == github.event.repository.default_branch"
    );
    expect(workflow).toContain('ref: ${{ github.event.repository.default_branch }}');
    expect(workflow).toContain('persist-credentials: false');
    expect(workflow).toContain('permissions:\n  contents: read');
    expect(workflow).not.toMatch(/^\s*[\w-]+:\s*write\s*$/m);
    expect(workflow).not.toMatch(/secrets\.|create-github-app-token|PAT_TOKEN/);
    expect(workflow).not.toMatch(
      /action-gh-release|build-push-action|\bgh release\b|\b(?:npm|bun) publish\b|\bdocker\s+(?:buildx\s+)?push\b|\bwrangler\b|\bgit push\b/
    );
    expect(workflow).toContain('uses: actions/upload-artifact@v4');
    expect(workflow).toContain('path: macos-bar/dist/AI-Account-Center.app.zip');
  });
});
