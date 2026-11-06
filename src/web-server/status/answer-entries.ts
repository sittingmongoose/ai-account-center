/**
 * One entry of an answer set (POST /api/status/answers): its strict shape, the refusals in fixed
 * words, and which record kind it makes on a given card.
 */
import type { SignedAnswer, UnsignedAnswer } from './answer-signing';
import { isCardId, type CardDoc } from './report-schema';

export const ANSWER_NOTE_MAX = 4000;

/** Why one entry of an answer set was not recorded (fixed sentences, never input). */
const REFUSALS = {
  card_changed: 'This card changed or is no longer open. Reload it and answer again.',
  invalid_entry: 'This entry is not a valid answer.',
  invalid_choice: 'The choice does not match this card’s options.',
  empty_entry: 'Choose an option or write a note.',
  invalid_note: `The note must be at most ${ANSWER_NOTE_MAX} characters of plain text.`,
  invalid_via: 'Only an unchanged recommendation can be sent as recommended.',
  invalid_kind: 'This kind of answer does not fit this card.',
  words_required: 'An objection needs your words in the note.',
  sample_card: 'This is a format sample; no answer is wanted.',
  judgement_anchored:
    'This judgement card carries a recommendation, so it cannot be answered until the agent removes it.',
  answer_not_saved: 'The answer could not be saved. Nothing was sent.',
  share_unavailable:
    'The reports share did not answer in time, so this card could not be checked. Nothing was recorded for it; try again.',
} as const;

export type RefusalCode = keyof typeof REFUSALS;

/** One entry's outcome, in the order the entries were sent. */
export interface EntryResult {
  cardId: string | null;
  outcome: 'recorded' | 'card_changed' | 'refused';
  code: RefusalCode | null;
  error: string | null;
}

export function refused(cardId: string | null, code: RefusalCode): EntryResult {
  return {
    cardId,
    outcome: code === 'card_changed' ? 'card_changed' : 'refused',
    code,
    error: REFUSALS[code],
  };
}

const CLIENT_SEND_ID = /^[A-Za-z0-9_-]{16,64}$/;
const ENTRY_ID = /^[0-9a-f]{32}$/;

/** The page's id for one draft: a retry of that draft carries it, a new draft never does. */
export function isEntryId(value: unknown): value is string {
  return typeof value === 'string' && ENTRY_ID.test(value);
}

/** Whether a body's `entryId`s are all valid and none repeats. Other shape checks come later. */
export function validEntryIds(entries: readonly unknown[]): boolean {
  const seen = new Set<string>();
  for (const entry of entries) {
    if (!entry || typeof entry !== 'object' || !('entryId' in entry)) continue;
    const id = (entry as { entryId: unknown }).entryId;
    if (!isEntryId(id) || seen.has(id)) return false;
    seen.add(id);
  }
  return true;
}

/**
 * The client's retry key for a send (the request's `sendId`): a content hash of the entries. It is
 * only ever a dedupe key, never the server's own batch id that names and signs the send summary.
 */
export function isClientSendId(value: unknown): value is string {
  return typeof value === 'string' && CLIENT_SEND_ID.test(value);
}

/** The device word for an answer, from the browser's User-Agent. */
export function answerDevice(userAgent: unknown): string {
  const ua = typeof userAgent === 'string' ? userAgent : '';
  if (/iPhone/.test(ua)) return 'iPhone';
  if (/iPad/.test(ua)) return 'iPad';
  if (/Android/.test(ua)) return 'Android';
  if (/Macintosh|Mac OS X/.test(ua)) return 'Mac';
  if (/Windows/.test(ua)) return 'Windows';
  if (/Linux|X11/.test(ua)) return 'Linux';
  return 'browser';
}

const NOTE_CONTROL = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/;
const LONE_SURROGATE = /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/;

/** The note as stored: trimmed of surrounding whitespace only; null when it is not acceptable. */
export function normalizeNote(note: unknown): string | null {
  if (note === undefined || note === null) return '';
  if (typeof note !== 'string') return null;
  const trimmed = note.trim();
  if (NOTE_CONTROL.test(trimmed) || LONE_SURROGATE.test(trimmed)) return null;
  if ([...trimmed].length > ANSWER_NOTE_MAX) return null;
  return trimmed;
}

/** Whether a choice (option keys, possibly none) fits this card. A note is always allowed. */
export function validChoice(card: CardDoc, choice: string[]): boolean {
  const keys = new Set(card.options.map((option) => option.key));
  if (new Set(choice).size !== choice.length) return false;
  if (!choice.every((key) => keys.has(key))) return false;
  return card.multiple || choice.length <= 1;
}

export type AnswerKind = UnsignedAnswer['kind'];
const ANSWER_KINDS: readonly AnswerKind[] = ['answer', 'note', 'object', 'done'];
/** Record kinds that close the card version they answer; a note or an objection leaves it open. */
export const CLOSING_KINDS: ReadonlySet<AnswerKind> = new Set(['answer', 'done']);

export interface AnswerEntry {
  cardId: string;
  cardSeq: number;
  cardSha256: string;
  choice: string[];
  note?: string;
  via: 'deck' | 'recommended';
  /** Needed for `object` and `done`; `answer`/`note` follow from the choice when absent. */
  kind?: AnswerKind;
  /** The draft's id; a repeat answers from the record it already made. */
  entryId?: string;
}

const ENTRY_KEYS = new Set([
  'cardId',
  'cardSeq',
  'cardSha256',
  'choice',
  'note',
  'via',
  'kind',
  'entryId',
]);
const SHA256 = /^[0-9a-f]{64}$/;

/** The strict shape of one entry of an answer set, or null. */
export function parseAnswerEntry(raw: unknown): AnswerEntry | null {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return null;
  const rec = raw as Record<string, unknown>;
  if (!Object.keys(rec).every((key) => ENTRY_KEYS.has(key))) return null;
  const { cardId, cardSeq, cardSha256, choice, note, via, kind, entryId } = rec;
  if (entryId !== undefined && !isEntryId(entryId)) return null;
  if (kind !== undefined && !ANSWER_KINDS.includes(kind as AnswerKind)) return null;
  if (!isCardId(cardId)) return null;
  if (typeof cardSeq !== 'number' || !Number.isSafeInteger(cardSeq) || cardSeq < 0) return null;
  if (typeof cardSha256 !== 'string' || !SHA256.test(cardSha256)) return null;
  if (
    !Array.isArray(choice) ||
    choice.length > 4 ||
    !choice.every((key) => typeof key === 'string' && key.length > 0 && key.length <= 32)
  )
    return null;
  if (note !== undefined && typeof note !== 'string') return null;
  if (via !== 'deck' && via !== 'recommended') return null;
  return {
    cardId,
    cardSeq,
    cardSha256,
    choice: choice as string[],
    ...(note !== undefined ? { note } : {}),
    via,
    ...(kind !== undefined ? { kind: kind as AnswerKind } : {}),
    ...(entryId !== undefined ? { entryId } : {}),
  };
}

/** The record kind an entry makes on this card, or the refusal. */
export function entryKind(
  card: CardDoc,
  entry: AnswerEntry,
  note: string
): AnswerKind | RefusalCode {
  const cardKind = card.kind ?? 'question';
  if (cardKind === 'sample') return 'sample_card';
  if (cardKind === 'judgement' && card.recommendation) return 'judgement_anchored';
  const kind = entry.kind ?? (entry.choice.length > 0 ? 'answer' : 'note');
  if (kind === 'answer')
    return (cardKind === 'question' || cardKind === 'judgement') && entry.choice.length > 0
      ? kind
      : 'invalid_kind';
  if (entry.choice.length > 0) return 'invalid_kind';
  if (kind === 'note') return note ? kind : 'empty_entry';
  if (kind === 'object')
    return cardKind !== 'objection' ? 'invalid_kind' : note ? kind : 'words_required';
  return cardKind === 'action' ? kind : 'invalid_kind';
}

/**
 * Where a record's signed copy goes: an answer or a done replaces `<card>.json`; a note or an
 * objection leaves the card open and may come again, so each gets its own file.
 */
export function shareFileName(answer: SignedAnswer, recordId: string): string {
  const hex = recordId.replace(/-/g, '').slice(0, 12);
  if (answer.kind === 'note') return `${answer.cardId}.note-${hex}.json`;
  if (answer.kind === 'object') return `${answer.cardId}.object-${hex}.json`;
  return `${answer.cardId}.json`;
}
