export interface RootCommandEntry {
  name: string;
  summary: string;
  visibility: 'public' | 'hidden';
  aliases?: readonly string[];
}

export const ROOT_COMMAND_CATALOG: readonly RootCommandEntry[] = [
  {
    name: 'dashboard',
    summary: 'Open the account and usage dashboard',
    aliases: ['config'],
    visibility: 'public',
  },
  {
    name: 'codex-auth',
    summary: 'Save Codex logins and activate the shared login safely',
    visibility: 'public',
  },
  {
    name: 'bar',
    summary: 'Install, launch, or inspect the native account bar',
    visibility: 'public',
  },
  { name: 'help', summary: 'Show command help', aliases: ['--help', '-h'], visibility: 'public' },
  {
    name: 'version',
    summary: 'Show version and installation paths',
    aliases: ['--version', '-v'],
    visibility: 'public',
  },
];

export function getPublicRootCommands(): RootCommandEntry[] {
  return ROOT_COMMAND_CATALOG.filter((entry) => entry.visibility === 'public');
}
