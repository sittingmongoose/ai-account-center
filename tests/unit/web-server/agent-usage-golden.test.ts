/**
 * The agent usage snapshot's output files are a live contract other agents read every minute.
 * This pins their exact bytes: one fixed fixture renders usage.md, usage.json and history.json,
 * and the result must equal the golden files captured before the rendering code was extracted
 * from agent-usage-snapshot.ts into its own file. A wording change is a deliberate edit here
 * (regenerate the goldens); an accidental byte change anywhere fails.
 */
import { describe, expect, it } from 'bun:test';
import fs from 'fs';
import path from 'path';
import { buildSnapshotFiles } from '../../../src/web-server/agent-usage/agent-usage-snapshot';
import { renderSnapshotMarkdown } from '../../../src/web-server/agent-usage/agent-usage-snapshot';
import {
  GOLDEN_SIGN_IN_NOW,
  goldenEvent,
  goldenSignIn,
  goldenSignInRenewal,
} from './agent-usage-golden/fixture';

const goldenFolder = path.join(__dirname, 'agent-usage-golden');

/** The three file bodies exactly as the publisher writes them, from the fixed fixture. */
function renderedFiles(): { markdown: string; json: string; history: string } {
  const { snapshot, history } = buildSnapshotFiles(
    goldenEvent(),
    60,
    null,
    goldenSignIn(),
    goldenSignInRenewal()
  );
  return {
    markdown: renderSnapshotMarkdown(snapshot, GOLDEN_SIGN_IN_NOW),
    json: `${JSON.stringify(snapshot, null, 2)}\n`,
    history: `${JSON.stringify(history)}\n`,
  };
}

describe('agent usage snapshot golden bytes', () => {
  it('renders usage.md byte for byte', () => {
    const { markdown } = renderedFiles();
    expect(Buffer.compare(Buffer.from(markdown, 'utf8'), fs.readFileSync(path.join(goldenFolder, 'usage.md')))).toBe(0);
  });

  it('writes usage.json byte for byte', () => {
    const { json } = renderedFiles();
    expect(Buffer.compare(Buffer.from(json, 'utf8'), fs.readFileSync(path.join(goldenFolder, 'usage.json')))).toBe(0);
  });

  it('writes history.json byte for byte', () => {
    const { history } = renderedFiles();
    expect(
      Buffer.compare(Buffer.from(history, 'utf8'), fs.readFileSync(path.join(goldenFolder, 'history.json')))
    ).toBe(0);
  });

  it('depends on no clock: the fixture stamps stay fixed', () => {
    const { snapshot } = buildSnapshotFiles(
      goldenEvent(),
      60,
      null,
      goldenSignIn(),
      goldenSignInRenewal()
    );
    expect(snapshot.generatedAt).toBe('2026-10-15T12:00:00.000Z');
  });
});
