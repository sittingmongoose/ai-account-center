import { randomUUID } from 'crypto';
import * as fs from 'fs';
import * as path from 'path';

const MAX_BYTES = 256 * 1024;
const MAX_OBSERVATIONS = 256;
const OBSERVATION_NAME = /^([a-f0-9]{64})\.json$/;
function owner(stat: fs.Stats): boolean {
  return (
    process.platform === 'win32' || (stat.uid === process.getuid?.() && (stat.mode & 0o077) === 0)
  );
}
function directory(stat: fs.Stats): boolean {
  return stat.isDirectory() && !stat.isSymbolicLink() && owner(stat);
}
function regular(stat: fs.Stats): boolean {
  return (
    stat.isFile() &&
    !stat.isSymbolicLink() &&
    stat.nlink === 1 &&
    stat.size <= MAX_BYTES &&
    owner(stat)
  );
}
function sameFile(a: fs.Stats, b: fs.Stats): boolean {
  return a.ino === b.ino && a.dev === b.dev;
}

function readPrivateCache(
  file: string,
  expectedParent?: fs.Stats
): { value: unknown; stat: fs.Stats } | null {
  let descriptor: number | undefined;
  try {
    const before = fs.lstatSync(path.dirname(file));
    if (!directory(before) || (expectedParent && !sameFile(before, expectedParent))) return null;
    const link = fs.lstatSync(file);
    if (!regular(link)) return null;
    descriptor = fs.openSync(file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
    const stat = fs.fstatSync(descriptor);
    if (!regular(stat) || !sameFile(link, stat)) return null;
    const buffer = Buffer.alloc(MAX_BYTES + 1);
    const length = fs.readSync(descriptor, buffer, 0, buffer.length, 0);
    const after = fs.lstatSync(path.dirname(file));
    if (length > MAX_BYTES || !directory(after) || !sameFile(before, after)) return null;
    return { value: JSON.parse(buffer.subarray(0, length).toString('utf8')), stat };
  } catch {
    return null;
  } finally {
    if (descriptor !== undefined) fs.closeSync(descriptor);
  }
}

/** Private observation data is bounded and cannot follow links or another owner's cache. */
export function readUsageCache(file: string): unknown {
  return readPrivateCache(file)?.value ?? null;
}

function observation(value: unknown, key: string): boolean {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const record = value as Record<string, unknown>;
  return (
    record.version === 1 &&
    record.key === key &&
    ['identity', 'primary', 'saved'].every(
      (field) =>
        record[field] === null ||
        (typeof record[field] === 'object' && !Array.isArray(record[field]))
    ) &&
    typeof record.nextAttempt === 'number' &&
    Number.isFinite(record.nextAttempt) &&
    (record.primaryFailure === undefined ||
      record.primaryFailure === null ||
      typeof record.primaryFailure === 'string') &&
    (record.observedPlan === undefined ||
      record.observedPlan === null ||
      typeof record.observedPlan === 'string') &&
    Object.keys(record).every((field) =>
      [
        'version',
        'key',
        'identity',
        'primary',
        'saved',
        'nextAttempt',
        'primaryFailure',
        'primarySignIn',
        'observedPlan',
      ].includes(field)
    )
  );
}

/** Bound credential-revision history without touching other private cache formats. */
function pruneObservations(current: string, parent: fs.Stats): void {
  if (!OBSERVATION_NAME.test(path.basename(current))) return;
  const root = path.dirname(current);
  const unchangedParent = () => {
    const stat = fs.lstatSync(root);
    return directory(stat) && sameFile(stat, parent);
  };
  if (!unchangedParent()) return;
  // Normal refreshes replace existing files. Avoid reading every cached payload
  // unless credential/source churn could have grown the observation set.
  const names = fs.opendirSync(root);
  let possible = 0;
  try {
    for (let entry = names.readSync(); entry; entry = names.readSync())
      if (OBSERVATION_NAME.test(entry.name) && ++possible > MAX_OBSERVATIONS) break;
  } finally {
    names.closeSync();
  }
  if (possible <= MAX_OBSERVATIONS || !unchangedParent()) return;
  const retained: Array<{ file: string; stat: fs.Stats }> = [];
  const entries = fs.opendirSync(root);
  try {
    for (let entry = entries.readSync(); entry; entry = entries.readSync()) {
      if (!unchangedParent()) return;
      const name = OBSERVATION_NAME.exec(entry.name);
      if (!name) continue;
      const file = path.join(root, entry.name);
      const read = readPrivateCache(file, parent);
      if (!read || !observation(read.value, name[1])) continue;
      retained.push({ file, stat: read.stat });
      if (retained.length <= MAX_OBSERVATIONS) continue;
      retained.sort((a, b) => {
        if (a.file === current) return 1;
        if (b.file === current) return -1;
        return a.stat.mtimeMs - b.stat.mtimeMs || a.file.localeCompare(b.file);
      });
      const oldest = retained.shift();
      if (!oldest) return;
      // A concurrent writer, link replacement or directory swap must never
      // turn collection of one observation into deletion of another file.
      const stat = fs.lstatSync(oldest.file);
      if (
        regular(stat) &&
        sameFile(stat, oldest.stat) &&
        stat.size === oldest.stat.size &&
        stat.mtimeMs === oldest.stat.mtimeMs &&
        unchangedParent()
      )
        fs.unlinkSync(oldest.file);
    }
  } finally {
    entries.closeSync();
  }
}

export function writeUsageCache(file: string, value: unknown): void {
  const temporary = `${file}.${randomUUID()}.tmp`;
  const safeDestination = () => {
    try {
      return regular(fs.lstatSync(file));
    } catch (error) {
      return (error as NodeJS.ErrnoException).code === 'ENOENT';
    }
  };
  try {
    const payload = JSON.stringify(value);
    if (Buffer.byteLength(payload) > MAX_BYTES) return;
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    const before = fs.lstatSync(path.dirname(file));
    if (!directory(before) || !safeDestination()) return;
    fs.writeFileSync(temporary, payload, { mode: 0o600, flag: 'wx' });
    const after = fs.lstatSync(path.dirname(file));
    if (!directory(after) || !sameFile(before, after) || !safeDestination()) return;
    fs.renameSync(temporary, file);
    pruneObservations(file, before);
  } catch {
    // A read-only disk does not discard the verified in-memory observation.
  } finally {
    try {
      fs.unlinkSync(temporary);
    } catch {
      // Renamed or never created.
    }
  }
}
