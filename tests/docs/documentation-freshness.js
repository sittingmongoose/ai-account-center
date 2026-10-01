#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

const root = path.resolve(__dirname, '../..');
const failures = [];

function read(relativePath) {
  return fs.readFileSync(path.join(root, relativePath), 'utf8');
}

function requireText(relativePath, expected) {
  if (!fs.existsSync(path.join(root, relativePath))) {
    failures.push(`${relativePath} is missing`);
    return;
  }
  if (!read(relativePath).includes(expected)) {
    failures.push(`${relativePath} is missing: ${expected}`);
  }
}

function collectFiles(directory, filePattern) {
  const files = [];
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const absolutePath = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...collectFiles(absolutePath, filePattern));
    } else if (entry.isFile() && filePattern.test(entry.name)) {
      files.push(absolutePath);
    }
  }
  return files;
}

const removedGuides = [
  'docs/ccs-bar.md',
  'docs/cursor-integration.md',
  'docs/dashboard-auth-cli.md',
  'docs/session-sharing-technical-analysis.md',
  'docs/browser-automation.md',
  'docs/openai-compatible-providers.md',
  'docs/image-analysis.md',
  'docs/websearch.md',
  'docs/system-architecture/target-adapters.md',
];

for (const relativePath of removedGuides) {
  if (fs.existsSync(path.join(root, relativePath))) {
    failures.push(`${relativePath} is retired; maintain the current product guides instead`);
  }
}

const retainedPaths = [
  'LICENSE',
  'docs/README.md',
  'docs/codebase-summary.md',
  'docs/code-standards.md',
  'docs/project-overview-pdr.md',
  'docs/project-roadmap.md',
  'docs/release-process.md',
  'docs/codex-auth.md',
  'docs/logging-contract.md',
  'docs/activate-in-place.md',
  'docs/system-architecture/index.md',
  'docs/system-architecture/provider-flows.md',
  'src/commands/command-catalog.ts',
  'src/web-server/index.ts',
  'src/web-server/middleware/auth-middleware.ts',
  'src/utils/config-manager.ts',
  'src/codex-auth/codex-auth-help.ts',
  'web-dashboard/Cargo.toml',
  'web-dashboard/Cargo.lock',
  'web-dashboard/ui/dashboard.slint',
  'web-dashboard/public/bridge.js',
  'macos-bar/Package.swift',
  'macos-bar/Scripts/package_app.sh',
  'macos-bar/Scripts/install_user.sh',
  'windows-bar/CCSBar/CCSBar.csproj',
  'windows-bar/scripts/Build.ps1',
  'windows-bar/scripts/Install.ps1',
  'docker/Dockerfile',
  'docker/compose.yaml',
  'docker/entrypoint.sh',
];
for (const relativePath of retainedPaths) {
  if (!fs.existsSync(path.join(root, relativePath))) {
    failures.push(`${relativePath} is missing from the retained product`);
  }
}

for (const expected of [
  'AI Account Center',
  'sittingmongoose/ai-account-center',
  'ai-account-center dashboard',
  'ccs config',
  '~/.ccs/',
  'CCS_HOME',
  'session aliases',
  'Tam Nhu Tran',
  'Copyright (c) 2025 CCS Contributors',
]) {
  requireText('README.md', expected);
}
requireText('LICENSE', 'Copyright (c) 2025 CCS Contributors');
requireText('docs/README.md', '../web-dashboard/README.md');
requireText('docs/release-process.md', '.github/workflows/bar-release.yml');
requireText('docs/codex-auth.md', 'src/codex-auth/codex-auth-help.ts');
requireText('docs/system-architecture/index.md', 'src/utils/config-manager.ts');
requireText('docs/system-architecture/index.md', 'web-dashboard/public/bridge.js');
requireText('web-dashboard/README.md', '=1.18.1');
requireText('web-dashboard/README.md', 'AboutSlint');
requireText('web-dashboard/README.md', 'dist/ui/');
requireText('macos-bar/README.md', './Scripts/package_app.sh');
requireText('macos-bar/README.md', './Scripts/install_user.sh');
requireText('macos-bar/README.md', '~/.ccs/bar');
requireText('windows-bar/README.md', './scripts/Build.ps1');
requireText('windows-bar/README.md', 'connection.dpapi');
requireText('docker/README.md', 'ai-account-center:local');
requireText('docker/README.md', '127.0.0.1');
requireText('docker/README.md', '/home/node/.ccs');
requireText('docker/README.md', 'CCS_SESSION_SECRET');
requireText('.github/ISSUE_TEMPLATE/documentation.yml', 'docs/README.md');
requireText('.github/ISSUE_TEMPLATE/config.yml', 'sittingmongoose/ai-account-center');

const practicalGuidanceFiles = [
  path.join(root, 'README.md'),
  path.join(root, 'CLAUDE.md'),
  path.join(root, 'CONTRIBUTING.md'),
  path.join(root, 'SECURITY.md'),
  path.join(root, 'docker', 'README.md'),
  path.join(root, 'macos-bar', 'README.md'),
  ...collectFiles(path.join(root, 'docs'), /\.mdx?$/),
  ...collectFiles(path.join(root, '.github', 'ISSUE_TEMPLATE'), /\.(md|ya?ml)$/),
];

for (const staleGuidePath of removedGuides) {
  for (const guidancePath of practicalGuidanceFiles) {
    if (read(path.relative(root, guidancePath)).includes(staleGuidePath)) {
      failures.push(
        `${path.relative(root, guidancePath)} references deleted guide: ${staleGuidePath}`
      );
    }
  }
}

const markdownFiles = [
  path.join(root, 'README.md'),
  path.join(root, 'CLAUDE.md'),
  path.join(root, 'CONTRIBUTING.md'),
  path.join(root, 'SECURITY.md'),
  path.join(root, 'tests', 'README.md'),
  path.join(root, 'tests', 'npm', 'README.md'),
  path.join(root, 'web-dashboard', 'README.md'),
  path.join(root, 'docker', 'README.md'),
  path.join(root, 'macos-bar', 'README.md'),
  path.join(root, 'windows-bar', 'README.md'),
  ...collectFiles(path.join(root, 'docs'), /\.mdx?$/),
];
const linkPattern = /!?\[[^\]]*]\(([^)]+)\)/g;

for (const markdownPath of markdownFiles) {
  const source = fs.readFileSync(markdownPath, 'utf8');
  for (const match of source.matchAll(linkPattern)) {
    let target = match[1].trim().replace(/^<|>$/g, '');
    if (!target || target.startsWith('#') || /^(https?:|mailto:|tel:)/i.test(target)) {
      continue;
    }

    target = target.split('#', 1)[0].split('?', 1)[0];
    try {
      target = decodeURIComponent(target);
    } catch {
      failures.push(`${path.relative(root, markdownPath)} has invalid link encoding: ${match[1]}`);
      continue;
    }

    const resolved = path.resolve(path.dirname(markdownPath), target);
    if (!fs.existsSync(resolved)) {
      failures.push(`${path.relative(root, markdownPath)} has missing relative link: ${match[1]}`);
    }
  }
}

if (failures.length > 0) {
  console.error('[X] Documentation freshness checks failed:');
  for (const failure of failures) {
    console.error(`    ${failure}`);
  }
  process.exit(1);
}

console.log('[OK] Documentation pointers, retained contracts, and relative links are current.');
