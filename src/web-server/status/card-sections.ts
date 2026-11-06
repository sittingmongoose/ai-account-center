/**
 * Card sections and media (AGENT-REPORTS-FORMAT.md, "Card content blocks"): a card carries only
 * the sections its question needs (card `sections[]` and option `sections[]`; `blocks` stays the
 * "what waits on the answer" string). Known section types are validated strictly; a section of a
 * type this server does not know passes through as `{ type, title, fold, unknown: true }`, so the
 * page can say it has a section it cannot show instead of the whole card being refused.
 */
import { bool, fail, isWriterFilePath, list, obj, oneOf, optText, text } from './schema-helpers';

/** A picture or a clip in the writer's own folder. */
export interface CardMedia {
  kind: 'image' | 'video';
  path: string;
  caption: string | null;
  label: 'built' | 'mockup' | null;
  /** A still shown before a video plays. */
  poster: string | null;
}

export interface BlockLink {
  label: string;
  url: string;
}

interface SectionBase {
  title: string | null;
  fold: boolean;
}

export type CardSection =
  | (SectionBase & { type: 'text'; body: string })
  | (SectionBase & { type: 'list'; items: string[]; ordered: boolean })
  | (SectionBase & { type: 'facts'; rows: Array<{ label: string; value: string }> })
  | (SectionBase & { type: 'table'; columns: string[]; rows: string[][] })
  | (SectionBase & {
      type: 'compare';
      items: Array<{
        name: string;
        does: string | null;
        problem: string | null;
        link: BlockLink | null;
        version: string | null;
      }>;
    })
  | (SectionBase & { type: 'notCopying'; items: Array<{ what: string; why: string }> })
  | (SectionBase & { type: 'unknowns'; items: string[] })
  | (SectionBase & { type: 'terms'; items: Array<{ term: string; meaning: string }> })
  | (SectionBase & { type: 'callout'; tone: 'info' | 'warn' | 'crit'; body: string })
  | (SectionBase & { type: 'links'; items: BlockLink[] })
  | (SectionBase & { type: 'media'; items: CardMedia[] })
  | (SectionBase & { type: string; unknown: true });

const BLOCK_TYPE = /^[A-Za-z][A-Za-z0-9-]{0,39}$/;
/** Section counts: on the card and on each option. */
export const MAX_CARD_SECTIONS = 30;
export const MAX_OPTION_SECTIONS = 10;
export const MAX_OPTION_MEDIA = 6;

function writerPath(value: unknown, field: string): string {
  if (!isWriterFilePath(value)) fail(field);
  return value;
}

export function media(value: unknown, field: string): CardMedia {
  const item = obj(value, field);
  return {
    kind: oneOf(item.kind, `${field}.kind`, ['image', 'video'] as const),
    path: writerPath(item.path, `${field}.path`),
    caption: optText(item.caption, `${field}.caption`, 300),
    label:
      item.label === null || item.label === undefined
        ? null
        : oneOf(item.label, `${field}.label`, ['built', 'mockup'] as const),
    poster:
      item.poster === null || item.poster === undefined
        ? null
        : writerPath(item.poster, `${field}.poster`),
  };
}

/** A web link (http or https) or a share path the page offers to copy; nothing else. */
function linkUrl(value: unknown, field: string): string {
  const url = text(value, field, 600);
  if (/^https?:\/\//i.test(url)) {
    try {
      const parsed = new URL(url);
      if ((parsed.protocol === 'http:' || parsed.protocol === 'https:') && !/\s/.test(url))
        return url;
    } catch {
      /* falls through */
    }
    fail(field);
  }
  if (url.startsWith('/') || url.startsWith('\\\\')) return url;
  return fail(field);
}

function blockLink(value: unknown, field: string): BlockLink {
  const item = obj(value, field);
  return { label: text(item.label, `${field}.label`, 120), url: linkUrl(item.url, `${field}.url`) };
}

/** A table cell may be empty. */
function cell(value: unknown, field: string): string {
  return value === '' ? '' : text(value, field, 300);
}

export function section(value: unknown, field: string): CardSection {
  const raw = obj(value, field);
  if (typeof raw.type !== 'string' || !BLOCK_TYPE.test(raw.type)) fail(`${field}.type`);
  const base: SectionBase = {
    title: optText(raw.title, `${field}.title`, 120),
    fold: bool(raw.fold, `${field}.fold`, false),
  };
  const items = <T>(max: number, each: (item: unknown, at: string) => T) =>
    list(raw.items, `${field}.items`, max, each);
  const line = (max: number) => (item: unknown, at: string) => text(item, at, max);
  switch (raw.type) {
    case 'text':
      return { ...base, type: 'text', body: text(raw.body, `${field}.body`, 4000) };
    case 'list':
      return {
        ...base,
        type: 'list',
        items: items(50, line(500)),
        ordered: bool(raw.ordered, `${field}.ordered`, false),
      };
    case 'facts':
      return {
        ...base,
        type: 'facts',
        rows: list(raw.rows, `${field}.rows`, 30, (row, at) => {
          const r = obj(row, at);
          return {
            label: text(r.label, `${at}.label`, 80),
            value: text(r.value, `${at}.value`, 300),
          };
        }),
      };
    case 'table': {
      const columns = list(raw.columns, `${field}.columns`, 8, line(80));
      if (columns.length === 0) fail(`${field}.columns`);
      const rows = list(raw.rows, `${field}.rows`, 30, (row, at) => {
        const cells = list(row, at, columns.length, cell);
        if (cells.length !== columns.length) fail(at);
        return cells;
      });
      return { ...base, type: 'table', columns, rows };
    }
    case 'compare':
      return {
        ...base,
        type: 'compare',
        items: items(12, (item, at) => {
          const r = obj(item, at);
          return {
            name: text(r.name, `${at}.name`, 120),
            does: optText(r.does, `${at}.does`, 1000),
            problem: optText(r.problem, `${at}.problem`, 1000),
            link: r.link === null || r.link === undefined ? null : blockLink(r.link, `${at}.link`),
            version: optText(r.version, `${at}.version`, 60),
          };
        }),
      };
    case 'notCopying':
      return {
        ...base,
        type: 'notCopying',
        items: items(20, (item, at) => {
          const r = obj(item, at);
          return { what: text(r.what, `${at}.what`, 300), why: text(r.why, `${at}.why`, 1000) };
        }),
      };
    case 'unknowns':
      return { ...base, type: 'unknowns', items: items(20, line(500)) };
    case 'terms':
      return {
        ...base,
        type: 'terms',
        items: items(20, (item, at) => {
          const r = obj(item, at);
          return {
            term: text(r.term, `${at}.term`, 80),
            meaning: text(r.meaning, `${at}.meaning`, 500),
          };
        }),
      };
    case 'callout':
      return {
        ...base,
        type: 'callout',
        tone: oneOf(raw.tone, `${field}.tone`, ['info', 'warn', 'crit'] as const),
        body: text(raw.body, `${field}.body`, 2000),
      };
    case 'links':
      return { ...base, type: 'links', items: items(20, blockLink) };
    case 'media':
      return { ...base, type: 'media', items: items(12, media) };
    default:
      // A type this server does not know: shown as "a section this page can't show yet".
      return { ...base, type: raw.type, unknown: true };
  }
}
