import { reportMedia } from './report-v2';
/**
 * Decision cards (agent-reports/card@1): the one place a card file is validated and projected.
 * Fields beyond these are ignored; content blocks of an unknown type pass through as
 * `{ type, title, fold, unknown: true }` so the page can say it cannot show them.
 */
import {
  bool,
  count,
  CARD_SCHEMA,
  head,
  OPTION_KEY,
  type DocHead,
  fail,
  Invalid,
  CARD_LOCAL_ID_MAX,
  idProblem,
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
 * judgement: a measurement card ("can't tell" is a valid option); one that carries a
 * recommendation is shown as a problem and cannot be answered, so the owner is never anchored.
 * sample: a format sample, shown with "no answer wanted" and never sent.
 */
export const CARD_KINDS = ['question', 'objection', 'action', 'judgement', 'sample'] as const;
export const OPTION_STANCES = ['keep', 'borrow', 'own'] as const;
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
  /** keep = the plan as written, borrow = take the outside approach, own = our own fix. */
  stance: (typeof OPTION_STANCES)[number] | null;
}

export interface CardRecommendation {
  key: string;
  why: string | null;
  confidence: 'low' | 'medium' | 'high' | null;
  /** What would change this recommendation. */
  flip: string | null;
}

export interface CardDoc extends DocHead {
  media?: Record<string, unknown>[];
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
  /** A badge such as adopt / adapt / keep ours / reject; the deck can filter on it. */
  verdict: string | null;
  /** A group within a batch, with its own "use the recommendations for this group". */
  group: string | null;
  /** Whether the judges agreed; the owner's time goes to split ones. */
  agreement: { same: boolean | null; note: string | null } | null;
  doubt: string | null;
  /** A rule card that settles a whole class. */
  appliesTo: { count: number | null; description: string } | null;
  changesCanon: Array<{ ref: string; what: string }>;
  /** Related card ids. */
  related: string[];
}

export function card(doc: Record<string, unknown>, writer: string, fileId: string): CardDoc {
  const base = head(doc, CARD_SCHEMA, writer);
  if (doc.id !== fileId) throw new Invalid('id does not match its file name and writer');
  // Which id rule it breaks, when one does: the writer prefix, the length or the grammar.
  const idRefusal = idProblem(doc.id, writer, CARD_LOCAL_ID_MAX);
  if (idRefusal || !isCardId(doc.id))
    throw new Invalid(idRefusal ?? 'id does not match its file name and writer');
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
      stance:
        option.stance === null || option.stance === undefined
          ? null
          : oneOf(option.stance, `${at}.stance`, OPTION_STANCES),
    };
  });
  const kind = oneOf(doc.kind ?? 'question', 'kind', CARD_KINDS);
  if ((kind === 'question' || kind === 'judgement') && options.length < 2)
    throw new Invalid('a card needs 2 to 4 options');
  if ((kind === 'objection' || kind === 'sample') && options.length === 1)
    throw new Invalid(
      `${kind === 'sample' ? 'a sample' : 'an objection'} has no options or 2 to 4`
    );
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
    ...(doc.media === undefined ? {} : { media: reportMedia(doc.media) }),
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
    verdict: optText(doc.verdict, 'verdict', 40),
    group: optText(doc.group, 'group', 80),
    agreement: optObj(doc.agreement, 'agreement', (value) => {
      if (value.same !== null && value.same !== undefined && typeof value.same !== 'boolean')
        fail('agreement.same');
      return {
        same: typeof value.same === 'boolean' ? value.same : null,
        note: optText(value.note, 'agreement.note', 500),
      };
    }),
    doubt: optText(doc.doubt, 'doubt', 1000),
    appliesTo: optObj(doc.appliesTo, 'appliesTo', (value) => ({
      count:
        value.count === null || value.count === undefined
          ? null
          : count(value.count, 'appliesTo.count'),
      description: text(value.description, 'appliesTo.description', 500),
    })),
    changesCanon: list(doc.changesCanon, 'changesCanon', 20, (value, at) => {
      const item = obj(value, at);
      return { ref: text(item.ref, `${at}.ref`, 200), what: text(item.what, `${at}.what`, 500) };
    }),
    related: list(doc.related, 'related', 20, (value, at) => text(value, at, 130)),
    replies: list(doc.replies, 'replies', 50, (value, at) => {
      const reply = obj(value, at);
      return { at: iso(reply.at, `${at}.at`), text: text(reply.text, `${at}.text`, 2000) };
    }),
    askedAt: iso(doc.askedAt ?? doc.writtenAt, 'askedAt'),
  };
}
