import * as fs from 'fs';
import * as path from 'path';
import type { RawUsageEntry } from '../jsonl-parser';
import { resolveDroidConfigPaths } from '../services/compatible-cli-config-paths';
import { warn } from '../../utils/ui';
import { querySqliteJson } from './sqlite-cli';

export type DroidSqliteQuery = typeof querySqliteJson;

interface DroidNativeUsageCollectorOptions {
  env?: NodeJS.ProcessEnv;
  homeDir?: string;
  includeCcsManagedSessions?: boolean;
  querySqliteJson?: DroidSqliteQuery;
}

interface DroidSessionMetadata {
  model: string;
  projectPath: string;
  rawSelector: string;
  version?: string;
}

const DROID_COST_BATCH_SIZE = 1000;

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function asString(value: unknown): string | null {
  return typeof value === 'string' && value.trim().length > 0 ? value.trim() : null;
}

function asNumber(value: unknown): number | null {
  const numeric = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(numeric) && numeric >= 0 ? numeric : null;
}

function isCcsManagedSelector(selector: string): boolean {
  return selector.startsWith('custom:CCS-') || selector.startsWith('custom:ccs-');
}

function buildSelectorAlias(displayName: string, index: number): string {
  return `${displayName.trim().replace(/\s+/g, '-')}-${index}`;
}

function normalizeCustomModels(settings: Record<string, unknown>): Array<Record<string, unknown>> {
  const raw = settings.customModels ?? settings.custom_models;
  if (Array.isArray(raw)) {
    return raw.filter((entry): entry is Record<string, unknown> => isObject(entry));
  }
  if (isObject(raw)) {
    return Object.values(raw).filter((entry): entry is Record<string, unknown> => isObject(entry));
  }
  return [];
}

function buildCustomModelMap(settingsPath: string): Map<string, string> {
  const selectors = new Map<string, string>();
  if (!fs.existsSync(settingsPath)) return selectors;

  try {
    const parsed = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
    if (!isObject(parsed)) return selectors;

    for (const [index, entry] of normalizeCustomModels(parsed).entries()) {
      const displayName = asString(entry.displayName ?? entry.model_display_name);
      const model = asString(entry.model);
      if (!displayName || !model) continue;
      selectors.set(`custom:${buildSelectorAlias(displayName, index)}`, model);
    }
  } catch {
    return selectors;
  }

  return selectors;
}

function collectSessionFiles(dir: string, suffix: string, results: string[] = []): string[] {
  if (!fs.existsSync(dir)) return results;

  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const entryPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      collectSessionFiles(entryPath, suffix, results);
      continue;
    }
    if (entry.isFile() && entry.name.endsWith(suffix)) {
      results.push(entryPath);
    }
  }

  return results;
}

function readSessionStartMetadata(
  filePath: string
): { projectPath: string; version?: string } | null {
  try {
    const firstLine = fs.readFileSync(filePath, 'utf8').split('\n')[0];
    const parsed = JSON.parse(firstLine);
    if (parsed?.type !== 'session_start') return null;
    return {
      projectPath: asString(parsed.cwd) ?? '/',
      version:
        typeof parsed.version === 'number'
          ? String(parsed.version)
          : (asString(parsed.version) ?? undefined),
    };
  } catch {
    return null;
  }
}

function loadSessionMetadata(factoryDir: string): Map<string, DroidSessionMetadata> {
  const metadata = new Map<string, DroidSessionMetadata>();
  const selectorMap = buildCustomModelMap(path.join(factoryDir, 'settings.json'));
  const sessionsDir = path.join(factoryDir, 'sessions');

  for (const settingsPath of collectSessionFiles(sessionsDir, '.settings.json')) {
    const sessionId = path.basename(settingsPath, '.settings.json');
    const jsonlPath = settingsPath.replace(/\.settings\.json$/, '.jsonl');
    const start = readSessionStartMetadata(jsonlPath);
    if (!start) continue;

    try {
      const parsed = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
      const rawSelector = asString(parsed?.model) ?? 'unknown-droid-model';
      metadata.set(sessionId, {
        model: selectorMap.get(rawSelector) ?? rawSelector,
        projectPath: start.projectPath,
        rawSelector,
        version: start.version,
      });
    } catch {
      continue;
    }
  }

  return metadata;
}

export async function scanDroidNativeUsageEntries(
  options: DroidNativeUsageCollectorOptions = {}
): Promise<RawUsageEntry[]> {
  const query = options.querySqliteJson ?? querySqliteJson;
  const { settingsPath } = resolveDroidConfigPaths({
    env: options.env,
    homeDir: options.homeDir,
  });
  const factoryDir = path.dirname(settingsPath);
  const costsDbPath = path.join(factoryDir, 'costs.db');
  if (!fs.existsSync(costsDbPath)) return [];

  const rows: Array<Record<string, unknown>> = [];
  let offset = 0;
  while (true) {
    const batch = await query(
      costsDbPath,
      `SELECT session_id, timestamp, input_tokens, output_tokens FROM costs ` +
        `ORDER BY timestamp ASC LIMIT ${DROID_COST_BATCH_SIZE} OFFSET ${offset};`
    );
    if (!batch.length) break;
    rows.push(...batch);
    if (batch.length < DROID_COST_BATCH_SIZE) break;
    offset += batch.length;
  }
  if (!rows.length) return [];

  const metadata = loadSessionMetadata(factoryDir);
  const entries: RawUsageEntry[] = [];
  let skippedRowsWithoutMetadata = 0;

  for (const row of rows) {
    if (!isObject(row)) continue;
    const sessionId = asString(row.session_id);
    const timestamp = asString(row.timestamp);
    const inputTokens = asNumber(row.input_tokens);
    const outputTokens = asNumber(row.output_tokens);
    if (!sessionId || !timestamp || inputTokens === null || outputTokens === null) continue;

    const session = metadata.get(sessionId);
    if (!session) {
      skippedRowsWithoutMetadata++;
      continue;
    }

    if (!options.includeCcsManagedSessions && isCcsManagedSelector(session.rawSelector)) {
      continue;
    }

    entries.push({
      inputTokens,
      outputTokens,
      cacheCreationTokens: 0,
      cacheReadTokens: 0,
      model: session.model,
      sessionId,
      timestamp,
      projectPath: session.projectPath,
      version: session.version,
      target: 'droid',
    });
  }

  if (skippedRowsWithoutMetadata > 0) {
    console.log(
      warn(
        `Skipped ${skippedRowsWithoutMetadata} Droid native cost row(s) without local session metadata`
      )
    );
  }

  return entries;
}
