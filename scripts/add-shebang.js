#!/usr/bin/env node
'use strict';

const fs = require('fs');
const path = require('path');

function getExecutablePaths() {
  return [path.join(__dirname, '../dist/ccs.js')];
}

/**
 * Add a shebang to the retained reference CLI. Published bins are native.
 * Run after: tsc
 */
function addShebang() {
  const executablePaths = getExecutablePaths();
  const missingPaths = executablePaths.filter((executablePath) => !fs.existsSync(executablePath));
  if (missingPaths.length > 0) {
    console.error(`[X] Missing built executable(s): ${missingPaths.join(', ')}. Run tsc first.`);
    process.exit(1);
  }

  for (const executablePath of executablePaths) {
    let content = fs.readFileSync(executablePath, 'utf8');

    if (!content.startsWith('#!/usr/bin/env node')) {
      content = '#!/usr/bin/env node\n' + content;
      fs.writeFileSync(executablePath, content);
      console.log(
        `[OK] Shebang added to ${path.relative(path.join(__dirname, '..'), executablePath)}`
      );
    }

    if (process.platform !== 'win32') {
      try {
        fs.chmodSync(executablePath, 0o755);
        console.log(
          `[OK] ${path.relative(path.join(__dirname, '..'), executablePath)} is now executable`
        );
      } catch (err) {
        console.warn(
          `[!] Could not chmod ${path.relative(path.join(__dirname, '..'), executablePath)}: ${err.message}`
        );
      }
    }
  }
}

addShebang();
