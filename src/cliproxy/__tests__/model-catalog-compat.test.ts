import { describe, it, expect } from 'bun:test';
import { findModel, supportsThinking } from '../model-catalog';

describe('model-catalog compatibility lookups', () => {
  it('finds agy Claude models using dotted major.minor IDs', () => {
    const dottedThinking = findModel('agy', 'claude-opus-4.6-thinking');
    const dottedNonThinking = findModel('agy', 'claude-sonnet-4.6');

    expect(dottedThinking?.id).toBe('claude-opus-4-6-thinking');
    expect(dottedNonThinking?.id).toBe('claude-sonnet-4-6');
  });

  it('maps legacy agy 4.5 IDs to canonical 4.6 catalog models', () => {
    const legacyOpusThinking = findModel('agy', 'claude-opus-4.5-thinking');
    const legacySonnetThinking = findModel('agy', 'claude-sonnet-4.5-thinking');
    const legacySonnet = findModel('agy', 'claude-sonnet-4.5');

    expect(legacyOpusThinking?.id).toBe('claude-opus-4-6-thinking');
    expect(legacySonnetThinking?.id).toBe('claude-sonnet-4-6');
    expect(legacySonnet?.id).toBe('claude-sonnet-4-6');
  });

  it('supports thinking checks for dotted agy model IDs', () => {
    expect(supportsThinking('agy', 'claude-opus-4.6-thinking')).toBe(true);
    expect(supportsThinking('agy', 'claude-sonnet-4.6')).toBe(true);
    expect(supportsThinking('agy', 'claude-opus-4.5-thinking')).toBe(true);
    expect(supportsThinking('agy', 'claude-sonnet-4.5')).toBe(true);
  });

  it('maps legacy sonnet 4.6 thinking aliases to canonical agy model', () => {
    const dottedLegacy = findModel('agy', 'claude-sonnet-4.6-thinking');
    const hyphenLegacy = findModel('agy', 'claude-sonnet-4-6-thinking');

    expect(dottedLegacy?.id).toBe('claude-sonnet-4-6');
    expect(hyphenLegacy?.id).toBe('claude-sonnet-4-6');
  });
});
