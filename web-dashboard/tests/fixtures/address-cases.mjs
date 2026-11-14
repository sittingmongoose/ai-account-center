// The address table of lib.rs `the_classic_addresses_open_the_same_tabs`, read from the Rust source, so every
// JavaScript copy of the rule (index.html's early module load, the /bridge.js upgrade module) is held to the same
// cases as the frame itself.
import { readFileSync } from 'node:fs';

export function addressCases() {
  const source = readFileSync(new URL('../../leptos/src/lib.rs', import.meta.url), 'utf8');
  const start = source.indexOf('fn the_classic_addresses_open_the_same_tabs');
  const block = source.slice(start, source.indexOf('] {', start));
  const cases = [...block.matchAll(/\("([^"]*)", "([^"]*)", Page::(\w+)\)/g)].map(([, path, search, page]) => [
    path,
    search,
    page.toLowerCase(),
  ]);
  if (cases.length < 20) throw new Error('lib.rs address cases not found');
  return cases;
}
