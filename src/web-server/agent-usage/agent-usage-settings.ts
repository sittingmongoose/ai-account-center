/**
 * The agent usage snapshot setting (`~/.ccs/agent-usage-snapshot.json`): the folder the dashboard
 * publishes into and, optionally, where each group of computers finds that folder. The locations
 * are the owner's own share paths, so they live in this private setting and in the shared folder
 * (`locations.json`), never in the package.
 */
import fs from 'fs';
import path from 'path';
import { ValidationError } from '../../errors/error-types';
import { getCcsDir } from '../../utils/config-manager';
import { readPrivateJsonFile, writePrivateJsonFile } from '../services/private-json-store';

export const SNAPSHOT_SETTINGS_FILE = 'agent-usage-snapshot.json';
export const SNAPSHOT_LOCATIONS = 'locations.json';
const MAX_DIRECTORY_LENGTH = 1024;
const MAX_LOCATIONS = 8;
const MAX_WHERE_LENGTH = 60;
/**
 * The read cap for the setting file. The longest valid setting (8 locations of 60 + 1024 three-byte
 * characters and a 1024-character folder) is about 30 KB, so every setting `set` accepts reads back.
 */
const MAX_SETTINGS_BYTES = 64 * 1024;

/**
 * Where one group of computers finds the snapshot folder, such as "the Mac" and
 * "/Volumes/share/aac-usage". The reader's rules block lists these so an agent on any computer can
 * find the folder.
 */
export interface SnapshotLocation {
  where: string;
  path: string;
}

export interface SnapshotSettings {
  version: 1;
  directory: string;
  locations?: SnapshotLocation[];
}

export function snapshotSettingsPath(ccsDir = getCcsDir()): string {
  return path.join(ccsDir, SNAPSHOT_SETTINGS_FILE);
}

/** An absolute folder path without control characters or `..` segments. */
export function isSnapshotDirectory(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length > 1 &&
    value.length <= MAX_DIRECTORY_LENGTH &&
    !/[\u0000-\u001f\u007f]/.test(value) &&
    path.isAbsolute(value) &&
    !value.split(/[\\/]+/).includes('..')
  );
}

const plainText = (value: unknown, max: number): value is string =>
  typeof value === 'string' &&
  value.trim() === value &&
  value.length > 0 &&
  value.length <= max &&
  !/[\u0000-\u001f\u007f]/.test(value);

/** A named place and a path without control characters; the path is another computer's, so any form. */
export function isSnapshotLocation(value: unknown): value is SnapshotLocation {
  const entry = value as Record<string, unknown> | null;
  return (
    typeof entry === 'object' &&
    entry !== null &&
    plainText(entry.where, MAX_WHERE_LENGTH) &&
    plainText(entry.path, MAX_DIRECTORY_LENGTH)
  );
}

export function isSnapshotLocationList(value: unknown): value is SnapshotLocation[] {
  return Array.isArray(value) && value.length <= MAX_LOCATIONS && value.every(isSnapshotLocation);
}

const copyLocations = (locations: readonly SnapshotLocation[]): SnapshotLocation[] =>
  locations.map(({ where, path: place }) => ({ where, path: place }));

export async function readSnapshotSettings(ccsDir = getCcsDir()): Promise<SnapshotSettings | null> {
  const read = await readPrivateJsonFile(snapshotSettingsPath(ccsDir), MAX_SETTINGS_BYTES);
  if (read.state !== 'ok') return null;
  const value = read.value as Record<string, unknown> | null;
  if (!value || value.version !== 1 || !isSnapshotDirectory(value.directory)) return null;
  // A damaged location list only loses the locations, never the folder.
  const locations = isSnapshotLocationList(value.locations) ? copyLocations(value.locations) : [];
  return { version: 1, directory: value.directory, ...(locations.length ? { locations } : {}) };
}

export async function writeSnapshotSettings(
  directory: string | null,
  ccsDir = getCcsDir(),
  locations: readonly SnapshotLocation[] = []
): Promise<void> {
  const file = snapshotSettingsPath(ccsDir);
  if (directory === null) {
    await fs.promises.rm(file, { force: true });
    return;
  }
  if (!isSnapshotDirectory(directory))
    throw new ValidationError('The usage snapshot folder must be an absolute path.');
  if (!isSnapshotLocationList(locations))
    throw new ValidationError(
      `Give at most ${MAX_LOCATIONS} locations, each a name and a path without control characters.`
    );
  const list = copyLocations(locations);
  const setting = { version: 1, directory, ...(list.length ? { locations: list } : {}) };
  // The same bytes writePrivateJsonFile writes: a setting that saves must also read back.
  if (Buffer.byteLength(`${JSON.stringify(setting, null, 2)}\n`) > MAX_SETTINGS_BYTES)
    throw new ValidationError('The usage snapshot setting is too large; use shorter locations.');
  await writePrivateJsonFile(file, setting);
}

/** `locations.json` in the snapshot folder: where each group of computers finds that folder. */
export function renderSnapshotLocations(locations: readonly SnapshotLocation[] = []): string {
  return `${JSON.stringify({ version: 1, locations: copyLocations(locations) }, null, 2)}\n`;
}
