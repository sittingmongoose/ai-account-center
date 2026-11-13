/**
 * Rust parity (native/aac-usage, DESIGN 13 Stage 1): with AAC_USAGE_SCAN_BIN naming a built
 * `aac-usage-scan`, a helper test that runs the Python helper also runs the Rust scanner on the same
 * fixture home and request, and the two answers must agree on everything the server reads: per-kind
 * states and flags, fingerprints, hourly rows and session rows (order-free; costs to 1e-9). Only the
 * scanner's additions (discovery, reader, sessions, messages, msgThrough) are left out.
 */
import { expect } from 'bun:test';
import { execFileSync } from 'child_process';

export const SCAN_BIN = process.env.AAC_USAGE_SCAN_BIN;

const BY_VALUE = new Set(['codex', 'muse']);
const byValue = (row: Record<string, unknown>) =>
  BY_VALUE.has(String(row.k)) ? { ...row, f: 'by-value' } : row;

export function parityView(answer: Record<string, unknown>): unknown {
  const kinds = (answer.kinds ?? {}) as Record<string, Record<string, unknown>>;
  const rowKey = (row: Record<string, unknown>) => {
    const rounded: Record<string, unknown> = {
      ...row,
      c: Math.round(Number(row.c ?? 0) * 1e9) / 1e9,
    };
    return JSON.stringify(
      Object.fromEntries(
        Object.keys(rounded)
          .sort()
          .map((key) => [key, rounded[key]])
      )
    );
  };
  return {
    version: answer.version,
    truncated: answer.truncated,
    discoveryTruncated: answer.discoveryTruncated ?? false,
    kinds: Object.fromEntries(
      Object.entries(kinds).map(([kind, entry]) => [
        kind,
        {
          state: entry.state,
          partial: entry.partial ?? false,
          readFailures: entry.readFailures ?? 0,
          walUnread: entry.walUnread ?? false,
          unreadable: entry.unreadable ?? false,
          replacedFiles: [...((entry.replacedFiles as string[]) ?? [])].sort(),
          // The scanner keys Codex logs by conversation and Muse logs by session (one owner per
          // conversation across every folder, FIX-ROUND-BIG 1 and 2); Python keys them by path.
          // Those prints are compared by value.
          fingerprints: BY_VALUE.has(kind)
            ? Object.values((entry.fingerprints ?? {}) as Record<string, Record<string, unknown>>)
                .map((print) => {
                  const { msgThrough: _sent, ...rest } = print;
                  return rowKey(rest);
                })
                .sort()
            : Object.fromEntries(
                Object.entries(
                  (entry.fingerprints ?? {}) as Record<string, Record<string, unknown>>
                ).map(([key, print]) => {
                  const { msgThrough: _sent, ...rest } = print;
                  return [key, rest];
                })
              ),
        },
      ])
    ),
    rows: ((answer.rows ?? []) as Array<Record<string, unknown>>).map(byValue).map(rowKey).sort(),
    srows: ((answer.srows ?? []) as Array<Record<string, unknown>>).map(byValue).map(rowKey).sort(),
  };
}

/** Runs the Rust scanner on `request` with `env` over the process environment, and compares. */
export function rustParity(
  request: Record<string, unknown>,
  python: Record<string, unknown>,
  env: Record<string, string | undefined>
): void {
  if (!SCAN_BIN || request.zcodeRecords) return;
  const output = execFileSync(SCAN_BIN, [], {
    input: JSON.stringify(request),
    encoding: 'utf8',
    timeout: 30_000,
    maxBuffer: 8 * 1024 * 1024,
    env: { ...process.env, HOMEDRIVE: undefined, HOMEPATH: undefined, ...env },
  });
  expect(parityView(JSON.parse(output) as Record<string, unknown>)).toEqual(parityView(python));
}
