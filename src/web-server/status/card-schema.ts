/**
 * Decision cards (agent-reports/card@1): the one place a card file is validated and projected.
 * Fields beyond these are ignored; content blocks of an unknown type pass through as
 * `{ type, title, fold, unknown: true }` so the page can say it cannot show them.
 */
import {
  bool,
  CARD_SCHEMA,
  head,
  OPTION_KEY,
  type DocHead,
  fail,
  Invalid,
  isCardId,
  isPmMailName,
  iso,
  link,
  list,
  obj,
  oneOf,
  optIso,
  optObj,
  optSlug,
  optText,
  text,
  type ReportLink,
} from './schema-helpers';
import {
  section,
  media,
  MAX_CARD_SECTIONS,
  MAX_OPTION_SECTIONS,
  MAX_OPTION_MEDIA,
  type CardSection,
  type CardMedia,
} from './card-sections';

export type { CardSection, CardMedia } from './card-sections';

export const CARD_STATES = ['open', 'withdrawn', 'superseded'] as const;
/**
 * question: options, answered in the deck. objection: a call the agent already made; the owner may
 * object, silence is not an answer. action: something only the owner can do; a Done button.
 */
export const CARD_KINDS = ['question', 'objection', 'action'] as const;
export type CardKind = (typeof CARD_KINDS)[number];
export const URGENCIES = ['now', 'today', 'this-week', 'whenever'] as const;
export type Urgency = (typeof URGENCIES)[number];

export interface CardOption {
  key: string;
  label: string;
  /** What the owner sees or gets with this option. */
  gets: string | null;
  /** What it costs or changes elsewhere. */
  costs: string | null;
  media: CardMedia[];
  sections: CardSection[];
}

export interface CardRecommendation {
  key: string;
  why: string | null;
  confidence: 'low' | 'medium' | 'high' | null;
  /** What would change this recommendation. */
  flip: string | null;
}

export interface CardDoc extends DocHead {
  id: string;
  /** A short id the owner can quote ("F-Q3"); the local id when the file names none. */
  label: string;
  project: string | null;
  stream: string | null;
  askedBy: string | null;
  replyTo: { pmMail: string | null; t3Thread: string | null };
  state: (typeof CARD_STATES)[number];
  supersededBy: string | null;
  /** Why a withdrawn card was withdrawn ("Settled by your note"). */
  reason: string | null;
  title: string;
  /** The situation today in plain words (`context` is read as an alias). */
  now: string | null;
  nowAsOf: string | null;
  oldAnswer: { text: string; at: string | null } | null;
  whatChanged: string | null;
  options: CardOption[];
  recommendation: CardRecommendation | null;
  multiple: boolean;
  urgency: Urgency;
  /** What waits on the answer; null for "nothing waits on this". */
  blocks: string | null;
  /** The card's content sections, in order. */
  sections: CardSection[];
  batch: string | null;
  evidence: ReportLink[];
  /** The agent's replies to the owner's notes. */
  replies: Array<{ at: string; text: string }>;
  askedAt: string;
  kind: CardKind;
  /** When an answer is needed and what happens or starts costing without one. */
  neededBy: { at: string | null; ifNoAnswer: string | null } | null;
  /** The owner's rule this card loosens, and how. Shown before anything else. */
  exception: { rule: string; text: string } | null;
  reversible: { yes: boolean; how: string | null } | null;
}

export function card(doc: Record<string, unknown>, writer: string, fileId: string): CardDoc {
  const base = head(doc, CARD_SCHEMA, writer);
  if (doc.id !== fileId || !isCardId(doc.id) || !fileId.startsWith(`${writer}--`))
    throw new Invalid('id does not match its file name and writer');
  const replyTo =
    doc.replyTo === undefined || doc.replyTo === null ? {} : obj(doc.replyTo, 'replyTo');
  const pmMail = replyTo.pmMail === undefined || replyTo.pmMail === null ? null : replyTo.pmMail;
  if (pmMail !== null && !isPmMailName(pmMail)) fail('replyTo.pmMail');
  const options = list(doc.options, 'options', 4, (value, at): CardOption => {
    const option = obj(value, at);
    if (typeof option.key !== 'string' || !OPTION_KEY.test(option.key)) fail(`${at}.key`);
    return {
      key: option.key,
      label: text(option.label, `${at}.label`, 300),
      gets: optText(option.gets, `${at}.gets`, 1000),
      costs: optText(option.costs, `${at}.costs`, 1000),
      media: list(option.media, `${at}.media`, MAX_OPTION_MEDIA, media),
      sections: list(option.sections, `${at}.sections`, MAX_OPTION_SECTIONS, section),
    };
  });
  const kind = oneOf(doc.kind ?? 'question', 'kind', CARD_KINDS);
  if (kind === 'question' && options.length < 2) throw new Invalid('a card needs 2 to 4 options');
  if (kind === 'objection' && options.length === 1)
    throw new Invalid('an objection has no options or 2 to 4');
  if (kind === 'action' && options.length > 0) throw new Invalid('an action card has no options');
  if (new Set(options.map((option) => option.key)).size !== options.length)
    throw new Invalid('options repeat a key');
  let recommendation: CardDoc['recommendation'] = null;
  if (doc.recommendation !== null && doc.recommendation !== undefined) {
    const value = obj(doc.recommendation, 'recommendation');
    if (!options.some((option) => option.key === value.key)) fail('recommendation.key');
    recommendation = {
      key: value.key as string,
      why: optText(value.why, 'recommendation.why', 1000),
      confidence:
        value.confidence === null || value.confidence === undefined
          ? null
          : oneOf(value.confidence, 'recommendation.confidence', [
              'low',
              'medium',
              'high',
            ] as const),
      flip: optText(value.flip, 'recommendation.flip', 500),
    };
  }
  let oldAnswer: CardDoc['oldAnswer'] = null;
  if (doc.oldAnswer !== null && doc.oldAnswer !== undefined) {
    const value = obj(doc.oldAnswer, 'oldAnswer');
    oldAnswer = {
      text: text(value.text, 'oldAnswer.text', 2000),
      at: optIso(value.at, 'oldAnswer.at'),
    };
  }
  const state = oneOf(doc.state ?? 'open', 'state', CARD_STATES);
  const local = fileId.slice(fileId.indexOf('--') + 2);
  return {
    ...base,
    id: fileId,
    label: optText(doc.label, 'label', 32) ?? local,
    project: optSlug(doc.project, 'project'),
    stream: optSlug(doc.stream, 'stream'),
    askedBy: optText(doc.askedBy, 'askedBy', 64),
    replyTo: { pmMail, t3Thread: optText(replyTo.t3Thread, 'replyTo.t3Thread', 200) },
    state,
    supersededBy: state === 'superseded' ? optText(doc.supersededBy, 'supersededBy', 130) : null,
    reason: state === 'open' ? null : optText(doc.reason, 'reason', 500),
    title: text(doc.title, 'title', 300),
    now: optText(doc.now ?? doc.context, 'now', 4000),
    nowAsOf: optIso(doc.nowAsOf, 'nowAsOf'),
    oldAnswer,
    whatChanged: optText(doc.whatChanged, 'whatChanged', 2000),
    options,
    recommendation,
    multiple: bool(doc.multiple, 'multiple', false),
    urgency: oneOf(doc.urgency ?? 'whenever', 'urgency', URGENCIES),
    blocks: optText(doc.blocks, 'blocks', 300),
    sections: list(doc.sections, 'sections', MAX_CARD_SECTIONS, section),
    batch: optSlug(doc.batch, 'batch'),
    evidence: list(doc.evidence, 'evidence', 20, link),
    kind,
    neededBy: optObj(doc.neededBy, 'neededBy', (value) => ({
      at: optIso(value.at, 'neededBy.at'),
      ifNoAnswer: optText(value.ifNoAnswer, 'neededBy.ifNoAnswer', 500),
    })),
    exception: optObj(doc.exception, 'exception', (value) => ({
      rule: text(value.rule, 'exception.rule', 300),
      text: text(value.text, 'exception.text', 1000),
    })),
    reversible: optObj(doc.reversible, 'reversible', (value) => {
      if (typeof value.yes !== 'boolean') fail('reversible.yes');
      return { yes: value.yes, how: optText(value.how, 'reversible.how', 500) };
    }),
    replies: list(doc.replies, 'replies', 50, (value, at) => {
      const reply = obj(value, at);
      return { at: iso(reply.at, `${at}.at`), text: text(reply.text, `${at}.text`, 2000) };
    }),
    askedAt: iso(doc.askedAt ?? doc.writtenAt, 'askedAt'),
  };
}
