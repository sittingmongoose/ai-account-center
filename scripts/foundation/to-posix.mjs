import { sep } from 'node:path';

// Wire tables and package.json ship lists record source paths in POSIX form on every platform (tool-issues 58/59).
export const toPosix = p => p.split(sep).join('/');
